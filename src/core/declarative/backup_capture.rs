//! Reference backup capture/recovery primitives. A captured bundle is not a
//! restorable backup. Incomplete or unsupported coverage fails closed.
use std::{collections::{BTreeMap,BTreeSet},fs::{self,File,Metadata},io::{Read,Write,Seek,SeekFrom},os::unix::fs::{MetadataExt},os::fd::AsRawFd,path::{Path,PathBuf},time::{Duration,Instant}};
use anyhow::{Result,anyhow,ensure,Context};
use base64::{Engine as _,engine::general_purpose::STANDARD};
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use chrono::{DateTime,Utc};
use serde::{Deserialize,Serialize};
use tokio::sync::watch;
use sha2::{Digest,Sha256};
use rustix::fs::{self as unix_fs,Mode,OFlags,RenameFlags};
use super::{Config,Store,CompositeProvider,backup_inventory::{self,BackupInventory},store,journal};

// Go's nil slices are encoded as JSON null.
mod go_slice {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    pub fn deserialize<'de,D:Deserializer<'de>,T:Deserialize<'de>>(d:D)->Result<Vec<T>,D::Error>{
        Ok(Option::<Vec<T>>::deserialize(d)?.unwrap_or_default())
    }
    pub fn serialize<S:Serializer,T:Serialize>(v:&[T],s:S)->Result<S::Ok,S::Error>{
        if v.is_empty(){s.serialize_none()}else{v.serialize(s)}
    }
}
fn nullable_map<'de,D:serde::Deserializer<'de>>(d:D)->std::result::Result<BTreeMap<String,String>,D::Error>{
    Ok(Option::<BTreeMap<String,String>>::deserialize(d)?.unwrap_or_default())
}
// Reject duplicate keys even in arbitrary maps, before typed decoding.
fn unique_manifest_json(raw:&[u8])->Result<()>{
    use serde::de::{self,DeserializeSeed,MapAccess,SeqAccess,Visitor};
    struct Node(usize);
    impl<'de> DeserializeSeed<'de> for Node{
        type Value=();
        fn deserialize<D:serde::Deserializer<'de>>(self,d:D)->std::result::Result<(),D::Error>{
            if self.0>16{return Err(de::Error::custom("manifest nesting limit exceeded"))}
            d.deserialize_any(self)
        }
    }
    impl<'de> Visitor<'de> for Node{
        type Value=();
        fn expecting(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result{f.write_str("unique-key JSON")}
        fn visit_bool<E:de::Error>(self,_:bool)->std::result::Result<(),E>{Ok(())}
        fn visit_i64<E:de::Error>(self,_:i64)->std::result::Result<(),E>{Ok(())}
        fn visit_u64<E:de::Error>(self,_:u64)->std::result::Result<(),E>{Ok(())}
        fn visit_f64<E:de::Error>(self,_:f64)->std::result::Result<(),E>{Ok(())}
        fn visit_str<E:de::Error>(self,_:&str)->std::result::Result<(),E>{Ok(())}
        fn visit_unit<E:de::Error>(self)->std::result::Result<(),E>{Ok(())}
        fn visit_seq<A:SeqAccess<'de>>(self,mut a:A)->std::result::Result<(),A::Error>{
            while a.next_element_seed(Node(self.0+1))?.is_some(){check_cancel().map_err(de::Error::custom)?;}
            Ok(())
        }
        fn visit_map<A:MapAccess<'de>>(self,mut a:A)->std::result::Result<(),A::Error>{
            let mut keys=BTreeSet::new();
            while let Some(k)=a.next_key::<String>()?{
                if !keys.insert(k){return Err(de::Error::custom("duplicate manifest JSON key"))}
                a.next_value_seed(Node(self.0+1))?;
            }
            Ok(())
        }
    }
    let mut d=serde_json::Deserializer::from_slice(raw);
    let result=Node(0).deserialize(&mut d);
    check_cancel()?;
    result.map_err(|_|anyhow!("invalid private backup manifest JSON"))?;
    d.end().map_err(|_|anyhow!("invalid private backup manifest JSON"))?;
    Ok(())
}
const MAX_ENTRIES:usize=50000;
const MAX_MANIFEST:usize=64*1024*1024;
const METADATA_RESERVE:u64=72*1024*1024;
#[derive(Clone,Debug,Serialize,Deserialize,PartialEq,Eq,PartialOrd,Ord)]
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
#[derive(Clone,Debug,Serialize,Deserialize,PartialEq,Eq)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
struct ManifestFile{
    #[serde(default,skip_serializing_if="is_zero")]captured_link_count:u64,
    #[serde(rename="type")]kind:String,
    uid:u32,gid:u32,atime:String,
    #[serde(default,skip_serializing_if="String::is_empty")]link_target:String,
    #[serde(default,skip_serializing_if="String::is_empty")]hardlink_to:String,
    source_link_count:u64,source_allocated_bytes:u64,
    #[serde(default,deserialize_with="nullable_map",skip_serializing_if="BTreeMap::is_empty")]xattrs:BTreeMap<String,String>,
    xattr_coverage:String,bundle_path:String,size_bytes:u64,sha256:String,mode:String,mtime:String,
}
fn is_zero(v:&u64)->bool{*v==0}
#[derive(Serialize,Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
struct Manifest{
    source_root:String,principal:Principal,#[serde(with="go_slice")]verification_required:Vec<String>,version:u8,stage:String,
    revision:String,#[serde(serialize_with="serialize_go_time")]created_at:DateTime<Utc>,maestro_version:String,files:Vec<ManifestFile>,
    #[serde(with="go_slice")]native_exports:Vec<NativeExport>,
    #[serde(with="go_slice")]running_workloads_at_backup:Vec<String>,
    #[serde(with="go_slice")]resumed_workloads:Vec<String>,
    #[serde(default,with="go_slice",skip_serializing_if="Vec::is_empty")]failed_resume:Vec<String>,
}
#[derive(Serialize,Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
struct Principal{uid:u32,gid:u32,#[serde(with="go_slice")]groups:Vec<u32>,#[serde(default)]uid_map:String,#[serde(default)]gid_map:String}
fn principal()->Result<Principal>{
    let count=unsafe{libc::getgroups(0,std::ptr::null_mut())};
    ensure!((0..=1024).contains(&count),"workload principal groups cannot be established");
    let mut groups=vec![0;count as usize];
    ensure!(unsafe{libc::getgroups(count,groups.as_mut_ptr())}==count,"workload principal groups changed");
    let mapping=|path:&str|->Result<String>{
        let mut raw=Vec::new();
        File::open(path)?.take(65537).read_to_end(&mut raw)?;
        ensure!(!raw.is_empty()&&raw.len()<=65536,"invalid capture account mapping");
        Ok(String::from_utf8(raw)?)
    };
    Ok(Principal{uid:unsafe{libc::geteuid()},gid:unsafe{libc::getegid()},groups,
        uid_map:mapping("/proc/self/uid_map")?,gid_map:mapping("/proc/self/gid_map")?})
}
#[derive(Debug,Clone,Serialize,Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
struct DestinationIdentity{device:u64,inode:u64}
#[derive(Debug,Clone,Serialize,Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub struct Artifact{
    pub stage:String,pub staging_path:String,pub device:u64,pub inode:u64,
    pub parent_device:u64,pub parent_inode:u64,
    #[serde(default,skip_serializing_if="Option::is_none")]reviewed_destination:Option<DestinationIdentity>,
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
    // Bundle member names are UTF-8 and JSON-escaped; unlike declared host
    // paths, descendant filenames may contain line breaks or tabs.
    !p.is_empty()&&p.len()<=4096&&!p.contains('\0')&&!p.starts_with('/')
        &&p.split('/').all(|part|!matches!(part,""|"."|".."))
}
fn pending(j:&Recovery)->bool{
    !matches!(j.stage.as_str(),"complete"|"released")||!matches!(j.artifact.stage.as_str(),"published"|"discarded"|"released")
}
fn validate(j:&Recovery)->Result<()>{
    let a=&j.artifact;
    ensure!(a.reviewed_destination.as_ref().is_none_or(|d|d.inode!=0&&d.device==a.parent_device&&(d.device,d.inode)!=(a.device,a.inode)),"invalid reviewed destination identity");
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
#[derive(Debug)]
pub struct BackupCancelled;
impl std::fmt::Display for BackupCancelled{
    fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result{f.write_str("backup cancelled")}
}
impl std::error::Error for BackupCancelled{}
tokio::task_local!{static BACKUP_CANCEL:watch::Receiver<bool>;}
fn check_cancel()->Result<()>{
    if BACKUP_CANCEL.try_with(|s|*s.borrow()).unwrap_or(false){return Err(BackupCancelled.into())}
    Ok(())
}
pub(super) fn check_time(end:Instant)->Result<()>{check_cancel()?;ensure!(Instant::now()<end,"backup deadline exceeded");Ok(())}
async fn cancelled(){
    let Ok(mut s)=BACKUP_CANCEL.try_with(Clone::clone) else{return std::future::pending::<()>().await};
    if s.wait_for(|v|*v).await.is_err(){std::future::pending::<()>().await;}
}
async fn interruptible<T,F:std::future::Future<Output=Result<T>>>(future:F)->Result<T>{
    tokio::select!{biased; _=cancelled()=>Err(BackupCancelled.into()),value=future=>value}
}
pub(super) fn hash_file(mut file:&File,end:Instant)->Result<(u64,String)>{
    let before=file.metadata()?;ensure!(before.is_file(),"backup payload is not regular");
    let mut h=Sha256::new();let mut count=0u64;let mut buffer=vec![0;128*1024];
    loop{check_time(end)?;let n=file.read(&mut buffer)?;if n==0{break}h.update(&buffer[..n]);count=count.checked_add(n as u64).context("payload size overflow")?;}
    ensure!(count==before.len()&&same(&before,&file.metadata()?),"backup file changed while hashing");
    Ok((count,format!("{:x}",h.finalize())))
}
// Go uses RFC3339Nano, trimming all trailing fractional zeroes.
fn go_timestamp(t:&DateTime<Utc>)->String{
    let mut value=t.to_rfc3339_opts(chrono::SecondsFormat::Nanos,true);
    value.pop(); // Z
    while value.ends_with('0'){value.pop();}
    if value.ends_with('.') {value.pop();}
    value.push('Z');
    value
}
fn serialize_go_time<S:serde::Serializer>(t:&DateTime<Utc>,s:S)->std::result::Result<S::Ok,S::Error>{
    s.serialize_str(&go_timestamp(t))
}
// encoding/json escapes HTML-sensitive characters and Unicode line separators.
// Only string contents can contain these characters in valid JSON.
fn encode_manifest(m:&Manifest)->Result<Vec<u8>>{
    let raw=serde_json::to_string(m)?;
    Ok(raw.replace('&',"\\u0026").replace('<',"\\u003c").replace('>',"\\u003e")
        .replace('\u{2028}',"\\u2028").replace('\u{2029}',"\\u2029").into_bytes())
}
fn metadata(meta:&Metadata,kind:&str,path:&str)->Result<ManifestFile>{
    let ts=|sec,nsec|DateTime::<Utc>::from_timestamp(sec,nsec as u32).map(|t|go_timestamp(&t)).context("invalid file timestamp");
    Ok(ManifestFile{kind:kind.into(),uid:meta.uid(),gid:meta.gid(),atime:ts(meta.atime(),meta.atime_nsec())?,
        mtime:ts(meta.mtime(),meta.mtime_nsec())?,mode:format!("{:04o}",meta.mode()&0o7777),source_link_count:meta.nlink(),
        source_allocated_bytes:meta.blocks().saturating_mul(512),captured_link_count:if kind=="file"{1}else{0},
        link_target:String::new(),hardlink_to:String::new(),xattrs:BTreeMap::new(),
        xattr_coverage:"visible".into(),bundle_path:path.into(),size_bytes:0,sha256:String::new()})
}
// Source attributes stay in the private manifest, not on payloads. Linux may
// hide privileged namespaces from an unprivileged workload account. ACLs and
// security labels, when visible, are captured as exact binary xattr values.
const MAX_XATTR_BYTES:usize=64*1024;
type Xattrs=BTreeMap<String,String>;
fn read_xattrs(file:Option<&File>,link:Option<&Path>)->Result<(Xattrs,String)>{
    let path=link.map(|p|CString::new(p.as_os_str().as_bytes())).transpose()?;
    ensure!(file.is_some()!=path.is_some(),"invalid xattr source");
    let call=|name:Option<&CString>,buffer:&mut [u8]|->std::io::Result<usize>{
        let ptr=if buffer.is_empty(){std::ptr::null_mut()}else{buffer.as_mut_ptr().cast::<libc::c_void>()};
        let len=buffer.len();
        let n=unsafe{match (file,path.as_ref(),name){
            (Some(f),_,None)=>libc::flistxattr(f.as_raw_fd(),ptr.cast(),len),
            (Some(f),_,Some(n))=>libc::fgetxattr(f.as_raw_fd(),n.as_ptr(),ptr,len),
            (None,Some(p),None)=>libc::llistxattr(p.as_ptr(),ptr.cast(),len),
            (None,Some(p),Some(n))=>libc::lgetxattr(p.as_ptr(),n.as_ptr(),ptr,len),
            _=>unreachable!(),
        }};
        if n<0{Err(std::io::Error::last_os_error())}else{Ok(n as usize)}
    };
    let size=match call(None,&mut []){
        Ok(n)=>n,
        Err(e) if e.raw_os_error()==Some(libc::ENOTSUP)=>return Ok((BTreeMap::new(),"unsupported_source_fs".into())),
        Err(_)=>return Err(anyhow!("source extended attribute names cannot be read")),
    };
    ensure!(size<=MAX_XATTR_BYTES,"source attribute names exceed capture limit");
    let mut attrs=BTreeMap::new();
    if size==0{return Ok((attrs,"visible".into()))}
    let mut names=vec![0;size];
    let n=call(None,&mut names).map_err(|_|anyhow!("source attribute names changed during capture"))?;
    ensure!(n<=size&&n>0&&names[n-1]==0,"invalid source attribute names");
    let mut value=vec![0;MAX_XATTR_BYTES];
    let mut encoded_bytes=0usize;
    for raw in names[..n-1].split(|b|*b==0){
        ensure!(!raw.is_empty()&&raw.len()<=255&&attrs.len()<256,"invalid source attribute name/count");
        let name=std::str::from_utf8(raw).map_err(|_|anyhow!("non-UTF-8 source attribute name"))?;
        let c_name=CString::new(raw)?;
        let count=call(Some(&c_name),&mut value).map_err(|_|anyhow!("source attribute value cannot be read"))?;
        ensure!(count<=value.len(),"source attribute exceeds capture limit");
        let encoded=STANDARD.encode(&value[..count]);
        encoded_bytes=encoded_bytes.checked_add(name.len()+encoded.len()).context("attribute size overflow")?;
        ensure!(encoded_bytes<=MAX_MANIFEST,"source attribute metadata exceeds capture limit");
        ensure!(attrs.insert(name.to_owned(),encoded).is_none(),"duplicate source attribute");
    }
    Ok((attrs,"visible".into()))
}
fn observe_attrs(row:&mut ManifestFile,file:Option<&File>,link:Option<&Path>)->Result<()>{
    (row.xattrs,row.xattr_coverage)=read_xattrs(file,link)?;Ok(())
}
fn check_attrs(row:&ManifestFile,file:Option<&File>,link:Option<&Path>)->Result<()>{
    let (attrs,coverage)=read_xattrs(file,link)?;
    ensure!(attrs==row.xattrs&&coverage==row.xattr_coverage,"source extended attributes changed during capture");
    Ok(())
}
// Resolve only lexical components; never follow a symlink on the source or
// target. Relative links may be dangling, but must stay within their declared
// directory tree. Manifest verification additionally requires bundle scope.
fn resolve_link(path:&str,target:&str,boundary:&str)->Result<String>{
    ensure!(!target.is_empty()&&target.len()<=4096&&!target.starts_with('/')&&!target.contains('\0'),"invalid or absolute backup symlink");
    let parent=path.rsplit_once('/').map_or("",|(p,_)|p);
    let mut parts:Vec<&str>=parent.split('/').filter(|p|!p.is_empty()).collect();
    let minimum=boundary.split('/').filter(|p|!p.is_empty()).count();
    ensure!(minimum<=parts.len()&&(boundary.is_empty()||parent==boundary||parent.strip_prefix(boundary).is_some_and(|s|s.starts_with('/'))),"invalid symlink capture boundary");
    for part in target.split('/'){
        match part{
            ""|"."=>{},
            ".."=>{ensure!(parts.len()>minimum,"symlink escapes its declared directory");parts.pop();},
            _=>parts.push(part),
        }
    }
    let result=parts.join("/");
    ensure!(!result.is_empty()&&portable(&result),"symlink target is not a portable bundle path");
    Ok(result)
}
fn real_parent(root:&File,path:&str)->Result<(File,String)>{
    ensure!(portable(path),"invalid backup path");
    let mut parts=path.split('/').collect::<Vec<_>>();
    let leaf=parts.pop().unwrap().to_owned();
    let mut parent=root.try_clone()?;
    for part in parts{parent=open_at(&parent,part,true)?;}
    Ok((parent,leaf))
}
fn leaf_metadata(parent:&File,name:&str)->Result<Metadata>{
    Ok(fs::symlink_metadata(fd_path(parent).join(name))?)
}
fn leaf_target(parent:&File,name:&str)->Result<String>{
    fs::read_link(fd_path(parent).join(name))?.into_os_string().into_string().map_err(|_|anyhow!("non-UTF-8 symlink target"))
}
fn open_source(parent:&File,name:&str,directory:bool)->Result<File>{
    let flags=OFlags::RDONLY|OFlags::NOFOLLOW|OFlags::CLOEXEC|OFlags::NONBLOCK|OFlags::NOATIME
        |if directory{OFlags::DIRECTORY}else{OFlags::empty()};
    match unix_fs::openat(parent,name,flags,Mode::empty()){
        Ok(fd)=>Ok(fd.into()),
        Err(rustix::io::Errno::PERM)=>open_at(parent,name,directory),
        Err(e)=>Err(e.into()),
    }
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
struct CapturedInode{path:String,sha256:String,info:Metadata}
struct Collector<'a>{
    root:&'a File,files:Vec<ManifestFile>,paths:BTreeSet<String>,end:Instant,
    links:BTreeMap<(u64,u64),CapturedInode>,metadata_bytes:usize,
}
impl<'a> Collector<'a>{
    fn new(root:&'a File,end:Instant)->Self{
        Self{root,files:vec![],paths:BTreeSet::new(),end,links:BTreeMap::new(),metadata_bytes:0}
    }
    fn append(&mut self,row:ManifestFile)->Result<()>{
        ensure!(self.files.len()<MAX_ENTRIES&&self.paths.insert(row.bundle_path.clone()),"capture entry limit or duplicate path");
        self.metadata_bytes=self.metadata_bytes.checked_add(serde_json::to_vec(&row)?.len()).context("capture metadata overflow")?;
        ensure!(self.metadata_bytes<=MAX_MANIFEST,"capture-wide metadata limit exceeded");
        self.files.push(row);
        Ok(())
    }
    // Declared inputs must themselves be real regular files or directories.
    // Descendant symlinks are captured without traversal.
    fn declared(&mut self,source:&Path,path:&str,directory:bool)->Result<()>{
        let parent=open_absolute(source.parent().context("missing capture source parent")?,true)?;
        let name=source.file_name().and_then(|s|s.to_str()).context("invalid capture source name")?;
        let before=leaf_metadata(&parent,name)?;
        ensure!(if directory{before.is_dir()}else{before.is_file()},"declared capture input changed type");
        let opened=open_source(&parent,name,directory)?;
        ensure!(same(&before,&opened.metadata()?),"declared capture input changed");
        let source_mount=mount(&opened)?;
        self.capture(&opened,path,0,&source_mount,path)?;
        ensure!(same(&before,&leaf_metadata(&parent,name)?),"declared capture input changed during capture");
        // Ensure the originally opened parent remains at the declared path.
        let current=open_absolute(source.parent().unwrap(),true)?;
        let a=parent.metadata()?;let b=current.metadata()?;
        ensure!((a.dev(),a.ino())==(b.dev(),b.ino()),"declared capture parent changed");
        Ok(())
    }
    fn capture(&mut self,source:&File,path:&str,depth:usize,source_mount:&str,boundary:&str)->Result<()>{
        check_time(self.end)?;
        ensure!(depth<=128&&self.files.len()<MAX_ENTRIES&&portable(path)&&!self.paths.contains(path),"backup tree exceeds limit or contains duplicate path");
        let before=source.metadata()?;
        ensure!(mount(source)?==source_mount,"capture crosses an unreviewed mount");
        if before.is_dir(){
            let (parent,leaf)=target_parent(self.root,path)?;
            let dst=mkdir_at(&parent,&leaf)?;
            let mut row=metadata(&before,"directory",path)?;
            observe_attrs(&mut row,Some(source),None)?;
            self.append(row.clone())?;
            for entry in fs::read_dir(fd_path(source))?{
                check_time(self.end)?;let entry=entry?;
                let name=entry.file_name().into_string().map_err(|_|anyhow!("non-UTF-8 capture path"))?;
                let child_path=format!("{path}/{name}");
                ensure!(portable(&child_path)&&depth<128&&self.files.len()<MAX_ENTRIES,"invalid or excessive source tree");
                let info=leaf_metadata(source,&name)?;
                if info.file_type().is_symlink(){
                    self.capture_link(source,&name,&child_path,&info,boundary,source_mount)?;
                }else{
                    ensure!(info.is_file()||info.is_dir(),"special files need an explicit capture strategy");
                    let child=open_source(source,&name,info.is_dir())?;
                    ensure!(same(&info,&child.metadata()?),"source entry changed before capture");
                    self.capture(&child,&child_path,depth+1,source_mount,boundary)?;
                }
                ensure!(same(&info,&leaf_metadata(source,&name)?),"source entry changed during capture");
            }
            check_attrs(&row,Some(source),None)?;
            dst.sync_all()?;
        }else{
            ensure!(before.is_file(),"special files need an explicit capture strategy");
            let mut row=metadata(&before,"file",path)?;
            observe_attrs(&mut row,Some(source),None)?;
            let identity=(before.dev(),before.ino());
            if let Some(prior)=self.links.get(&identity){
                ensure!(same(&before,&prior.info),"hardlinked source changed during capture");
                let (old_parent,old_name)=real_parent(self.root,&prior.path)?;
                let (new_parent,new_name)=target_parent(self.root,path)?;
                unix_fs::linkat(&old_parent,old_name.as_str(),&new_parent,new_name.as_str(),unix_fs::AtFlags::empty())?;
                new_parent.sync_all()?;
                row.kind="hardlink".into();row.hardlink_to=prior.path.clone();
                row.size_bytes=before.len();row.sha256=prior.sha256.clone();
            }else{
                let mut destination=create_payload(self.root,path)?;
                let mut src=source;let mut h=Sha256::new();let mut count=0u64;let mut buffer=vec![0;128*1024];
                loop{
                    check_time(self.end)?;let n=src.read(&mut buffer)?;if n==0{break}
                    h.update(&buffer[..n]);count=count.checked_add(n as u64).context("capture size overflow")?;
                    ensure!(count<=before.len(),"capture source grew during copy");
                    if buffer[..n].iter().all(|v|*v==0){destination.seek(SeekFrom::Current(n as i64))?;}else{destination.write_all(&buffer[..n])?;}
                }
                destination.set_len(count)?;destination.sync_all()?;
                ensure!(count==before.len(),"capture source changed during copy");
                row.size_bytes=count;row.sha256=format!("{:x}",h.finalize());
                self.links.insert(identity,CapturedInode{path:path.into(),sha256:row.sha256.clone(),info:before.clone()});
            }
            check_attrs(&row,Some(source),None)?;
            self.append(row)?;
        }
        ensure!(same(&before,&source.metadata()?),"source changed during capture");
        Ok(())
    }
    fn capture_link(&mut self,parent:&File,name:&str,path:&str,info:&Metadata,boundary:&str,source_mount:&str)->Result<()>{
        check_time(self.end)?;
        // O_PATH + O_NOFOLLOW opens the symlink inode itself, not its target.
        let link_fd:File=unix_fs::openat(parent,name,OFlags::PATH|OFlags::NOFOLLOW|OFlags::CLOEXEC,Mode::empty())?.into();
        ensure!(mount(&link_fd)?==source_mount&&same(info,&link_fd.metadata()?),"source symlink identity or mount changed");
        let link=leaf_target(parent,name)?;
        resolve_link(path,&link,boundary)?;
        let source_path=fd_path(parent).join(name);
        let mut row=metadata(info,"symlink",path)?;
        row.link_target=link;
        observe_attrs(&mut row,None,Some(&source_path))?;
        let (dst,leaf)=target_parent(self.root,path)?;
        unix_fs::symlinkat(row.link_target.as_str(),&dst,leaf.as_str())?;
        dst.sync_all()?;
        check_attrs(&row,None,Some(&source_path))?;
        ensure!(leaf_target(parent,name)?==row.link_target&&same(info,&leaf_metadata(parent,name)?),"source symlink changed during capture");
        self.append(row)
    }
    fn finish(mut self)->Result<Vec<ManifestFile>>{
        let mut counts=BTreeMap::<String,u64>::new();
        for row in &self.files{
            if row.kind=="file"{*counts.entry(row.bundle_path.clone()).or_default()+=1;}
            else if row.kind=="hardlink"{*counts.entry(row.hardlink_to.clone()).or_default()+=1;}
        }
        for row in &mut self.files{
            if row.kind=="file"||row.kind=="hardlink"{
                let path=if row.kind=="file"{&row.bundle_path}else{&row.hardlink_to};
                row.captured_link_count=counts[path];
                ensure!(row.captured_link_count<=row.source_link_count,"captured hardlink count exceeds source");
            }
        }
        validate_file_metadata(&self.files)?;
        Ok(self.files)
    }
}
fn empty_destination(parent:&File,leaf:&str)->Result<File>{
    let f=open_at(parent,leaf,true)?;
    let m=f.metadata()?;
    ensure!(private(&m,true)&&mount(&f)?==mount(parent)?,"existing destination must be a private real directory on the parent mount");
    ensure!(fs::read_dir(fd_path(&f))?.next().transpose()?.is_none(),"existing destination is not empty");
    let current=open_at(parent,leaf,true)?.metadata()?;
    ensure!((m.dev(),m.ino())==(current.dev(),current.ino()),"destination changed during review");
    Ok(f)
}
fn destination_matches(parent:&File,leaf:&str,expected:Option<&DestinationIdentity>)->Result<Option<File>>{
    match expected{
        Some(identity)=>{
            let f=empty_destination(parent,leaf)?;
            let m=f.metadata()?;
            ensure!((m.dev(),m.ino())==(identity.device,identity.inode),"reviewed destination identity changed");
            Ok(Some(f))
        },
        None=>{
            ensure!(matches!(unix_fs::statat(parent,leaf,unix_fs::AtFlags::SYMLINK_NOFOLLOW),Err(rustix::io::Errno::NOENT)),"destination appeared after review");
            Ok(None)
        }
    }
}
struct Publication{parent:File,root:File,artifact:Artifact,destination:String,_reviewed:Option<File>}
impl Publication{
    fn new(destination:&str,source_root:&Path)->Result<Self>{
        ensure!(clean_absolute(destination),"destination must be an absolute cleaned path");
        let path=Path::new(destination);let parent_path=path.parent().context("missing destination parent")?;
        ensure!(!path.starts_with(source_root)&&!["/proc","/sys","/dev"].iter().any(|p|path.starts_with(p)),"destination cannot be inside configuration or system directories");
        let parent=open_absolute(parent_path,true)?;
        ensure!(parent.metadata()?.uid()==unsafe{libc::geteuid()}&&parent.metadata()?.mode()&0o022==0,"capture parent must be owned and not writable by other users");
        let leaf=path.file_name().and_then(|n|n.to_str()).context("invalid destination")?;
        let reviewed=match unix_fs::statat(&parent,leaf,unix_fs::AtFlags::SYMLINK_NOFOLLOW){
            Err(rustix::io::Errno::NOENT)=>None,
            Ok(_)=>Some(empty_destination(&parent,leaf)?),
            Err(e)=>return Err(e.into()),
        };
        let reviewed_destination=reviewed.as_ref().map(|f|->Result<DestinationIdentity>{
            let m=f.metadata()?;Ok(DestinationIdentity{device:m.dev(),inode:m.ino()})
        }).transpose()?;
        let stage=format!(".staging-backup-{}",uuid::Uuid::new_v4().simple());
        unix_fs::mkdirat(&parent,stage.as_str(),Mode::from_bits_truncate(0o700))?;
        parent.sync_all()?;
        let root=open_at(&parent,&stage,true)?;
        let meta=root.metadata()?;let p=parent.metadata()?;
        Ok(Self{parent,root,artifact:Artifact{stage:"building".into(),staging_path:parent_path.join(stage).to_string_lossy().into_owned(),device:meta.dev(),inode:meta.ino(),parent_device:p.dev(),parent_inode:p.ino(),manifest_checksum:String::new(),reviewed_destination},destination:destination.into(),_reviewed:reviewed})
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
    fn rename(&self,end:Instant)->Result<()>{
        self.check()?;
        let stage=Path::new(&self.artifact.staging_path).file_name().unwrap();
        let dest=Path::new(&self.destination).file_name().unwrap();
        let _reviewed=destination_matches(&self.parent,dest.to_str().context("invalid destination leaf")?,self.artifact.reviewed_destination.as_ref())?;
        self.check()?;
        check_time(end)?;
        // Atomic directory rename refuses nonempty destinations. Never unlink
        // the reviewed empty directory before publication.
        if self.artifact.reviewed_destination.is_some(){
            unix_fs::renameat(&self.parent,stage,&self.parent,dest)?;
        }else{
            unix_fs::renameat_with(&self.parent,stage,&self.parent,dest,RenameFlags::NOREPLACE)?;
        }
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
            Some("incus")=>continue,
            _=>Err(anyhow!("native export is unsupported")),
        };
        if let Ok(object)=result{
            input.native=Some(object);input.inspection="native_dependencies_discovered";
            inv.issues.retain(|issue|issue.resource!=format!("service/{id}")||issue.code!="backup_native_dependencies");
        }
    }
    if let Ok(inputs) = provider.incus.backup_inputs(&config).await {
        inv.inputs.retain(|i| i.kind != "incus_instance_export");
        inv.issues.retain(|issue| issue.code != "backup_native_dependencies" ||
            !issue.resource.strip_prefix("service/").and_then(|id|config.services.get(id)).is_some_and(|s|s.backend=="incus"));
        inv.inputs.extend(inputs);
    }
    ensure!(inv.inputs.len()+inv.requirements.len()<=2048 && serde_json::to_vec(&inv)?.len()<=1024*1024, "backup inventory exceeds bounds");
    ensure!(store.read()?.is_some_and(|s|s.revision==inv.revision),"configuration changed during inventory");
    Ok(inv)
}
async fn service_state(provider:&CompositeProvider,c:&Config,id:&str)->Result<&'static str>{
    let service=c.services.get(id).context("unknown backup workload")?;
    let state=match service.backend.as_str(){
        "libvirt"=>provider.libvirt.status_details(c,id).await?.state,
        "incus"=>provider.incus.backup_state(c,id).await?,
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
                    }else if c.services[id].backend=="incus"{provider.incus.backup_action(c,id,"start").await?;}else{provider.action(c,id,"start").await?;}
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
    let fresh=provider.incus.backup_inputs(c).await?;
    let reviewed:Vec<_>=inv.inputs.iter().filter(|i|i.native.as_ref().is_some_and(|n|n.backend=="incus")).collect();
    ensure!(fresh.len()==reviewed.len()&&fresh.iter().all(|i|reviewed.iter().any(|r|r.kind==i.kind&&r.native==i.native&&r.resources==i.resources)), "Incus backup dependency graph changed");
    for input in inv.inputs.iter().filter(|i|i.strategy=="native_export"){
        let object=input.native.as_ref().context("unresolved native export")?;
        let id=input.resources.first().and_then(|s|s.strip_prefix("service/")).context("invalid native resource")?;
        match object.backend.as_str(){
            "libvirt"=>{provider.libvirt.validate_backup(c,id,object,true).await?;},
            "incus"=>provider.incus.validate_backup(c,input,true).await?,
            _=>return Err(anyhow!("unsupported native exporter")),
        }
    }Ok(())
}
// Manifest metadata is untrusted until bounded and cross-checked. Keep source
// permission/ACL observations separate from the intentionally private payload.
fn validate_file_metadata(files:&[ManifestFile])->Result<()>{
    let mut entries=BTreeMap::new();
    let mut counts=BTreeMap::<&str,u64>::new();
    for f in files{
        check_cancel()?;
        ensure!(portable(&f.bundle_path)&&f.bundle_path!="manifest.json"&&!f.bundle_path.starts_with("manifest.json/")
            &&!f.bundle_path.starts_with("native/")&&f.bundle_path!="native","invalid capture path");
        ensure!(f.source_link_count>0&&f.size_bytes<=i64::MAX as u64&&f.source_allocated_bytes<=i64::MAX as u64
            &&f.mode.len()==4&&f.mode.bytes().all(|b|(b'0'..=b'7').contains(&b))
            &&DateTime::parse_from_rfc3339(&f.atime).is_ok()&&DateTime::parse_from_rfc3339(&f.mtime).is_ok(),"invalid captured metadata");
        ensure!(matches!(f.xattr_coverage.as_str(),"visible"|"unsupported_source_fs")&&f.xattrs.len()<=256
            &&(f.xattr_coverage!="unsupported_source_fs"||f.xattrs.is_empty()),"invalid attribute coverage");
        let mut names=0usize;
        for (name,value) in &f.xattrs{
            names+=name.len()+1;
            ensure!(!name.is_empty()&&name.len()<=255&&!name.contains('\0')&&names<=MAX_XATTR_BYTES
                &&value.len()<=MAX_XATTR_BYTES.div_ceil(3)*4,"invalid captured attribute");
            let raw=STANDARD.decode(value).map_err(|_|anyhow!("invalid captured attribute encoding"))?;
            ensure!(raw.len()<=MAX_XATTR_BYTES&&STANDARD.encode(raw)==*value,"invalid captured attribute value");
        }
        match f.kind.as_str(){
            "file"|"hardlink"=>{
                ensure!(store::valid_revision(&f.sha256)&&f.link_target.is_empty()&&f.captured_link_count>0
                    &&f.captured_link_count<=f.source_link_count&&if f.kind=="file"{f.hardlink_to.is_empty()}else{portable(&f.hardlink_to)},"invalid file or hardlink metadata");
                let canonical=if f.kind=="file"{f.bundle_path.as_str()}else{f.hardlink_to.as_str()};
                *counts.entry(canonical).or_default()+=1;
            },
            "directory"|"symlink"=>{
                ensure!(f.sha256.is_empty()&&f.size_bytes==0&&f.captured_link_count==0&&f.hardlink_to.is_empty(),"invalid directory or symlink metadata");
                if f.kind=="symlink"{resolve_link(&f.bundle_path,&f.link_target,"")?;}else{ensure!(f.link_target.is_empty(),"invalid directory target");}
            },
            _=>return Err(anyhow!("unsupported manifest payload type")),
        }
        ensure!(entries.insert(f.bundle_path.as_str(),f).is_none(),"duplicate manifest payload");
    }
    for f in files{
        check_cancel()?;
        if f.kind=="file"{ensure!(counts[f.bundle_path.as_str()]==f.captured_link_count,"manifest hardlink count differs");}
        else if f.kind=="hardlink"{
            let canonical=entries.get(f.hardlink_to.as_str()).context("hardlink canonical payload missing")?;
            ensure!(canonical.kind=="file","hardlink canonical payload is not a file");
            let mut compare=(**canonical).clone();
            compare.kind=f.kind.clone();compare.bundle_path=f.bundle_path.clone();
            compare.hardlink_to=f.hardlink_to.clone();compare.atime=f.atime.clone();
            ensure!(&compare==f,"hardlink source metadata differs");
        }
    }
    Ok(())
}
fn expected_paths(m:&Manifest)->Result<BTreeMap<String,(String,u64,String)>>{
    ensure!(m.version==1&&m.stage=="captured"&&store::valid_revision(&m.revision)&&clean_absolute(&m.source_root)
        &&m.created_at.timestamp()>-62135596800
        &&!m.files.is_empty()&&m.files.len()<=MAX_ENTRIES&&m.native_exports.len()<=2048,"invalid capture manifest");
    validate_file_metadata(&m.files)?;
    let mut expected=BTreeMap::new();
    for f in &m.files{
        check_cancel()?;
        ensure!(expected.insert(f.bundle_path.clone(),(f.kind.clone(),f.size_bytes,f.sha256.clone())).is_none(),"duplicate manifest payload");
    }
    ensure!(expected.get("maestro.yaml").is_some_and(|(k,_,hash)|k=="file"&&hash==&m.revision),"capture has no pinned configuration");
    for export in &m.native_exports{
        check_cancel()?;
        ensure!(portable(&export.bundle_path)&&export.bundle_path.starts_with("native/")&&export.size_bytes>0&&export.size_bytes<=i64::MAX as u64&&!export.native.name.is_empty()
            &&store::valid_revision(&export.sha256)&&export.backend==export.native.backend
            &&matches!((export.backend.as_str(),export.kind.as_str()),("incus","incus_instance_export")|("incus","incus_custom_volume")|("libvirt","libvirt_guest_state"))
            && !export.resources.is_empty() && export.resources.len()<=128
            && (export.kind=="incus_custom_volume"||export.resources.len()==1)
            && export.resources.windows(2).all(|w|w[0]<w[1]),"invalid native export");
        ensure!(expected.insert(export.bundle_path.clone(),("file".into(),export.size_bytes,export.sha256.clone())).is_none(),"duplicate native export");
    }
    let keys:Vec<_>=expected.keys().cloned().collect();
    for key in keys{
        check_cancel()?;
        let mut at=Path::new(&key).parent();
        let mut depth=0;
        while let Some(p)=at.filter(|p|!p.as_os_str().is_empty()){
            depth+=1;ensure!(depth<=132,"manifest path depth exceeded");
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
    let before=file.metadata()?;
    let mut reader=(&file).take(limit as u64+1);
    let mut raw=Vec::new();let mut buf=[0u8;64*1024];
    loop{
        check_cancel()?;
        let n=reader.read(&mut buf)?;
        if n==0{break}
        raw.extend_from_slice(&buf[..n]);
    }
    ensure!(raw.len()<=limit&&same(&before,&file.metadata()?),"capture metadata changed or exceeds limit");Ok(raw)
}
fn verify_tree(root:&File,expected:&BTreeMap<String,(String,u64,String)>,files:&[ManifestFile],end:Instant)->Result<()>{
    struct Verifier<'a>{
        expected:&'a BTreeMap<String,(String,u64,String)>,
        rows:BTreeMap<&'a str,&'a ManifestFile>,
        seen:BTreeMap<String,Metadata>,
        inodes:BTreeMap<(u64,u64),(String,Metadata)>,
        end:Instant,mount_id:String,
    }
    impl Verifier<'_>{
        fn walk(&mut self,dir:&File,prefix:&str,depth:usize)->Result<()>{
            check_time(self.end)?;ensure!(depth<=132,"capture verification depth exceeded");
            let before=dir.metadata()?;
            ensure!(private(&before,true)&&mount(dir)?==self.mount_id,"unsafe capture directory");
            for entry in fs::read_dir(fd_path(dir))?{
                check_time(self.end)?;let entry=entry?;
                let name=entry.file_name().into_string().map_err(|_|anyhow!("invalid capture member"))?;
                let path=if prefix.is_empty(){name.clone()}else{format!("{prefix}/{name}")};
                let (kind,size,digest)=self.expected.get(&path).context("unexpected capture payload")?.clone();
                let info=leaf_metadata(dir,&name)?;
                ensure!(self.seen.insert(path.clone(),info.clone()).is_none(),"duplicate capture payload");
                if kind=="symlink"{
                    let row=self.rows.get(path.as_str()).context("symlink manifest row missing")?;
                    ensure!(info.file_type().is_symlink()&&info.uid()==unsafe{libc::geteuid()}&&info.nlink()==1
                        &&leaf_target(dir,&name)?==row.link_target,"capture symlink identity or target differs");
                    let link:File=unix_fs::openat(dir,name.as_str(),OFlags::PATH|OFlags::NOFOLLOW|OFlags::CLOEXEC,Mode::empty())?.into();
                    ensure!(mount(&link)?==self.mount_id&&same(&info,&link.metadata()?),"unsafe capture symlink");
                }else{
                    let child=open_at(dir,&name,kind=="directory")?;
                    let opened=child.metadata()?;
                    ensure!(same(&info,&opened)&&mount(&child)?==self.mount_id,"unsafe or changed capture member");
                    if kind=="directory"{
                        self.walk(&child,&path,depth+1)?;child.sync_all()?;
                    }else{
                        let row=self.rows.get(path.as_str());
                        let links=row.map_or(1,|r|r.captured_link_count);
                        let canonical=if kind=="hardlink"{row.context("hardlink row missing")?.hardlink_to.clone()}else{path.clone()};
                        ensure!(opened.is_file()&&opened.uid()==unsafe{libc::geteuid()}&&opened.mode()&0o077==0
                            &&opened.nlink()==links&&opened.len()==size,"capture payload identity, privacy or size differs");
                        let identity=(opened.dev(),opened.ino());
                        if let Some((prior,prior_info))=self.inodes.get(&identity){
                            ensure!(prior==&canonical&&same(prior_info,&opened),"unrecorded or changed capture hardlink");
                        }else{
                            ensure!(hash_file(&child,self.end)?==(size,digest),"capture payload checksum/size mismatch");
                            self.inodes.insert(identity,(canonical,opened));
                        }
                        child.sync_all()?;
                    }
                }
                ensure!(same(&info,&leaf_metadata(dir,&name)?),"capture member changed during verification");
            }
            ensure!(same(&before,&dir.metadata()?),"capture tree changed during verification");Ok(())
        }
    }
    let mut verifier=Verifier{expected,rows:files.iter().map(|r|(r.bundle_path.as_str(),r)).collect(),
        seen:BTreeMap::new(),inodes:BTreeMap::new(),end,mount_id:mount(root)?};
    verifier.walk(root,"",0)?;
    ensure!(verifier.seen.len()==expected.len(),"capture is missing a recorded payload");
    // Recheck through held, no-follow parents after the entire traversal.
    for (path,info) in &verifier.seen{
        check_time(end)?;
        let (parent,leaf)=real_parent(root,path)?;
        ensure!(same(info,&leaf_metadata(&parent,&leaf)?),"capture tree changed during verification");
        if let Some(row)=verifier.rows.get(path.as_str()){
            if row.kind=="hardlink"{
                let canonical=verifier.seen.get(&row.hardlink_to).context("hardlink canonical payload missing")?;
                ensure!((canonical.dev(),canonical.ino())==(info.dev(),info.ino()),"capture hardlink identity differs");
            }
        }
    }
    root.sync_all()?;Ok(())
}
fn verify_bundle(root:&File,checksum:&str,revision:&str,source_root:&Path,storage_root:&Path,end:Instant)->Result<Manifest>{
    ensure!(store::valid_revision(checksum),"reviewed manifest checksum required");
    let raw=read_small(root,"manifest.json",MAX_MANIFEST)?;
    ensure!(super::revision(&raw)==checksum,"backup manifest changed since review");
    unique_manifest_json(&raw)?;
    check_time(end)?;
    let manifest:Manifest=serde_json::from_slice(&raw).map_err(|_|anyhow!("invalid private backup manifest"))?;
    check_time(end)?;
    ensure!(manifest.revision==revision&&Path::new(&manifest.source_root)==source_root,"capture revision or source root differs");
    let mut expected=expected_paths(&manifest)?;
    expected.insert("manifest.json".into(),("file".into(),raw.len() as u64,checksum.into()));
    verify_tree(root,&expected,&manifest.files,end)?;
    let config_file=read_payload(root,"maestro.yaml",false)?;
    let config_meta=config_file.metadata()?;
    let config_row=manifest.files.iter().find(|f|f.bundle_path=="maestro.yaml").context("capture configuration missing")?;
    ensure!(config_meta.is_file()&&config_meta.uid()==unsafe{libc::geteuid()}&&config_meta.mode()&0o077==0
        &&config_meta.nlink()==config_row.captured_link_count&&config_meta.len()==config_row.size_bytes,"unsafe captured configuration");
    let mut yaml=Vec::new();(&config_file).take(super::MAX_CONFIG_BYTES as u64+1).read_to_end(&mut yaml)?;
    ensure!(yaml.len()<=super::MAX_CONFIG_BYTES&&same(&config_meta,&config_file.metadata()?),"capture configuration changed or exceeds limit");
    let config=super::parse(&yaml)?;
    ensure!(!config.services.values().any(|s|s.vm.as_ref().is_some_and(|v|v.is_system_container())), "system-container capture verification is not implemented");
    ensure!(super::revision(&yaml)==revision,"captured configuration revision differs");
    // Validate coverage against the captured authoritative definition, not
    // against paths supplied by a manifest author.
    let relative=|path:PathBuf|->Result<String>{Ok(path.strip_prefix(source_root)?.to_str().context("invalid source path")?.into())};
    for storage in config.storage.values(){
        ensure!(!storage.external,"external data is not captured");
        let path=relative(storage_root.join(&storage.path))?;
        ensure!(expected.get(&path).is_some_and(|(kind,_,_)|if storage.kind=="file"{matches!(kind.as_str(),"file"|"hardlink")}else{kind=="directory"}),"declared storage is missing from capture");
        if let Some(asset)=&storage.source{
            ensure!(expected.get(&asset.path).is_some_and(|(kind,_,sha)|matches!(kind.as_str(),"file"|"hardlink")&&sha==&asset.sha256),"initial image asset checksum differs");
        }
    }
    for secret in config.secrets.values(){ensure!(expected.get(secret).is_some_and(|(kind,_,_)|matches!(kind.as_str(),"file"|"hardlink")),"private file is missing from capture");}
    ensure!(!config.shares.values().any(|s|s.enabled),"published share data needs client quiescence");
    let mut native_ids=BTreeSet::new();
    for exp in &manifest.native_exports{
        let id=exp.resources[0].strip_prefix("service/").filter(|id|super::id_valid(id)).context("invalid native capture resource")?;
        for resource in &exp.resources{
            let id=resource.strip_prefix("service/").filter(|id|super::id_valid(id)).context("invalid native capture resource")?;
            let service=config.services.get(id).context("unknown native capture consumer")?;
            ensure!(service.backend==exp.backend&&service.kind=="vm","native capture consumer backend differs");
        }
        if exp.kind!="incus_custom_volume"{ensure!(native_ids.insert(id),"duplicate native capture resource");}
        let service=config.services.get(id).context("unknown captured native guest")?;
        ensure!(service.backend==exp.backend&&service.kind=="vm","native capture backend differs");
        match exp.backend.as_str(){
            "libvirt"=>{
                let want=super::libvirt_render::parse_domain(super::libvirt_render::render(&config,id,storage_root)?.as_bytes())?;
                ensure!(exp.native.name==super::libvirt_render::domain_name(id)&&exp.native.project.is_empty()&&exp.native.pool.is_empty()&&exp.native.content_type.is_empty(),"Libvirt native capture identity differs");
                let raw=read_small(root,&exp.bundle_path,super::MAX_CONFIG_BYTES)?;
                super::libvirt_render::validate_backup_xml(&raw,&want)?;
            },
            "incus"=>{},
            _=>return Err(anyhow!("unsupported native capture")),
        }
    }
    super::incus::verify_bundle_dependencies(&config,&manifest.native_exports,root,end)?;
    ensure!(config.services.iter().filter(|(_,s)|s.kind=="vm").all(|(id,_)|native_ids.contains(id.as_str())),"native guest capture is missing");
    Ok(manifest)
}

