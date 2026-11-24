//! Reference backup capture/recovery primitives. A captured bundle is not a
//! restorable backup. Incomplete or unsupported coverage fails closed.
use std::{collections::{BTreeMap,BTreeSet},fs::{self,File,Metadata},io::{Read,Write,Seek,SeekFrom},os::unix::fs::{MetadataExt},os::fd::AsRawFd,path::{Path,PathBuf},time::{Duration,Instant}};
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
    let dir=store::LockedDirectory::try_acquire(store.directory()?)?;let mut j=reviewed(store,&dir,revision,token)?;
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
                ensure!(count<=before.len(),"capture source grew during copy");if buffer[..n].iter().all(|v|*v==0){destination.seek(SeekFrom::Current(n as i64))?;}else{destination.write_all(&buffer[..n])?;}
            }
            destination.set_len(count)?;
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

pub(super) fn read_export(root:&File,path:&str)->Result<File>{read_payload(root,path,false)}

/// Enrich the existing metadata-only inventory using owned native identities.
/// Errors remain explicit blockers; private XML/config is never returned.
pub async fn inventory(store:&Store,storage_root:&Path,provider:&CompositeProvider)->Result<BackupInventory>{
    let path=store.path();let root=path.parent().context("missing authority directory")?;
    let mut inv=backup_inventory::inventory(store,root,storage_root)?;
    let snapshot=store.read()?.context("configuration unavailable")?;
    ensure!(snapshot.revision==inv.revision,"configuration changed during inventory");
    let config=snapshot.config()?;
    for input in inv.inputs.iter_mut().filter(|i|i.strategy=="native_export"){
        let Some(id)=input.resources.first().and_then(|key|key.strip_prefix("service/"))else{continue};
        let result=match config.services.get(id).map(|s|s.backend.as_str()){
            Some("libvirt")=>provider.libvirt.backup_dependency(&config,id).await,
            Some("incus")=>provider.incus.backup_dependency(&config,id).await,
            _=>Err(anyhow!("native export is unsupported")),
        };
        if let Ok(object)=result{
            input.native=Some(object);input.inspection="native_dependencies_discovered";
            inv.issues.retain(|issue|issue.resource!=format!("service/{id}")||issue.code!="backup_native_dependencies");
        }
    }
    ensure!(store.read()?.is_some_and(|s|s.revision==inv.revision),"configuration changed during inventory");
    Ok(inv)
}
async fn service_state(provider:&CompositeProvider,c:&Config,id:&str)->Result<&'static str>{
    let service=c.services.get(id).context("unknown backup workload")?;
    let state=match service.backend.as_str(){
        "libvirt"=>provider.libvirt.status_details(c,id).await?.state,
        "incus"=>provider.incus.status_details(c,id).await?.state,
        "compose"=>{
            let status=if service.engine=="docker"{provider.docker_compose.status_details(c,id).await?}
                else{provider.podman_compose.status_details(c,id).await?};
            if status.state=="stopped"{
                ensure!(status.members.iter().all(|m|matches!(m.state.as_str(),"missing"|"exited"|"stopped"|"created"|"configured"|"dead")),
                    "Compose members are not all stopped");
            }
            status.state
        },
        "quadlet"=>provider.quadlet.as_ref().context("Quadlet provider unavailable")?.status_details(c,id).await?.state,
        _=>return Err(anyhow!("unsupported backup workload provider")),
    };
    ensure!(matches!(state,"running"|"stopped"),"workload state is not stable and fully running or stopped");
    Ok(state)
}
fn check_revision(store:&Store,dir:&File,revision:&str)->Result<()>{
    ensure!(store::current(dir,&store.filename)?.is_some_and(|s|s.revision==revision),"configuration revision changed during backup");Ok(())
}
async fn resume(store:&Store,dir:&File,storage_root:&Path,provider:&CompositeProvider,c:&Config,j:&mut Recovery)->Result<(Vec<String>,Vec<String>)>{
    j.stage="recovering".into();persist(store,dir,j)?;
    let end=Instant::now()+Duration::from_secs(120);
    let mut resumed=Vec::new();let mut failed=Vec::new();
    for id in c.service_order()?{
        let Some(prior)=j.services.get(id).cloned()else{continue};
        if matches!(prior.as_str(),"untouched"|"resumed"){continue}
        check_revision(store,dir,&j.revision)?;
        let mut settled=!matches!(prior.as_str(),"stop_attempted"|"stop_uncertain");
        let result=tokio::time::timeout(end.saturating_duration_since(Instant::now()),async{
            match service_state(provider,c,id).await?{
                "running"=>{ensure!(settled,"shutdown may still be in progress");Ok(())},
                "stopped"=>{
                    settled=true;j.services.insert(id.into(),"stopped".into());persist(store,dir,j)?;
                    for dep in &c.services[id].depends_on{
                        ensure!(service_state(provider,c,dep).await?=="running","backup dependency is not running");
                    }
                    let mut health=super::preflight::check_files(store,c,storage_root,Some(Path::new("/proc")));
                    super::preflight::propagate(c,&mut health,vec![]);
                    ensure!(!health.issues.iter().any(|issue|issue.resource==format!("service/{id}")&&issue.severity!="warning"),
                        "required service files need attention before workload recovery");
                    if c.services[id].backend=="compose"{
                        let adapter=if c.services[id].engine=="docker"{&provider.docker_compose}else{&provider.podman_compose};
                        adapter.resume_backup_workload(c,id).await?;
                    }else{provider.action(c,id,"start").await?;}
                    ensure!(service_state(provider,c,id).await?=="running","workload did not resume");Ok(())
                },
                _=>Err(anyhow!("unknown workload state")),
            }
        }).await;
        if matches!(result,Ok(Ok(()))){j.services.insert(id.into(),"resumed".into());resumed.push(id.into());}
        else{j.services.insert(id.into(),if settled{"failed"}else{"stop_uncertain"}.into());failed.push(id.into());}
        persist(store,dir,j)?;
    }
    if failed.is_empty(){j.stage="complete".into();persist(store,dir,j)?;}
    Ok((resumed,failed))
}
pub async fn recover(store:&Store,storage_root:&Path,provider:&CompositeProvider,revision:&str,token:&str)->Result<RecoveryStatus>{
    let dir=store::LockedDirectory::try_acquire(store.directory()?)?;
    let mut j=reviewed(store,&dir,revision,token)?;
    ensure!(j.revision==revision,"backup recovery requires the original configuration revision");
    if !pending(&j){return status(store)}
    journal::ensure_idle(&dir)?;
    let config=store::current(&dir,&store.filename)?.context("configuration unavailable")?.config()?;
    ensure!(j.services.keys().all(|id|config.services.contains_key(id)),"backup recovery references an unknown workload");
    let (_,failed)=resume(store,&dir,storage_root,provider,&config,&mut j).await?;
    ensure!(failed.is_empty(),"previously running workloads need explicit recovery");
    status(store)
}
async fn native_boundary(provider:&CompositeProvider,c:&Config,inv:&BackupInventory)->Result<()>{
    for id in c.services.keys(){ensure!(service_state(provider,c,id).await?=="stopped","all workloads must be verified stopped for capture");}
    for input in inv.inputs.iter().filter(|i|i.strategy=="native_export"){
        let object=input.native.as_ref().context("unresolved native export")?;
        let id=input.resources.first().and_then(|s|s.strip_prefix("service/")).context("invalid native resource")?;
        match object.backend.as_str(){
            "libvirt"=>{provider.libvirt.validate_backup(c,id,object,true).await?;},
            "incus"=>provider.incus.validate_backup(c,id,object,true).await?,
            _=>return Err(anyhow!("unsupported native exporter")),
        }
    }Ok(())
}
fn expected_paths(m:&Manifest)->Result<BTreeMap<String,(String,u64,String)>>{
    ensure!(m.version==1&&m.stage=="captured"&&store::valid_revision(&m.revision)&&clean_absolute(&m.source_root)
        &&!m.files.is_empty()&&m.files.len()<=MAX_ENTRIES&&m.native_exports.len()<=2048,"invalid capture manifest");
    let mut expected=BTreeMap::new();
    for f in &m.files{
        ensure!(portable(&f.bundle_path)&&f.bundle_path!="manifest.json"&&!f.bundle_path.starts_with("native/")&&f.bundle_path!="native","invalid capture path");
        ensure!(matches!(f.kind.as_str(),"file"|"directory")&&f.mode.len()==4&&u32::from_str_radix(&f.mode,8).is_ok_and(|v|v<=0o7777)
            &&DateTime::parse_from_rfc3339(&f.atime).is_ok()&&DateTime::parse_from_rfc3339(&f.mtime).is_ok(),"invalid captured metadata");
        ensure!(if f.kind=="file"{store::valid_revision(&f.sha256)&&f.source_link_count==1&&f.captured_link_count==1}
            else{f.sha256.is_empty()&&f.size_bytes==0&&f.captured_link_count==0},"invalid manifest payload");
        ensure!(expected.insert(f.bundle_path.clone(),(f.kind.clone(),f.size_bytes,f.sha256.clone())).is_none(),"duplicate manifest payload");
    }
    ensure!(expected.get("server-man.yaml").is_some_and(|(k,_,hash)|k=="file"&&hash==&m.revision),"capture has no pinned configuration");
    for export in &m.native_exports{
        ensure!(portable(&export.bundle_path)&&export.bundle_path.starts_with("native/")&&export.size_bytes>0
            &&store::valid_revision(&export.sha256)&&export.backend==export.native.backend
            &&matches!((export.backend.as_str(),export.kind.as_str()),("incus","incus_instance_export")|("libvirt","libvirt_guest_state"))
            &&export.resources.len()==1,"invalid native export");
        ensure!(expected.insert(export.bundle_path.clone(),("file".into(),export.size_bytes,export.sha256.clone())).is_none(),"duplicate native export");
    }
    let keys:Vec<_>=expected.keys().cloned().collect();
    for key in keys{
        let mut at=Path::new(&key).parent();
        while let Some(p)=at.filter(|p|!p.as_os_str().is_empty()){
            let pstr=p.to_str().context("invalid manifest ancestor")?;
            ensure!(expected.entry(pstr.into()).or_insert_with(||("directory".into(),0,String::new())).0=="directory","non-directory payload ancestor");
            at=p.parent();
        }
    }
    ensure!(expected.len()<=MAX_ENTRIES+8192,"manifest path count exceeds limit");
    Ok(expected)
}
fn read_small(root:&File,path:&str,limit:usize)->Result<Vec<u8>>{
    let file=read_payload(root,path,false)?;
    ensure!(private(&file.metadata()?,false),"private capture metadata permissions differ");
    let mut raw=Vec::new();file.take(limit as u64+1).read_to_end(&mut raw)?;
    ensure!(raw.len()<=limit,"capture metadata exceeds limit");Ok(raw)
}
fn verify_tree(root:&File,expected:&BTreeMap<String,(String,u64,String)>,end:Instant)->Result<()>{
    fn walk(dir:&File,prefix:&str,expected:&BTreeMap<String,(String,u64,String)>,seen:&mut BTreeSet<String>,mount_id:&str,end:Instant,depth:usize)->Result<()>{
        check_time(end)?;ensure!(depth<=132,"capture verification depth exceeded");
        let before=dir.metadata()?;
        for entry in fs::read_dir(fd_path(dir))?{
            check_time(end)?;let entry=entry?;let name=entry.file_name().into_string().map_err(|_|anyhow!("invalid capture member"))?;
            let path=if prefix.is_empty(){name.clone()}else{format!("{prefix}/{name}")};
            let (kind,size,digest)=expected.get(&path).context("unexpected capture payload")?;
            ensure!(seen.insert(path.clone()),"duplicate capture payload");
            let child=open_at(dir,&name,kind=="directory")?;
            ensure!(mount(&child)?==mount_id&&private(&child.metadata()?,kind=="directory"),"unsafe capture member");
            if kind=="directory"{walk(&child,&path,expected,seen,mount_id,end,depth+1)?;child.sync_all()?;}
            else{ensure!(hash_file(&child,end)?==(*size,digest.clone()),"capture payload checksum/size mismatch");child.sync_all()?;}
        }
        ensure!(same(&before,&dir.metadata()?),"capture tree changed during verification");Ok(())
    }
    let mut seen=BTreeSet::new();walk(root,"",expected,&mut seen,&mount(root)?,end,0)?;
    ensure!(seen.len()==expected.len(),"capture is missing a recorded payload");root.sync_all()?;Ok(())
}
fn verify_bundle(root:&File,checksum:&str,revision:&str,source_root:&Path,storage_root:&Path,end:Instant)->Result<Manifest>{
    ensure!(store::valid_revision(checksum),"reviewed manifest checksum required");
    let raw=read_small(root,"manifest.json",MAX_MANIFEST)?;
    ensure!(super::revision(&raw)==checksum,"backup manifest changed since review");
    let manifest:Manifest=serde_json::from_slice(&raw)?;
    ensure!(manifest.revision==revision&&Path::new(&manifest.source_root)==source_root,"capture revision or source root differs");
    let mut expected=expected_paths(&manifest)?;
    expected.insert("manifest.json".into(),("file".into(),raw.len() as u64,checksum.into()));
    verify_tree(root,&expected,end)?;
    let yaml=read_small(root,"server-man.yaml",super::MAX_CONFIG_BYTES)?;
    let config=super::parse(&yaml)?;
    ensure!(super::revision(&yaml)==revision,"captured configuration revision differs");
    // Validate coverage against the captured authoritative definition, not
    // against paths supplied by a manifest author.
    let relative=|path:PathBuf|->Result<String>{Ok(path.strip_prefix(source_root)?.to_str().context("invalid source path")?.into())};
    for storage in config.storage.values(){
        ensure!(!storage.external,"external data is not captured");
        let path=relative(storage_root.join(&storage.path))?;
        ensure!(expected.get(&path).is_some_and(|(kind,_,_)|kind==if storage.kind=="file"{"file"}else{"directory"}),"declared storage is missing from capture");
        if let Some(asset)=&storage.source{
            ensure!(expected.get(&asset.path).is_some_and(|(kind,_,sha)|kind=="file"&&sha==&asset.sha256),"initial image asset checksum differs");
        }
    }
    for secret in config.secrets.values(){ensure!(expected.get(secret).is_some_and(|(kind,_,_)|kind=="file"),"private file is missing from capture");}
    ensure!(!config.shares.values().any(|s|s.enabled),"published share data needs client quiescence");
    let mut native_ids=BTreeSet::new();
    for exp in &manifest.native_exports{
        let id=exp.resources[0].strip_prefix("service/").filter(|id|super::id_valid(id)).context("invalid native capture resource")?;
        ensure!(native_ids.insert(id),"duplicate native capture resource");
        let service=config.services.get(id).context("unknown captured native guest")?;
        ensure!(service.backend==exp.backend&&service.kind=="vm","native capture backend differs");
        match exp.backend.as_str(){
            "libvirt"=>{
                let want=super::libvirt_render::parse_domain(super::libvirt_render::render(&config,id,storage_root)?.as_bytes())?;
                ensure!(exp.native.name==super::libvirt_render::domain_name(id)&&exp.native.project.is_empty()&&exp.native.pool.is_empty()&&exp.native.content_type.is_empty(),"Libvirt native capture identity differs");
                let raw=read_small(root,&exp.bundle_path,super::MAX_CONFIG_BYTES)?;
                super::libvirt_render::validate_backup_xml(&raw,&want)?;
            },
            "incus"=>super::incus::verify_backup_archive(&config,id,&exp.native,read_export(root,&exp.bundle_path)?,end)?,
            _=>return Err(anyhow!("unsupported native capture")),
        }
    }
    ensure!(config.services.iter().filter(|(_,s)|s.kind=="vm").all(|(id,_)|native_ids.contains(id.as_str())),"native guest capture is missing");
    Ok(manifest)
}

