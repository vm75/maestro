//! Reference backup capture/recovery primitives. A captured bundle is not a
//! restorable backup. Incomplete or unsupported coverage fails closed.
use std::{collections::{BTreeMap,BTreeSet},fs::{self,File,Metadata},io::{Read,Write},os::unix::fs::{MetadataExt,PermissionsExt},os::fd::AsRawFd,path::{Path,PathBuf},time::{Duration,Instant}};
use anyhow::{Result,anyhow,ensure,Context};
use chrono::{DateTime,Utc};
use serde::{Deserialize,Serialize};
use sha2::{Digest,Sha256};
use rustix::fs::{self as unix_fs,Mode,OFlags,RenameFlags};
use super::{Config,Store,CompositeProvider,backup_inventory::{self,BackupInventory},store,journal};

const MAX_ENTRIES:usize=50000;
const MAX_MANIFEST:usize=64*1024*1024;
const METADATA_RESERVE:u64=72*1024*1024;
#[derive(Clone,Debug,Serialize,Deserialize,PartialEq,Eq)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub struct NativeObject{
    pub backend:String,pub name:String,
    #[serde(default,skip_serializing_if="String::is_empty")]pub project:String,
    #[serde(default,skip_serializing_if="String::is_empty")]pub pool:String,
    #[serde(default,skip_serializing_if="String::is_empty")]pub content_type:String,
}
#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub struct NativeExport{
    pub resources:Vec<String>,pub backend:String,pub kind:String,pub bundle_path:String,
    pub size_bytes:u64,pub sha256:String,pub native:NativeObject,
}
#[derive(Debug,Serialize,Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
struct ManifestFile{
    #[serde(rename="type")]kind:String,
    uid:u32,gid:u32,atime:String,mtime:String,mode:String,
    source_link_count:u64,source_allocated_bytes:u64,
    #[serde(default,skip_serializing_if="is_zero")]captured_link_count:u64,
    xattr_coverage:String,bundle_path:String,size_bytes:u64,sha256:String,
}
fn is_zero(v:&u64)->bool{*v==0}
#[derive(Serialize,Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
struct Manifest{
    source_root:String,principal:Principal,verification_required:Vec<String>,version:u8,stage:String,
    revision:String,created_at:DateTime<Utc>,server_man_version:String,files:Vec<ManifestFile>,
    native_exports:Vec<NativeExport>,running_workloads_at_backup:Vec<String>,resumed_workloads:Vec<String>,
    #[serde(default,skip_serializing_if="Vec::is_empty")]failed_resume:Vec<String>,
}
#[derive(Serialize,Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
struct Principal{uid:u32,gid:u32,groups:Vec<u32>}
fn principal()->Result<Principal>{
    let count=unsafe{libc::getgroups(0,std::ptr::null_mut())};
    ensure!((0..=1024).contains(&count),"workload principal groups cannot be established");
    let mut groups=vec![0;count as usize];
    ensure!(unsafe{libc::getgroups(count,groups.as_mut_ptr())}==count,"workload principal groups changed");
    Ok(Principal{uid:unsafe{libc::geteuid()},gid:unsafe{libc::getegid()},groups})
}
#[derive(Debug,Clone,Serialize,Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub struct Artifact{
    pub stage:String,pub staging_path:String,pub device:u64,pub inode:u64,
    pub parent_device:u64,pub parent_inode:u64,
    #[serde(default,skip_serializing_if="String::is_empty")]pub manifest_checksum:String,
}
#[derive(Debug,Clone,Serialize,Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub struct Recovery{
    pub version:u8,pub revision:String,pub destination:String,pub stage:String,
    pub updated_at:DateTime<Utc>,pub services:BTreeMap<String,String>,pub artifact:Artifact,
}
#[derive(Serialize)]
#[serde(rename_all="camelCase")]
pub struct RecoveryStatus{
    #[serde(skip_serializing_if="Option::is_none")]pub journal:Option<Recovery>,
    #[serde(skip_serializing_if="String::is_empty")]pub review_token:String,
    #[serde(skip_serializing_if="String::is_empty")]pub current_revision:String,
}
#[derive(Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub struct PrepareOptions{
    pub revision:String,pub destination:String,
    #[serde(default="yes")]pub quiesce:bool,
    #[serde(default)]pub deadline_seconds:u64,
}
fn yes()->bool{true}
pub fn deadline(seconds:u64)->Result<Duration>{
    let seconds=if seconds==0{900}else{seconds};
    ensure!((30..=86400).contains(&seconds),"deadlineSeconds must be between 30 and 86400");
    Ok(Duration::from_secs(seconds))
}
fn name(store:&Store)->String{format!("{}.backup.json",store.filename)}
fn clean_absolute(p:&str)->bool{
    p.len()<=4096&&super::path_valid(p,true)&&p!="/"&&!p.chars().any(char::is_control)
}
fn portable(p:&str)->bool{
    p.len()<=4096&&super::path_valid(p,false)&&p!="."&&!p.chars().any(char::is_control)
}
fn pending(j:&Recovery)->bool{
    !matches!(j.stage.as_str(),"complete"|"released")||!matches!(j.artifact.stage.as_str(),"published"|"discarded"|"released")
}
fn validate(j:&Recovery)->Result<()>{
    let a=&j.artifact;
    let stage=Path::new(&a.staging_path);let dest=Path::new(&j.destination);
    let nonce=stage.file_name().and_then(|v|v.to_str()).and_then(|v|v.strip_prefix(".staging-backup-")).unwrap_or("");
    ensure!(j.version==1&&store::valid_revision(&j.revision)&&clean_absolute(&j.destination)&&clean_absolute(&a.staging_path)
        &&stage.parent()==dest.parent()&&nonce.len()==32&&nonce.bytes().all(|b|b.is_ascii_digit()||(b'a'..=b'f').contains(&b))
        &&a.inode!=0&&a.parent_inode!=0&&j.services.len()<=128,"invalid backup recovery journal");
    ensure!(matches!(j.stage.as_str(),"capturing"|"recovering"|"complete"|"released")
        &&matches!(a.stage.as_str(),"building"|"synced"|"publishing"|"published"|"discarded"|"uncertain"|"released"),"invalid backup recovery stage");
    ensure!((a.manifest_checksum.is_empty()&&!matches!(a.stage.as_str(),"synced"|"publishing"|"published"))||store::valid_revision(&a.manifest_checksum),"invalid backup manifest checksum");
    for (id,state) in &j.services{
        ensure!(super::id_valid(id)&&matches!(state.as_str(),"untouched"|"stop_attempted"|"stop_uncertain"|"stopped"|"resumed"|"failed")
            &&(j.stage!="complete"||matches!(state.as_str(),"untouched"|"resumed")),"invalid backup recovery service state");
    }Ok(())
}
fn read_locked(store:&Store,dir:&File)->Result<Option<Recovery>>{
    match store::read_private(dir,&name(store),super::MAX_CONFIG_BYTES){
        Ok(raw)=>{let j=serde_json::from_slice(&raw)?;validate(&j)?;Ok(Some(j))},
        Err(e) if e.downcast_ref::<rustix::io::Errno>()==Some(&rustix::io::Errno::NOENT)=>Ok(None),
        Err(e)=>Err(e),
    }
}
fn persist(store:&Store,dir:&File,j:&mut Recovery)->Result<()>{
    j.updated_at=Utc::now();validate(j)?;
    store::publish(dir,&name(store),&serde_json::to_vec(j)?,false)
}
pub(super) fn ensure_idle_locked(store:&Store,dir:&File)->Result<()>{
    ensure!(!read_locked(store,dir)?.as_ref().is_some_and(pending),"backup recovery is pending; review recovery actions in Configuration");
    Ok(())
}
pub fn ensure_idle(store:&Store)->Result<()>{
    match store.directory(){
        Ok(dir)=>ensure_idle_locked(store,&dir),
        Err(e) if e.downcast_ref::<rustix::io::Errno>()==Some(&rustix::io::Errno::NOENT)=>Ok(()),
        Err(e)=>Err(e),
    }
}
pub fn status(store:&Store)->Result<RecoveryStatus>{
    let dir=match store.directory(){
        Ok(d)=>d,Err(e) if e.downcast_ref::<rustix::io::Errno>()==Some(&rustix::io::Errno::NOENT)=>return Ok(RecoveryStatus{journal:None,review_token:String::new(),current_revision:String::new()}),
        Err(e)=>return Err(e),
    };
    let j=read_locked(store,&dir)?;
    let token=match &j{Some(j)=>super::revision(&serde_json::to_vec(j)?),None=>String::new()};
    let current_revision=store::current(&dir,&store.filename)?.map_or(super::MISSING_REVISION.into(),|s|s.revision);
    Ok(RecoveryStatus{journal:j,review_token:token,current_revision})
}
fn reviewed(store:&Store,dir:&File,revision:&str,token:&str)->Result<Recovery>{
    let j=read_locked(store,dir)?.context("backup recovery is unavailable")?;
    ensure!(super::revision(&serde_json::to_vec(&j)?)==token,"backup recovery review changed");
    ensure!(store::current(dir,&store.filename)?.map_or(super::MISSING_REVISION.into(),|s|s.revision)==revision,"configuration revision changed");
    Ok(j)
}
pub fn release(store:&Store,revision:&str,token:&str)->Result<RecoveryStatus>{
    let dir=store.lock_directory()?;let mut j=reviewed(store,&dir,revision,token)?;
    if pending(&j){j.stage="released".into();if !matches!(j.artifact.stage.as_str(),"published"|"discarded"){j.artifact.stage="released".into();}persist(store,&dir,&mut j)?;}
    status(store)
}