pub async fn prepare_cancellable(store:&Store,storage_root:&Path,provider:&CompositeProvider,opts:PrepareOptions,signal:watch::Receiver<bool>)->Result<serde_json::Value>{
    BACKUP_CANCEL.scope(signal,prepare(store,storage_root,provider,opts)).await
}
pub async fn prepare(store:&Store,storage_root:&Path,provider:&CompositeProvider,opts:PrepareOptions)->Result<serde_json::Value>{
    let end=Instant::now()+deadline(opts.deadline_seconds)?;
    check_time(end)?;
    ensure!(store::valid_revision(&opts.revision),"a reviewed configuration revision is required");
    ensure!(clean_absolute(&opts.destination),"destination must be an absolute cleaned path");
    let dir=store::LockedDirectory::try_acquire(store.directory()?)?;
    journal::ensure_idle(&dir)?;ensure_idle_locked(store,&dir)?;
    let snapshot=store::current(&dir,&store.filename)?.context("configuration unavailable")?;
    ensure!(snapshot.revision==opts.revision,"configuration revision changed");
    let config=snapshot.config()?;
    ensure!(!config.shares.values().any(|s|s.enabled),"published shares require verified client quiescence");
    let inv=interruptible(async{
        Ok(tokio::time::timeout(end.saturating_duration_since(Instant::now()),inventory(store,storage_root,provider)).await??)
    }).await?;
    ensure!(inv.revision==snapshot.revision&&inv.issues.iter().all(|i|i.severity=="warning"),"backup inventory has unresolved input or native dependency issues");
    for input in &inv.inputs{
        ensure!(matches!(input.strategy,"bundle_copy"|"native_export"),"unsupported external/native backup input");
        if input.strategy=="bundle_copy"&&input.id!="configuration"{
            let path=input.bundle_path.as_deref().context("unresolved backup path")?;
            ensure!(portable(path)&&!matches!(path,"manifest.json"|"native"|"maestro.yaml")&&!path.starts_with("native/"),"reserved capture path");
        }else if input.strategy=="native_export"{
            let object=input.native.as_ref().context("unresolved native capture identity")?;
            let id=input.resources[0].strip_prefix("service/").context("invalid native capture resource")?;
            interruptible(async{tokio::time::timeout(end.saturating_duration_since(Instant::now()),async{
                match object.backend.as_str(){
                    "libvirt"=>{provider.libvirt.validate_backup(&config,id,object,false).await?;},
                    "incus"=>provider.incus.validate_backup(&config,input,false).await?,
                    _=>return Err(anyhow!("native export unsupported")),
                }Ok(())
            }).await??;Ok(())}).await?;
        }
    }
    let source_path=store.path();let source_root=source_path.parent().context("configuration root unavailable")?;
    let mut running=Vec::new();
    for id in config.service_order()?{
        if interruptible(async{Ok(tokio::time::timeout(end.saturating_duration_since(Instant::now()),service_state(provider,&config,id)).await??)}).await?=="running"{running.push(id.to_owned());}
    }
    ensure!(opts.quiesce||running.is_empty(),"running workloads must be quiesced");
    let required=inv.inputs.iter().filter_map(|i|i.allocated_bytes.or(i.logical_bytes)).try_fold(METADATA_RESERVE,|a,b|a.checked_add(b).context("capture capacity estimate overflow"))?;
    let parent=Path::new(&opts.destination).parent().context("destination parent unavailable")?;
    ensure!(available_bytes(&open_absolute(parent,true)?)? >= required,"insufficient disk space at destination");
    check_time(end)?;
    let publication=Publication::new(&opts.destination,source_root)?;
    let mut j=Recovery{version:1,revision:snapshot.revision.clone(),destination:opts.destination.clone(),stage:"capturing".into(),updated_at:Utc::now(),
        services:running.iter().map(|id|(id.clone(),"untouched".into())).collect(),artifact:publication.artifact.clone()};
    persist(store,&dir,&mut j)?;
    let result=async{
        let capture=async{
            for id in running.iter().rev(){
                check_time(end)?;check_revision(store,&dir,&j.revision)?;
                j.services.insert(id.clone(),"stop_attempted".into());persist(store,&dir,&mut j)?;
                if config.services[id].backend=="incus"{provider.incus.backup_action(&config,id,"stop").await?;}else{provider.action(&config,id,"stop").await?;}
                ensure!(service_state(provider,&config,id).await?=="stopped","workload did not reach verified stopped state");
                j.services.insert(id.clone(),"stopped".into());persist(store,&dir,&mut j)?;
            }
            native_boundary(provider,&config,&inv).await?;
            let mut collector=Collector::new(&publication.root,end);
            collector.declared(&source_path,"maestro.yaml",false)?;
            ensure!(collector.files[0].sha256==snapshot.revision,"configuration changed during copy");
            for input in inv.inputs.iter().filter(|i|i.strategy=="bundle_copy"&&i.id!="configuration"&&i.included_by.is_none()){
                let path=input.path.as_deref().context("missing source path")?;
                let bundle=input.bundle_path.as_deref().context("missing capture path")?;
                collector.declared(Path::new(path),bundle,input.kind=="directory")?;
            }
            let mut exports=Vec::new();
            for input in inv.inputs.iter().filter(|i|i.strategy=="native_export"){
                let object=input.native.as_ref().context("missing native capture identity")?;
                let id=input.resources[0].strip_prefix("service/").context("invalid native resource")?;
                let exported=match object.backend.as_str(){
                    "libvirt"=>provider.libvirt.backup_export(&config,id,object,&publication.root,end).await?,
                    "incus"=>provider.incus.backup_export(&config,input,&publication.root,end,available_bytes(&publication.root)?.saturating_sub(METADATA_RESERVE)).await?,
                    _=>return Err(anyhow!("native exporter unavailable")),
                };
                exports.push(exported);
            }
            native_boundary(provider,&config,&inv).await?;
            check_revision(store,&dir,&j.revision)?;
            Ok::<_,anyhow::Error>((collector.finish()?,exports))
        };
        // Timeout never drops the outer cleanup/recovery step. Persisted
        // stop intent handles commands which may have changed native state.
        let capture=interruptible(async{
            tokio::time::timeout(end.saturating_duration_since(Instant::now()),capture).await.context("backup capture deadline exceeded")?
        }).await;
        // Recovery has an independent deadline. Do not drop it on cancellation.
        let (resumed,failed)=resume(store,&dir,storage_root,provider,&config,&mut j).await?;
        if capture.is_err()||check_cancel().is_err(){
            ensure!(failed.is_empty(),"backup interrupted; workload recovery needs explicit attention");
        }
        let (files,exports)=capture?;
        check_time(end)?;
        let mut running=running;
        running.sort();
        let manifest=Manifest{source_root:source_root.to_string_lossy().into_owned(),principal:principal()?,
            verification_required:vec!["UID/GID mapping, privileged attribute visibility, inode flags, full durability and native/restore validation require review before this bundle can be marked prepared.".into()],
            version:1,stage:"captured".into(),revision:snapshot.revision.clone(),created_at:Utc::now(),maestro_version:env!("CARGO_PKG_VERSION").into(),
            files,native_exports:exports,running_workloads_at_backup:running,resumed_workloads:resumed.clone(),failed_resume:failed.clone()};
        expected_paths(&manifest)?;
        check_time(end)?;
        let raw=encode_manifest(&manifest)?;
        check_time(end)?;
        ensure!(raw.len()<=MAX_MANIFEST,"private manifest exceeds capture limit");
        let checksum=super::revision(&raw);let mut file=create_payload(&publication.root,"manifest.json")?;file.write_all(&raw)?;file.sync_all()?;
        verify_bundle(&publication.root,&checksum,&snapshot.revision,source_root,storage_root,end)?;
        publication.check()?;publication.parent.sync_all()?;
        j.artifact.manifest_checksum=checksum.clone();j.artifact.stage="synced".into();persist(store,&dir,&mut j)?;
        j.artifact.stage="publishing".into();persist(store,&dir,&mut j)?;
        check_revision(store,&dir,&j.revision)?;
        publication.rename(end)?;
        j.artifact.stage="published".into();persist(store,&dir,&mut j)?;
        let total_bytes=manifest.files.iter().filter(|f|f.kind=="file").map(|f|f.size_bytes).chain(manifest.native_exports.iter().map(|n|n.size_bytes)).try_fold(raw.len() as u64,|a,b|a.checked_add(b).context("capture size overflow"))?;
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
pub fn publish_cancellable(store:&Store,storage_root:&Path,revision:&str,token:&str,seconds:u64,signal:watch::Receiver<bool>)->Result<RecoveryStatus>{
    BACKUP_CANCEL.sync_scope(signal,||publish(store,storage_root,revision,token,seconds))
}
pub fn publish(store:&Store,storage_root:&Path,revision:&str,token:&str,seconds:u64)->Result<RecoveryStatus>{
    let end=Instant::now()+deadline(seconds)?;
    check_time(end)?;
    let dir=store::LockedDirectory::try_acquire(store.directory()?)?;let mut j=reviewed(store,&dir,revision,token)?;
    ensure!(j.revision==revision&&!j.artifact.manifest_checksum.is_empty()&&matches!(j.artifact.stage.as_str(),"synced"|"publishing"|"uncertain"|"published"),"no verified staged capture is available");
    let destination=j.destination.clone();
    let dest=Path::new(&destination);let parent=open_absolute(dest.parent().context("missing capture parent")?,true)?;
    let p=parent.metadata()?;
    ensure!((p.dev(),p.ino())==(j.artifact.parent_device,j.artifact.parent_inode)&&p.uid()==unsafe{libc::geteuid()}&&p.mode()&0o022==0,"capture parent changed");
    let staged=Path::new(&j.artifact.staging_path).file_name().unwrap().to_str().unwrap();
    let final_name=dest.file_name().unwrap().to_str().unwrap();
    let (root,rename)=match open_at(&parent,final_name,true){
        Ok(file)=>{
            let m=file.metadata()?;
            if (m.dev(),m.ino())==(j.artifact.device,j.artifact.inode){(file,false)}
            else{
                ensure!(j.artifact.reviewed_destination.is_some(),"destination identity changed");
                destination_matches(&parent,final_name,j.artifact.reviewed_destination.as_ref())?;
                (open_at(&parent,staged,true)?,true)
            }
        },
        Err(e) if e.downcast_ref::<rustix::io::Errno>()==Some(&rustix::io::Errno::NOENT)=>{
            ensure!(j.artifact.reviewed_destination.is_none(),"reviewed destination disappeared");
            (open_at(&parent,staged,true)?,true)
        },
        Err(e)=>return Err(e),
    };
    let m=root.metadata()?;ensure!(private(&m,true)&&(m.dev(),m.ino())==(j.artifact.device,j.artifact.inode),"recorded capture identity changed");
    let source_path=store.path();let source_root=source_path.parent().unwrap();
    verify_bundle(&root,&j.artifact.manifest_checksum,revision,source_root,storage_root,end)?;
    check_time(end)?;
    j.artifact.stage="publishing".into();persist(store,&dir,&mut j)?;
    if rename{
        let publication=Publication{parent,root,artifact:j.artifact.clone(),destination:j.destination.clone(),_reviewed:None};
        publication.rename(end)?;
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
    use base64::Engine as _;
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
        assert_eq!(fs::read(dest.join("maestro.yaml")).unwrap(),snapshot.bytes);
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
    #[test]
    fn metadata_and_links_are_captured_privately_and_verified(){
        let tmp=tempfile::tempdir().unwrap();
        let source=tmp.path().join("source");let target=tmp.path().join("target");
        fs::create_dir(&source).unwrap();fs::create_dir(&target).unwrap();
        fs::set_permissions(&target,fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(source.join("nested")).unwrap();
        fs::create_dir(source.join("empty")).unwrap();
        let original=source.join("payload");fs::write(&original,b"private").unwrap();
        fs::hard_link(&original,source.join("alias")).unwrap();
        fs::hard_link(&original,tmp.path().join("outside-selection")).unwrap();
        symlink("../payload",source.join("nested/link")).unwrap();
        let f=File::open(&original).unwrap();
        let value=b"\0binary\xff";
        assert_eq!(unsafe{libc::fsetxattr(f.as_raw_fd(),c"user.maestro_test".as_ptr(),value.as_ptr().cast(),value.len(),0)},0);
        let mut acl=2u32.to_le_bytes().to_vec();
        for (tag,perm,id) in [(1u16,6u16,u32::MAX),(2,4,unsafe{libc::geteuid()}),(4,0,u32::MAX),(16,4,u32::MAX),(32,0,u32::MAX)]{
            acl.extend(tag.to_le_bytes());acl.extend(perm.to_le_bytes());acl.extend(id.to_le_bytes());
        }
        assert_eq!(unsafe{libc::fsetxattr(f.as_raw_fd(),c"system.posix_acl_access".as_ptr(),acl.as_ptr().cast(),acl.len(),0)},0);
        let root=File::open(&target).unwrap();
        let end=Instant::now()+Duration::from_secs(30);
        let mut collector=Collector::new(&root,end);
        collector.declared(&source,"data",true).unwrap();
        let rows=collector.finish().unwrap();
        let aliases:Vec<_>=rows.iter().filter(|r|matches!(r.kind.as_str(),"file"|"hardlink")).collect();
        assert_eq!(aliases.len(),2);
        for row in aliases{
            assert_eq!(row.source_link_count,3);assert_eq!(row.captured_link_count,2);
            assert_eq!(STANDARD.decode(&row.xattrs["user.maestro_test"]).unwrap(),value.to_vec());
            assert_eq!(STANDARD.decode(&row.xattrs["system.posix_acl_access"]).unwrap(),acl);
        }
        assert_eq!(fs::metadata(target.join("data/payload")).unwrap().ino(),fs::metadata(target.join("data/alias")).unwrap().ino());
        assert_eq!(fs::read_link(target.join("data/nested/link")).unwrap(),Path::new("../payload"));
        assert!(target.join("data/empty").is_dir());
        let output=File::open(target.join("data/payload")).unwrap();
        assert_eq!(output.metadata().unwrap().mode()&0o077,0);
        assert!(!read_xattrs(Some(&output),None).unwrap().0.contains_key("system.posix_acl_access"));
        let expected=rows.iter().map(|r|(r.bundle_path.clone(),(r.kind.clone(),r.size_bytes,r.sha256.clone()))).collect();
        verify_tree(&root,&expected,&rows,end).unwrap();
        // Equal bytes alone cannot substitute for a recorded hardlink.
        fs::remove_file(target.join("data/alias")).unwrap();
        fs::write(target.join("data/alias"),b"private").unwrap();
        fs::set_permissions(target.join("data/alias"),fs::Permissions::from_mode(0o600)).unwrap();
        assert!(verify_tree(&root,&expected,&rows,end).is_err());
    }
    #[test]
    fn hardlinks_across_declared_inputs_and_escaped_names(){
        let tmp=tempfile::tempdir().unwrap();
        let a=tmp.path().join("a");let b=tmp.path().join("b");let target=tmp.path().join("target");
        fs::create_dir(&a).unwrap();fs::create_dir(&b).unwrap();fs::create_dir(&target).unwrap();
        fs::set_permissions(&target,fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(a.join("line\nbreak"),b"data").unwrap();
        fs::hard_link(a.join("line\nbreak"),b.join("alias")).unwrap();
        symlink("line\nbreak",a.join("link")).unwrap();
        let root=File::open(&target).unwrap();
        let end=Instant::now()+Duration::from_secs(30);
        let mut collector=Collector::new(&root,end);
        collector.declared(&a,"one",true).unwrap();
        collector.declared(&b,"two",true).unwrap();
        let rows=collector.finish().unwrap();
        let alias=rows.iter().find(|r|r.bundle_path=="two/alias").unwrap();
        assert_eq!(alias.hardlink_to,"one/line\nbreak");
        assert_eq!(alias.captured_link_count,2);
        let expected=rows.iter().map(|r|(r.bundle_path.clone(),(r.kind.clone(),r.size_bytes,r.sha256.clone()))).collect();
        verify_tree(&root,&expected,&rows,end).unwrap();
        fs::remove_file(target.join("one/link")).unwrap();
        symlink("different",target.join("one/link")).unwrap();
        assert!(verify_tree(&root,&expected,&rows,end).is_err());
    }
    #[test]
    fn symlink_boundary_and_metadata_validation(){
        assert_eq!(resolve_link("data/nested/link","../payload","data").unwrap(),"data/payload");
        assert!(resolve_link("data/nested/link","../../outside","data").is_err());
        assert!(resolve_link("data/link","/etc/passwd","data").is_err());
        assert!(resolve_link("data/link","../outside","data").is_err());
        let temp=tempfile::tempdir().unwrap();
        let path=temp.path().join("file");fs::write(&path,b"x").unwrap();
        let mut row=metadata(&fs::metadata(&path).unwrap(),"file","data/file").unwrap();
        row.sha256=super::super::revision(b"x");row.size_bytes=1;
        row.xattrs.insert("user.invalid".into(),"not base64!".into());
        assert!(validate_file_metadata(&[row]).is_err());
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
    // Fixed version-1 wire fixture using the reference Go struct field order,
    // nil slice encoding, RFC3339Nano, and encoding/json HTML escaping.
    #[test]
    fn maestro_manifest_wire_fixture_round_trips(){
        let raw=br#"{"sourceRoot":"/source\u003cprivate\u003e","principal":{"uid":1000,"gid":1000,"groups":null,"uidMap":"","gidMap":""},"verificationRequired":null,"version":1,"stage":"captured","revision":"b79606fb3afea5bd1609ed40b622142f1c98125abcfe89a76a661b0e8e343910","createdAt":"2026-11-23T12:00:00.12Z","maestroVersion":"fixture","files":[{"capturedLinkCount":1,"type":"file","uid":1000,"gid":1000,"atime":"2026-11-23T12:00:00.12Z","sourceLinkCount":1,"sourceAllocatedBytes":4096,"xattrCoverage":"visible","bundlePath":"maestro.yaml","sizeBytes":6,"sha256":"b79606fb3afea5bd1609ed40b622142f1c98125abcfe89a76a661b0e8e343910","mode":"0600","mtime":"2026-11-23T12:00:00.12Z"}],"nativeExports":null,"runningWorkloadsAtBackup":null,"resumedWorkloads":null}"#;
        unique_manifest_json(raw).unwrap();
        let manifest:Manifest=serde_json::from_slice(raw).unwrap();
        assert!(expected_paths(&manifest).is_ok());
        assert_eq!(encode_manifest(&manifest).unwrap().as_slice(),raw);
    }
    #[test]
    fn manifest_rejects_duplicate_nested_keys_and_accepts_nil_slices(){
        assert!(unique_manifest_json(br#"{"xattrs":{"user.x":"a","user.x":"b"}}"#).is_err());
        assert!(unique_manifest_json(br#"{"x":1}{}"#).is_err());
        assert!(unique_manifest_json(br#"{"x":null}"#).is_ok());
        #[derive(Serialize,Deserialize)]
        struct Wire{#[serde(with="go_slice")] values:Vec<String>}
        let nil:Wire=serde_json::from_str(r#"{"values":null}"#).unwrap();
        assert!(nil.values.is_empty());
        assert_eq!(serde_json::to_string(&nil).unwrap(),r#"{"values":null}"#);
        assert!(serde_json::from_str::<Wire>(r#"{"values":[]}"#).unwrap().values.is_empty());
    }
    #[tokio::test]
    async fn capture_replaces_reviewed_private_empty_directory(){
        let (_tmp,store,storage,dest)=fixture();
        fs::create_dir(&dest).unwrap();
        fs::set_permissions(&dest,fs::Permissions::from_mode(0o700)).unwrap();
        let provider=CompositeProvider::new(&AppConfig::default(),CommandRunner::new(true));
        let revision=store.read().unwrap().unwrap().revision;
        let result=prepare(&store,&storage,&provider,PrepareOptions{revision,destination:dest.to_str().unwrap().into(),quiesce:true,deadline_seconds:30}).await.unwrap();
        assert_eq!(result["stage"],"captured");
        let manifest:serde_json::Value=serde_json::from_slice(&fs::read(dest.join("manifest.json")).unwrap()).unwrap();
        assert!(manifest["nativeExports"].is_null());
        assert!(manifest["runningWorkloadsAtBackup"].is_null());
        assert!(manifest["resumedWorkloads"].is_null());
    }
    #[test]
    fn reviewed_destination_change_is_not_overwritten(){
        let (_tmp,store,_storage,dest)=fixture();
        fs::create_dir(&dest).unwrap();
        fs::set_permissions(&dest,fs::Permissions::from_mode(0o700)).unwrap();
        let source=store.path();
        let publication=Publication::new(dest.to_str().unwrap(),source.parent().unwrap()).unwrap();
        fs::write(dest.join("keep"),b"keep").unwrap();
        assert!(publication.rename(Instant::now()+Duration::from_secs(30)).is_err());
        assert_eq!(fs::read(dest.join("keep")).unwrap(),b"keep");
        fs::remove_file(dest.join("keep")).unwrap();
        fs::rename(&dest,dest.with_extension("old")).unwrap();
        fs::create_dir(&dest).unwrap();
        fs::set_permissions(&dest,fs::Permissions::from_mode(0o700)).unwrap();
        assert!(publication.rename(Instant::now()+Duration::from_secs(30)).is_err());
    }
    #[tokio::test]
    async fn cancellation_before_capture_does_not_create_destination_or_journal(){
        let (_tmp,store,storage,dest)=fixture();
        let provider=CompositeProvider::new(&AppConfig::default(),CommandRunner::new(true));
        let revision=store.read().unwrap().unwrap().revision;
        let (_sender,signal)=watch::channel(true);
        let error=prepare_cancellable(&store,&storage,&provider,PrepareOptions{revision,destination:dest.to_str().unwrap().into(),quiesce:true,deadline_seconds:30},signal).await.unwrap_err();
        assert!(error.is::<BackupCancelled>());
        assert!(!dest.exists());
        assert!(status(&store).unwrap().journal.is_none());
    }
    #[tokio::test]
    async fn cancellation_interrupts_pending_capture_future(){
        let (sender,signal)=watch::channel(false);
        BACKUP_CANCEL.scope(signal,async{
            let cancel=async move{tokio::task::yield_now().await;sender.send_replace(true);};
            let capture=interruptible(std::future::pending::<Result<()>>());
            let (_,result)=tokio::join!(cancel,capture);
            assert!(result.unwrap_err().is::<BackupCancelled>());
        }).await;
    }
    #[test]
    fn cancelled_publication_preserves_reviewed_directory_and_stage(){
        let (_tmp,store,_storage,dest)=fixture();
        fs::create_dir(&dest).unwrap();
        fs::set_permissions(&dest,fs::Permissions::from_mode(0o700)).unwrap();
        let source=store.path();
        let p=Publication::new(dest.to_str().unwrap(),source.parent().unwrap()).unwrap();
        let original=fs::metadata(&dest).unwrap().ino();
        let (_sender,signal)=watch::channel(true);
        let error=BACKUP_CANCEL.sync_scope(signal,||p.rename(Instant::now()+Duration::from_secs(30))).unwrap_err();
        assert!(error.is::<BackupCancelled>());
        assert_eq!(fs::metadata(&dest).unwrap().ino(),original);
        assert!(Path::new(&p.artifact.staging_path).is_dir());
        assert!(fs::read_dir(&dest).unwrap().next().is_none());
    }
    #[tokio::test]
    async fn reviewed_publish_recovers_staged_capture_into_existing_empty_directory(){
        let (_tmp,store,storage,dest)=fixture();
        let original=dest.with_extension("original");
        let provider=CompositeProvider::new(&AppConfig::default(),CommandRunner::new(true));
        let revision=store.read().unwrap().unwrap().revision;
        prepare(&store,&storage,&provider,PrepareOptions{revision:revision.clone(),destination:original.to_str().unwrap().into(),quiesce:true,deadline_seconds:30}).await.unwrap();
        fs::create_dir(&dest).unwrap();
        fs::set_permissions(&dest,fs::Permissions::from_mode(0o700)).unwrap();
        let source=store.path();
        let publication=Publication::new(dest.to_str().unwrap(),source.parent().unwrap()).unwrap();
        let stage=Path::new(&publication.artifact.staging_path);
        for entry in fs::read_dir(&original).unwrap(){
            let entry=entry.unwrap();
            fs::rename(entry.path(),stage.join(entry.file_name())).unwrap();
        }
        let mut j=status(&store).unwrap().journal.unwrap();
        let checksum=j.artifact.manifest_checksum.clone();
        j.destination=dest.to_str().unwrap().into();
        j.artifact=publication.artifact.clone();
        j.artifact.manifest_checksum=checksum;
        j.artifact.stage="synced".into();
        {let dir=store::LockedDirectory::try_acquire(store.directory().unwrap()).unwrap();persist(&store,&dir,&mut j).unwrap();}
        let review=status(&store).unwrap();
        let (_sender,signal)=watch::channel(true);
        assert!(matches!(publish_cancellable(&store,&storage,&revision,&review.review_token,30,signal),Err(e) if e.is::<BackupCancelled>()));
        assert!(stage.is_dir()&&fs::read_dir(&dest).unwrap().next().is_none());
        let result=publish(&store,&storage,&revision,&review.review_token,30).unwrap();
        assert_eq!(result.journal.unwrap().artifact.stage,"published");
        assert_eq!(fs::read(dest.join("storage/data/payload")).unwrap(),b"private payload");
        assert!(!stage.exists());
    }
    #[test]
    fn publication_never_replaces_nonempty_directory(){
        let (_tmp,store,_storage,dest)=fixture();
        let source=store.path();
        fs::create_dir(&dest).unwrap();fs::write(dest.join("keep"),b"keep").unwrap();
        assert!(Publication::new(dest.to_str().unwrap(),source.parent().unwrap()).is_err());
        assert_eq!(fs::read(dest.join("keep")).unwrap(),b"keep");
    }
}