pub async fn prepare(store:&Store,storage_root:&Path,provider:&CompositeProvider,opts:PrepareOptions)->Result<serde_json::Value>{
    let end=Instant::now()+deadline(opts.deadline_seconds)?;
    ensure!(store::valid_revision(&opts.revision),"a reviewed configuration revision is required");
    ensure!(clean_absolute(&opts.destination),"destination must be an absolute cleaned path");
    let dir=store::LockedDirectory::try_acquire(store.directory()?)?;
    journal::ensure_idle(&dir)?;ensure_idle_locked(store,&dir)?;
    let snapshot=store::current(&dir,&store.filename)?.context("configuration unavailable")?;
    ensure!(snapshot.revision==opts.revision,"configuration revision changed");
    let config=snapshot.config()?;
    ensure!(!config.shares.values().any(|s|s.enabled),"published shares require verified client quiescence");
    let inv=tokio::time::timeout(end.saturating_duration_since(Instant::now()),inventory(store,storage_root,provider)).await??;
    ensure!(inv.revision==snapshot.revision&&inv.issues.iter().all(|i|i.severity=="warning"),"backup inventory has unresolved input or native dependency issues");
    for input in &inv.inputs{
        ensure!(matches!(input.strategy,"bundle_copy"|"native_export"),"unsupported external/native backup input");
        if input.strategy=="bundle_copy"&&input.id!="configuration"{
            let path=input.bundle_path.as_deref().context("unresolved backup path")?;
            ensure!(portable(path)&&!matches!(path,"manifest.json"|"native"|"server-man.yaml")&&!path.starts_with("native/"),"reserved capture path");
        }else if input.strategy=="native_export"{
            let object=input.native.as_ref().context("unresolved native capture identity")?;
            let id=input.resources[0].strip_prefix("service/").context("invalid native capture resource")?;
            tokio::time::timeout(end.saturating_duration_since(Instant::now()),async{
                match object.backend.as_str(){
                    "libvirt"=>{provider.libvirt.validate_backup(&config,id,object,false).await?;},
                    "incus"=>provider.incus.validate_backup(&config,id,object,false).await?,
                    _=>return Err(anyhow!("native export unsupported")),
                }Ok(())
            }).await??;
        }
    }
    let source_path=store.path();let source_root=source_path.parent().context("configuration root unavailable")?;
    let mut running=Vec::new();
    for id in config.service_order()?{
        if tokio::time::timeout(end.saturating_duration_since(Instant::now()),service_state(provider,&config,id)).await??=="running"{running.push(id.to_owned());}
    }
    ensure!(opts.quiesce||running.is_empty(),"running workloads must be quiesced");
    let required=inv.inputs.iter().filter_map(|i|i.allocated_bytes.or(i.logical_bytes)).try_fold(METADATA_RESERVE,|a,b|a.checked_add(b).context("capture capacity estimate overflow"))?;
    let parent=Path::new(&opts.destination).parent().context("destination parent unavailable")?;
    ensure!(available_bytes(&open_absolute(parent,true)?)? >= required,"insufficient disk space at destination");
    let publication=Publication::new(&opts.destination,source_root)?;
    let mut j=Recovery{version:1,revision:snapshot.revision.clone(),destination:opts.destination.clone(),stage:"capturing".into(),updated_at:Utc::now(),
        services:running.iter().map(|id|(id.clone(),"untouched".into())).collect(),artifact:publication.artifact.clone()};
    persist(store,&dir,&mut j)?;
    let result=async{
        let capture=async{
            for id in running.iter().rev(){
                check_time(end)?;check_revision(store,&dir,&j.revision)?;
                j.services.insert(id.clone(),"stop_attempted".into());persist(store,&dir,&mut j)?;
                provider.action(&config,id,"stop").await?;
                ensure!(service_state(provider,&config,id).await?=="stopped","workload did not reach verified stopped state");
                j.services.insert(id.clone(),"stopped".into());persist(store,&dir,&mut j)?;
            }
            native_boundary(provider,&config,&inv).await?;
            let mut collector=Collector{root:&publication.root,files:vec![],paths:BTreeSet::new(),end};
            let source=open_absolute(&source_path,false)?;
            collector.capture(&source,"server-man.yaml",0,&mount(&source)?)?;
            ensure!(collector.files[0].sha256==snapshot.revision,"configuration changed during copy");
            for input in inv.inputs.iter().filter(|i|i.strategy=="bundle_copy"&&i.id!="configuration"&&i.included_by.is_none()){
                let path=input.path.as_deref().context("missing source path")?;
                let bundle=input.bundle_path.as_deref().context("missing capture path")?;
                let file=open_absolute(Path::new(path),input.kind=="directory")?;
                ensure!(file.metadata()?.is_dir()==(input.kind=="directory"),"capture input type changed");
                collector.capture(&file,bundle,0,&mount(&file)?)?;
            }
            let mut exports=Vec::new();
            for input in inv.inputs.iter().filter(|i|i.strategy=="native_export"){
                let object=input.native.as_ref().context("missing native capture identity")?;
                let id=input.resources[0].strip_prefix("service/").context("invalid native resource")?;
                let exported=match object.backend.as_str(){
                    "libvirt"=>provider.libvirt.backup_export(&config,id,object,&publication.root,end).await?,
                    "incus"=>provider.incus.backup_export(&config,id,object,&publication.root,end,available_bytes(&publication.root)?.saturating_sub(METADATA_RESERVE)).await?,
                    _=>return Err(anyhow!("native exporter unavailable")),
                };
                exports.push(exported);
            }
            native_boundary(provider,&config,&inv).await?;
            check_revision(store,&dir,&j.revision)?;
            Ok::<_,anyhow::Error>((collector.files,exports))
        };
        // Timeout never drops the outer cleanup/recovery step. Persisted
        // stop intent handles commands which may have changed native state.
        let capture=tokio::time::timeout(end.saturating_duration_since(Instant::now()),capture).await;
        let recovery=resume(store,&dir,storage_root,provider,&config,&mut j).await;
        let (files,exports)=capture.context("backup capture deadline exceeded")??;
        let (resumed,failed)=recovery?;
        let manifest=Manifest{source_root:source_root.to_string_lossy().into_owned(),principal:principal()?,
            verification_required:vec!["UID/GID mapping, privileged attributes, complete metadata, native and restore validation remain required.".into()],
            version:1,stage:"captured".into(),revision:snapshot.revision.clone(),created_at:Utc::now(),server_man_version:env!("CARGO_PKG_VERSION").into(),
            files,native_exports:exports,running_workloads_at_backup:running,resumed_workloads:resumed.clone(),failed_resume:failed.clone()};
        let raw=serde_json::to_vec(&manifest)?;
        ensure!(raw.len()<=MAX_MANIFEST,"private manifest exceeds capture limit");
        let checksum=super::revision(&raw);let mut file=create_payload(&publication.root,"manifest.json")?;file.write_all(&raw)?;file.sync_all()?;
        verify_bundle(&publication.root,&checksum,&snapshot.revision,source_root,storage_root,end)?;
        publication.check()?;publication.parent.sync_all()?;
        j.artifact.manifest_checksum=checksum.clone();j.artifact.stage="synced".into();persist(store,&dir,&mut j)?;
        j.artifact.stage="publishing".into();persist(store,&dir,&mut j)?;
        check_revision(store,&dir,&j.revision)?;
        publication.rename()?;
        j.artifact.stage="published".into();persist(store,&dir,&mut j)?;
        let total_bytes=manifest.files.iter().map(|f|f.size_bytes).chain(manifest.native_exports.iter().map(|n|n.size_bytes)).try_fold(raw.len() as u64,|a,b|a.checked_add(b).context("capture size overflow"))?;
        Ok::<_,anyhow::Error>(serde_json::json!({"stage":"captured","destination":opts.destination,"revision":snapshot.revision,"preparedAt":manifest.created_at,
            "totalEntries":manifest.files.len()+manifest.native_exports.len()+1,"totalBytes":total_bytes,"manifestChecksum":checksum,"quiesced":opts.quiesce,
            "resumedServices":resumed,"failedResume":failed}))
    }.await;
    // Never recursively delete a staging tree on uncertain identity. Retain
    // the recorded location for explicit review/release or verified publish.
    if result.is_err()&&j.artifact.stage!="published"{
        j.artifact.stage="uncertain".into();persist(store,&dir,&mut j)?;
    }
    result
}
fn available_bytes(file:&File)->Result<u64>{
    let mut info=std::mem::MaybeUninit::<libc::statvfs>::uninit();
    ensure!(unsafe{libc::fstatvfs(file.as_raw_fd(),info.as_mut_ptr())}==0,"capture filesystem capacity unavailable");
    let info=unsafe{info.assume_init()};
    ensure!(info.f_flag&libc::ST_RDONLY==0,"capture filesystem is read-only");
    Ok((info.f_bavail as u64).saturating_mul(info.f_frsize as u64))
}
pub fn publish(store:&Store,storage_root:&Path,revision:&str,token:&str,seconds:u64)->Result<RecoveryStatus>{
    let end=Instant::now()+deadline(seconds)?;
    let dir=store::LockedDirectory::try_acquire(store.directory()?)?;let mut j=reviewed(store,&dir,revision,token)?;
    ensure!(j.revision==revision&&!j.artifact.manifest_checksum.is_empty()&&matches!(j.artifact.stage.as_str(),"synced"|"publishing"|"uncertain"|"published"),"no verified staged capture is available");
    let dest=Path::new(&j.destination);let parent=open_absolute(dest.parent().context("missing capture parent")?,true)?;
    let p=parent.metadata()?;
    ensure!((p.dev(),p.ino())==(j.artifact.parent_device,j.artifact.parent_inode)&&p.uid()==unsafe{libc::geteuid()}&&p.mode()&0o022==0,"capture parent changed");
    let staged=Path::new(&j.artifact.staging_path).file_name().unwrap().to_str().unwrap();
    let final_name=dest.file_name().unwrap().to_str().unwrap();
    let (root,rename)=match open_at(&parent,final_name,true){
        Ok(file)=>(file,false),
        Err(e) if e.downcast_ref::<rustix::io::Errno>()==Some(&rustix::io::Errno::NOENT)=>(open_at(&parent,staged,true)?,true),
        Err(e)=>return Err(e),
    };
    let m=root.metadata()?;ensure!(private(&m,true)&&(m.dev(),m.ino())==(j.artifact.device,j.artifact.inode),"recorded capture identity changed");
    let source_path=store.path();let source_root=source_path.parent().unwrap();
    verify_bundle(&root,&j.artifact.manifest_checksum,revision,source_root,storage_root,end)?;
    j.artifact.stage="publishing".into();persist(store,&dir,&mut j)?;
    if rename{
        let publication=Publication{parent,root,artifact:j.artifact.clone(),destination:j.destination.clone()};
        publication.rename()?;
    }else{
        let current=open_absolute(dest,true)?;let m=current.metadata()?;
        ensure!((m.dev(),m.ino())==(j.artifact.device,j.artifact.inode),"published capture identity changed");
        parent.sync_all()?;
    }
    j.artifact.stage="published".into();persist(store,&dir,&mut j)?;
    status(store)
}