pub(super) fn fd_path(file:&File)->PathBuf{PathBuf::from(format!("/proc/self/fd/{}",file.as_raw_fd()))}
fn open_at(parent:&File,name:&str,directory:bool)->Result<File>{
    ensure!(!name.is_empty()&&!name.contains('/')&&name!="."&&name!="..","invalid capture component");
    let flags=OFlags::RDONLY|OFlags::NOFOLLOW|OFlags::CLOEXEC|OFlags::NONBLOCK|if directory{OFlags::DIRECTORY}else{OFlags::empty()};
    Ok(unix_fs::openat(parent,name,flags,Mode::empty())?.into())
}
pub(super) fn open_absolute(path:&Path,directory:bool)->Result<File>{
    let text=path.to_str().context("invalid capture path")?;
    ensure!(clean_absolute(text),"invalid capture path");
    let mut at=File::open("/")?;
    let parts:Vec<_>=text.trim_start_matches('/').split('/').collect();
    for (index,part) in parts.iter().enumerate(){at=open_at(&at,part,index+1<parts.len()||directory)?;}
    Ok(at)
}
fn same(a:&Metadata,b:&Metadata)->bool{
    a.dev()==b.dev()&&a.ino()==b.ino()&&a.len()==b.len()&&a.mode()==b.mode()
        &&a.uid()==b.uid()&&a.gid()==b.gid()&&a.nlink()==b.nlink()
        &&a.mtime()==b.mtime()&&a.mtime_nsec()==b.mtime_nsec()&&a.ctime()==b.ctime()&&a.ctime_nsec()==b.ctime_nsec()
}
fn mount(file:&File)->Result<String>{
    let info=fs::read_to_string(format!("/proc/self/fdinfo/{}",file.as_raw_fd()))?;
    Ok(info.lines().find_map(|line|line.strip_prefix("mnt_id:")).context("cannot determine capture mount identity")?.trim().to_owned())
}
fn private(meta:&Metadata,directory:bool)->bool{
    meta.uid()==unsafe{libc::geteuid()}&&meta.mode()&0o077==0&&(if directory{meta.is_dir()}else{meta.is_file()&&meta.nlink()==1})
}
fn check_time(end:Instant)->Result<()>{ensure!(Instant::now()<end,"backup deadline exceeded");Ok(())}
pub(super) fn hash_file(mut file:&File,end:Instant)->Result<(u64,String)>{
    let before=file.metadata()?;ensure!(before.is_file(),"backup payload is not regular");
    let mut h=Sha256::new();let mut count=0u64;let mut buffer=vec![0;128*1024];
    loop{check_time(end)?;let n=file.read(&mut buffer)?;if n==0{break}h.update(&buffer[..n]);count=count.checked_add(n as u64).context("payload size overflow")?;}
    ensure!(count==before.len()&&same(&before,&file.metadata()?),"backup file changed while hashing");
    Ok((count,format!("{:x}",h.finalize())))
}
fn metadata(meta:&Metadata,kind:&str,path:&str)->Result<ManifestFile>{
    let ts=|sec,nsec|DateTime::<Utc>::from_timestamp(sec,nsec as u32).map(|t|t.to_rfc3339()).context("invalid file timestamp");
    Ok(ManifestFile{kind:kind.into(),uid:meta.uid(),gid:meta.gid(),atime:ts(meta.atime(),meta.atime_nsec())?,
        mtime:ts(meta.mtime(),meta.mtime_nsec())?,mode:format!("{:04o}",meta.mode()&0o7777),source_link_count:meta.nlink(),
        source_allocated_bytes:meta.blocks().saturating_mul(512),captured_link_count:if kind=="file"{1}else{0},
        xattr_coverage:"visible".into(),bundle_path:path.into(),size_bytes:0,sha256:String::new()})
}
fn reject_xattrs(file:&File)->Result<()>{
    // This first filesystem collector cannot yet preserve ACLs or arbitrary
    // xattrs. Refuse rather than silently lose visible metadata.
    let n=unsafe{libc::flistxattr(file.as_raw_fd(),std::ptr::null_mut(),0)};
    ensure!(n==0,"source xattrs/ACLs are present or cannot be read; full metadata capture is pending");Ok(())
}
fn mkdir_at(parent:&File,name:&str)->Result<File>{
    match unix_fs::mkdirat(parent,name,Mode::from_bits_truncate(0o700)){
        Ok(())=>parent.sync_all()?,Err(rustix::io::Errno::EXIST)=>{},Err(e)=>return Err(e.into()),
    }
    let file=open_at(parent,name,true)?;ensure!(private(&file.metadata()?,true),"capture directory is not private");Ok(file)
}
fn target_parent(root:&File,path:&str)->Result<(File,String)>{
    ensure!(portable(path),"invalid backup bundle path");
    let mut parts=path.split('/').collect::<Vec<_>>();let leaf=parts.pop().unwrap().to_owned();
    let mut parent=root.try_clone()?;for part in parts{parent=mkdir_at(&parent,part)?;}Ok((parent,leaf))
}
pub(super) fn create_payload(root:&File,path:&str)->Result<File>{
    let (dir,leaf)=target_parent(root,path)?;
    Ok(unix_fs::openat(&dir,leaf.as_str(),OFlags::WRONLY|OFlags::CREATE|OFlags::EXCL|OFlags::NOFOLLOW|OFlags::CLOEXEC,Mode::from_bits_truncate(0o600))?.into())
}
fn read_payload(root:&File,path:&str,directory:bool)->Result<File>{
    ensure!(portable(path),"invalid backup path");
    let mut at=root.try_clone()?;let parts=path.split('/').collect::<Vec<_>>();
    for (i,part) in parts.iter().enumerate(){at=open_at(&at,part,i+1<parts.len()||directory)?;}
    Ok(at)
}
pub(super) fn native_export(root:&File,path:&str,kind:&str,resources:Vec<String>,native:NativeObject,end:Instant)->Result<NativeExport>{
    let file=read_payload(root,path,false)?;
    ensure!(private(&file.metadata()?,false),"native export is not private");
    let (size_bytes,sha256)=hash_file(&file,end)?;
    ensure!(size_bytes>0,"native export is empty");
    Ok(NativeExport{resources,backend:native.backend.clone(),kind:kind.into(),bundle_path:path.into(),size_bytes,sha256,native})
}
struct Collector<'a>{root:&'a File,files:Vec<ManifestFile>,paths:BTreeSet<String>,end:Instant}
impl Collector<'_>{
    fn capture(&mut self,source:&File,path:&str,depth:usize,source_mount:&str)->Result<()>{
        check_time(self.end)?;
        ensure!(depth<=128&&self.files.len()<MAX_ENTRIES&&portable(path)&&self.paths.insert(path.into()),"backup tree exceeds limit or contains duplicate path");
        let before=source.metadata()?;
        ensure!(mount(source)?==source_mount,"capture crosses an unreviewed mount");
        reject_xattrs(source)?;
        if before.is_dir(){
            let (parent,leaf)=target_parent(self.root,path)?;
            let dst=mkdir_at(&parent,&leaf)?;
            self.files.push(metadata(&before,"directory",path)?);
            for entry in fs::read_dir(fd_path(source))?{
                check_time(self.end)?;let entry=entry?;
                let name=entry.file_name().into_string().map_err(|_|anyhow!("non-UTF-8 capture path"))?;
                let child=open_at(source,&name,false)?;
                self.capture(&child,&format!("{path}/{name}"),depth+1,source_mount)?;
            }
            dst.sync_all()?;
        }else{
            ensure!(before.is_file()&&before.nlink()==1,"symlink, hardlink or special input requires full filesystem capture support");
            let mut row=metadata(&before,"file",path)?;
            let mut destination=create_payload(self.root,path)?;
            let mut src=source;let mut h=Sha256::new();let mut count=0u64;let mut buffer=vec![0;128*1024];
            loop{
                check_time(self.end)?;let n=src.read(&mut buffer)?;if n==0{break}
                h.update(&buffer[..n]);count=count.checked_add(n as u64).context("capture size overflow")?;
                ensure!(count<=before.len(),"capture source grew during copy");destination.write_all(&buffer[..n])?;
            }
            destination.sync_all()?;
            ensure!(count==before.len(),"capture source changed during copy");
            row.size_bytes=count;row.sha256=format!("{:x}",h.finalize());self.files.push(row);
        }
        ensure!(same(&before,&source.metadata()?),"source changed during capture");
        Ok(())
    }
}
struct Publication{parent:File,root:File,artifact:Artifact,destination:String}
impl Publication{
    fn new(destination:&str,source_root:&Path)->Result<Self>{
        ensure!(clean_absolute(destination),"destination must be an absolute cleaned path");
        let path=Path::new(destination);let parent_path=path.parent().context("missing destination parent")?;
        ensure!(!path.starts_with(source_root)&&!["/proc","/sys","/dev"].iter().any(|p|path.starts_with(p)),"destination cannot be inside configuration or system directories");
        let parent=open_absolute(parent_path,true)?;
        ensure!(parent.metadata()?.uid()==unsafe{libc::geteuid()}&&parent.metadata()?.mode()&0o022==0,"capture parent must be owned and not writable by other users");
        let leaf=path.file_name().and_then(|n|n.to_str()).context("invalid destination")?;
        match unix_fs::statat(&parent,leaf,unix_fs::AtFlags::SYMLINK_NOFOLLOW){
            Err(rustix::io::Errno::NOENT)=>{},_=>return Err(anyhow!("destination must be absent; existing directories are not replaced")),
        }
        let stage=format!(".staging-backup-{}",uuid::Uuid::new_v4().simple());
        unix_fs::mkdirat(&parent,stage.as_str(),Mode::from_bits_truncate(0o700))?;
        parent.sync_all()?;
        let root=open_at(&parent,&stage,true)?;
        let meta=root.metadata()?;let p=parent.metadata()?;
        Ok(Self{parent,root,artifact:Artifact{stage:"building".into(),staging_path:parent_path.join(stage).to_string_lossy().into_owned(),device:meta.dev(),inode:meta.ino(),parent_device:p.dev(),parent_inode:p.ino(),manifest_checksum:String::new()},destination:destination.into()})
    }
    fn check(&self)->Result<()>{
        let parent=open_absolute(Path::new(&self.destination).parent().unwrap(),true)?;
        let p=parent.metadata()?;
        ensure!((p.dev(),p.ino())==(self.artifact.parent_device,self.artifact.parent_inode),"capture parent changed");
        let stage=Path::new(&self.artifact.staging_path).file_name().unwrap().to_str().unwrap();
        let opened=open_at(&self.parent,stage,true)?;let m=opened.metadata()?;
        ensure!(private(&m,true)&&(m.dev(),m.ino())==(self.artifact.device,self.artifact.inode),"capture staging identity changed");
        Ok(())
    }
    fn rename(&self)->Result<()>{
        self.check()?;
        let stage=Path::new(&self.artifact.staging_path).file_name().unwrap();
        let dest=Path::new(&self.destination).file_name().unwrap();
        unix_fs::renameat_with(&self.parent,stage,&self.parent,dest,RenameFlags::NOREPLACE)?;
        self.parent.sync_all()?;
        let opened=open_absolute(Path::new(&self.destination),true)?;let m=opened.metadata()?;
        ensure!((m.dev(),m.ino())==(self.artifact.device,self.artifact.inode),"published capture identity changed");Ok(())
    }
}