#[cfg(test)]
mod tests{
    use super::*;
    use std::os::unix::fs::{PermissionsExt,symlink};
    use crate::{command::CommandRunner,config::AppConfig};
    fn fixture()->(tempfile::TempDir,Store,PathBuf,PathBuf){
        let tmp=tempfile::tempdir().unwrap();
        fs::set_permissions(tmp.path(),fs::Permissions::from_mode(0o700)).unwrap();
        let root=tmp.path().join("source");
        let store=Store::new(root.clone());
        store.save(b"version: 1\nstorage:\n  data: {kind: directory, path: data}\n",super::super::MISSING_REVISION).unwrap();
        let storage=root.join("storage");
        fs::create_dir_all(storage.join("data")).unwrap();
        fs::write(storage.join("data/payload"),b"private payload").unwrap();
        let dest=tmp.path().join("backup");
        (tmp,store,storage,dest)
    }
    #[tokio::test]
    async fn capture_and_reviewed_publication_verify_exact_tree(){
        let (_tmp,store,storage,dest)=fixture();
        let provider=CompositeProvider::new(&AppConfig::default(),CommandRunner::new(true));
        let snapshot=store.read().unwrap().unwrap();
        let result=prepare(&store,&storage,&provider,PrepareOptions{revision:snapshot.revision.clone(),destination:dest.to_str().unwrap().into(),quiesce:true,deadline_seconds:30}).await.unwrap();
        assert_eq!(result["stage"],"captured");
        assert_eq!(fs::read(dest.join("storage/data/payload")).unwrap(),b"private payload");
        assert_eq!(fs::read(dest.join("server-man.yaml")).unwrap(),snapshot.bytes);
        assert_eq!(fs::metadata(dest.join("manifest.json")).unwrap().mode()&0o077,0);
        let status_before=status(&store).unwrap();
        let j=status_before.journal.as_ref().unwrap();
        assert_eq!(j.stage,"complete");assert_eq!(j.artifact.stage,"published");
        assert!(ensure_idle(&store).is_ok());
        assert!(publish(&store,&storage,&snapshot.revision,"stale",30).is_err());
        assert!(publish(&store,&storage,&snapshot.revision,&status_before.review_token,30).is_ok());
        let current=status(&store).unwrap();
        fs::write(dest.join("storage/data/payload"),b"tampered").unwrap();
        assert!(publish(&store,&storage,&snapshot.revision,&current.review_token,30).is_err());
        assert_eq!(fs::read(dest.join("storage/data/payload")).unwrap(),b"tampered");
    }
    #[tokio::test]
    async fn interruption_blocks_writes_and_release_does_not_delete_staging(){
        let (_tmp,store,storage,dest)=fixture();
        let provider=CompositeProvider::new(&AppConfig::default(),CommandRunner::new(true));
        let revision=store.read().unwrap().unwrap().revision;
        symlink("/etc/passwd",storage.join("data/unsupported")).unwrap();
        let result=prepare(&store,&storage,&provider,PrepareOptions{revision:revision.clone(),destination:dest.to_str().unwrap().into(),quiesce:true,deadline_seconds:30}).await;
        assert!(result.is_err());assert!(!dest.exists());
        let reviewed=status(&store).unwrap();
        let staged=reviewed.journal.as_ref().unwrap().artifact.staging_path.clone();
        assert!(Path::new(&staged).is_dir());assert!(ensure_idle(&store).is_err());
        assert!(store.save(b"version: 1\n",&revision).is_err());
        assert!(release(&store,&revision,"wrong").is_err());
        let released=release(&store,&revision,&reviewed.review_token).unwrap();
        assert_eq!(released.journal.unwrap().stage,"released");assert!(ensure_idle(&store).is_ok());
        assert!(Path::new(&staged).is_dir());assert!(!dest.exists());
    }
    #[test]
    fn publication_never_replaces_existing_directory(){
        let (_tmp,store,_storage,dest)=fixture();
        let source=store.path();
        fs::create_dir(&dest).unwrap();fs::write(dest.join("keep"),b"keep").unwrap();
        assert!(Publication::new(dest.to_str().unwrap(),source.parent().unwrap()).is_err());
        assert_eq!(fs::read(dest.join("keep")).unwrap(),b"keep");
    }
}
