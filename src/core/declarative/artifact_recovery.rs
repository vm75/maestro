//! Server Man publication manifests and reviewed metadata-only conflict recovery.
//! Native definitions and runtime state are never changed by this module.
use std::{collections::BTreeMap,fs::{self,File,Metadata},io::Read,path::{Path,PathBuf},os::unix::fs::MetadataExt};
use anyhow::{Result,Context,ensure,anyhow};
use rustix::{fs::{self as unix_fs,AtFlags,OFlags,Mode},io::Errno};
use serde::Serialize;
use super::{Config,Store,CompositeProvider,ObservedResource,ApplyPhase,store,journal};

pub(super) const CONFLICT_REMEDY:&str="Generated artifact publication conflict; review and reconcile private publication metadata before retry.";
const SUFFIX:&str=".server-man-artifacts.json";
const MAX_FILES:usize=256;
const MAX_PRESERVED:usize=8;
type Files=BTreeMap<String,Vec<u8>>;

#[derive(Debug)]
struct ArtifactConflict;
impl std::fmt::Display for ArtifactConflict{
    fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result{f.write_str(CONFLICT_REMEDY)}
}
impl std::error::Error for ArtifactConflict{}
pub(super) fn is_conflict(error:&anyhow::Error)->bool{error.downcast_ref::<ArtifactConflict>().is_some()}
fn conflict<T>(result:Result<T>)->Result<T>{result.map_err(|_|ArtifactConflict.into())}

#[derive(Clone)]
pub(super) struct Target{
    label:&'static str,dir:PathBuf,sidecar_dir:PathBuf,sidecar:String,
    previous:Files,desired:Files,quadlet:Option<String>,
}
impl Target{
    pub(super) fn dedicated(label:&'static str,dir:PathBuf,name:&str,previous:Option<Vec<u8>>,desired:Option<Vec<u8>>)->Result<Self>{
        let leaf=dir.file_name().and_then(|n|n.to_str()).context("invalid artifact directory")?;
        let sidecar=format!("{leaf}{SUFFIX}");
        let sidecar_dir=dir.parent().context("missing artifact parent")?.to_path_buf();
        let map=|bytes:Option<Vec<u8>>|bytes.into_iter().map(|b|(name.to_owned(),b)).collect();
        Ok(Self{label,dir,sidecar_dir,sidecar,previous:map(previous),desired:map(desired),quadlet:None})
    }
    pub(super) fn quadlet(dir:PathBuf,id:&str,previous:Files,desired:Files)->Result<Self>{
        ensure!(super::id_valid(id),"invalid Quadlet artifact scope");
        // Maestro shares a single Quadlet directory among services. Scope the
        // sidecar and complete generated file set to the resource, not all units.
        Ok(Self{label:"Native Quadlet definitions",sidecar_dir:dir.clone(),dir,
            sidecar:format!(".maestro-{id}{SUFFIX}"),previous,desired,quadlet:Some(id.into())})
    }
    fn relevant(&self,name:&str)->bool{
        let Some(id)=&self.quadlet else{return true};
        if self.previous.contains_key(name)||self.desired.contains_key(name){return true}
        if name==format!("maestro-network-{}.network",&super::revision(id.as_bytes())[..24]){return true}
        name.strip_prefix(&format!("maestro-{id}-")).and_then(|s|s.strip_suffix(".container"))
            .is_some_and(|hash|hash.len()==16&&hash.bytes().all(|b|b.is_ascii_digit()||(b'a'..=b'f').contains(&b)))
    }
}
#[derive(Serialize)]
struct PublicationManifest{owner:&'static str,version:u8,files:BTreeMap<String,String>}
fn canonical(files:&Files)->Result<Vec<u8>>{
    ensure!(files.len()<=MAX_FILES&&files.keys().all(|n|!n.is_empty()&&n!="."&&n!=".."&&!n.contains(['/','\0'])),"invalid artifact file set");
    Ok(serde_json::to_vec(&PublicationManifest{owner:"server-man",version:1,files:files.iter().map(|(n,b)|(n.clone(),super::revision(b))).collect()})?)
}
fn optional<T>(r:Result<T>)->Result<Option<T>>{
    match r{Ok(v)=>Ok(Some(v)),Err(e) if e.downcast_ref::<Errno>()==Some(&Errno::NOENT)=>Ok(None),Err(e)=>Err(e)}
}
fn open_dir(path:&Path)->Result<File>{
    let dir=super::backup_capture::open_absolute(path,true)?;
    let m=dir.metadata()?;
    ensure!(m.uid()==unsafe{libc::geteuid()}&&m.mode()&0o077==0,"unsafe private artifact directory");
    Ok(dir)
}
fn parent(target:&Target)->Result<Option<File>>{
    optional(open_dir(&target.sidecar_dir))
}
fn stable(a:&Metadata,b:&Metadata)->bool{
    a.dev()==b.dev()&&a.ino()==b.ino()&&a.mode()==b.mode()&&a.uid()==b.uid()&&a.gid()==b.gid()
        &&a.nlink()==b.nlink()&&a.len()==b.len()&&a.mtime()==b.mtime()&&a.mtime_nsec()==b.mtime_nsec()
        &&a.ctime()==b.ctime()&&a.ctime_nsec()==b.ctime_nsec()
}
fn read(dir:&File,name:&str)->Result<Option<Vec<u8>>>{
    let Some(fd)=optional(unix_fs::openat(dir,name,OFlags::RDONLY|OFlags::NOFOLLOW|OFlags::NONBLOCK|OFlags::CLOEXEC,Mode::empty()).map_err(anyhow::Error::from))? else{return Ok(None)};
    let file=File::from(fd);let before=file.metadata()?;
    ensure!(before.is_file()&&before.nlink()==1&&before.uid()==unsafe{libc::geteuid()}&&before.mode()&0o077==0
        &&before.len()<=super::MAX_CONFIG_BYTES as u64,"unsafe private publication metadata or generated file");
    let mut bytes=Vec::new();(&file).take(super::MAX_CONFIG_BYTES as u64+1).read_to_end(&mut bytes)?;
    let current=fs::symlink_metadata(super::backup_capture::fd_path(dir).join(name))?;
    ensure!(bytes.len() as u64==before.len()&&stable(&before,&file.metadata()?)&&stable(&before,&current),"private artifact changed during review");
    Ok(Some(bytes))
}
fn sidecar(target:&Target)->Result<Option<Vec<u8>>>{
    match parent(target)?{Some(dir)=>read(&dir,&target.sidecar),None=>Ok(None)}
}
fn actual(target:&Target)->Result<Files>{
    let Some(dir)=optional(open_dir(&target.dir))? else{return Ok(Files::new())};
    let before=dir.metadata()?;
    let mut out=Files::new();
    let mut total=0usize;
    for (at,entry) in fs::read_dir(super::backup_capture::fd_path(&dir))?.enumerate(){
        ensure!(at<4096,"artifact directory inventory exceeds limit");
        let name=entry?.file_name().into_string().map_err(|_|anyhow!("invalid artifact filename"))?;
        if !target.relevant(&name){continue}
        ensure!(out.len()<MAX_FILES,"too many generated files");
        let bytes=read(&dir,&name)?.context("artifact disappeared")?;
        total=total.checked_add(bytes.len()).context("artifact size overflow")?;
        ensure!(total<=16*super::MAX_CONFIG_BYTES,"artifact inventory exceeds limit");
        out.insert(name,bytes);
    }
    ensure!(stable(&before,&dir.metadata()?),"generated directory changed during review");
    check_identity(&dir,&target.dir)?;
    Ok(out)
}
fn matching(target:&Target)->Result<(Files,&'static str)>{
    let files=actual(target)?;
    if files==target.desired{return Ok((files,"desired"))}
    if files==target.previous{return Ok((files,"previous"))}
    Err(anyhow!("generated files do not match reviewed definitions"))
}
fn check_identity(dir:&File,path:&Path)->Result<()>{
    let other=open_dir(path)?;
    let a=dir.metadata()?;let b=other.metadata()?;
    ensure!((a.dev(),a.ino())==(b.dev(),b.ino()),"artifact metadata parent changed");
    Ok(())
}
pub(super) fn before_publication(targets:&[Target])->Result<Vec<Option<Vec<u8>>>>{
    conflict(targets.iter().map(|target|{
        let (files,_)=matching(target)?;
        let prior=sidecar(target)?;
        if let Some(raw)=&prior{
            ensure!(*raw==canonical(&files)?,"conflicting publication metadata");
        }
        // Missing legacy sidecars can be created only by a normal exact-byte
        // publication. A failed/observed desired state cannot skip that step.
        Ok(prior)
    }).collect())
}
pub(super) fn after_publication(targets:&[Target],seals:&[Option<Vec<u8>>])->Result<()>{
    conflict((||{
        ensure!(targets.len()==seals.len(),"invalid publication seal");
        for (target,prior) in targets.iter().zip(seals){
            ensure!(actual(target)?==target.desired,"generated artifact publication differs");
            let Some(dir)=parent(target)? else{
                ensure!(target.desired.is_empty()&&prior.is_none(),"publication metadata parent missing");continue
            };
            let lock=store::LockedDirectory::try_acquire(dir)?;
            check_identity(&lock,&target.sidecar_dir)?;
            ensure!(&read(&lock,&target.sidecar)?==prior,"publication metadata changed");
            ensure!(actual(target)?==target.desired,"generated artifacts changed");
            if target.desired.is_empty(){
                if prior.is_some(){unix_fs::unlinkat(&lock,target.sidecar.as_str(),AtFlags::empty())?;lock.sync_all()?;}
            }else{
                let expected=canonical(&target.desired)?;
                if prior.as_ref()!=Some(&expected){store::publish(&lock,&target.sidecar,&expected,prior.is_none())?;}
            }
        }
        Ok(())
    })())
}
pub(super) fn check_observation(targets:&[Target])->Result<()>{
    conflict((||{
        for target in targets{
            let (files,state)=matching(target)?;
            let raw=sidecar(target)?;
            if files.is_empty(){ensure!(raw.is_none(),"unexpected publication metadata");}
            else if let Some(raw)=raw{ensure!(raw==canonical(&files)?,"conflicting publication metadata");}
            else if state=="desired"&&target.previous!=target.desired{ensure!(false,"missing desired publication metadata");}
        }
        Ok(())
    })())
}
pub(super) fn check_observed_state(targets:&[Target],state:ObservedResource)->Result<()>{
    if state!=ObservedResource::After{return Ok(())}
    conflict((||{
        for target in targets{
            ensure!(actual(target)?==target.desired,"generated artifact differs");
            let raw=sidecar(target)?;
            if target.desired.is_empty(){ensure!(raw.is_none(),"unexpected publication metadata");}
            else{ensure!(raw==Some(canonical(&target.desired)?),"missing or conflicting desired publication metadata");}
        }
        Ok(())
    })())
}

#[derive(Clone,Serialize)]
#[serde(rename_all="camelCase")]
pub struct ReviewPart{label:&'static str,files:&'static str,manifest:&'static str}
#[derive(Serialize)]
#[serde(rename_all="camelCase")]
pub struct Review{
    resource:String,revision:String,review_token:String,can_reconcile:bool,parts:Vec<ReviewPart>,
}
struct Part{target:Target,expected:Vec<u8>,original:Option<Vec<u8>>,change:bool}
fn preserved_name(target:&Target,raw:&[u8])->String{
    format!("{}.preserved-{}",target.sidecar,super::revision(raw))
}
fn preservation_available(target:&Target,raw:&[u8])->Result<bool>{
    let dir=parent(target)?.context("artifact metadata parent missing")?;
    let name=preserved_name(target,raw);
    let prefix=format!("{}.preserved-",target.sidecar);
    let mut count=0;
    for (at,entry) in fs::read_dir(super::backup_capture::fd_path(&dir))?.enumerate(){
        ensure!(at<1024,"artifact metadata inventory limit reached");
        if entry?.file_name().to_str().is_some_and(|n|n.starts_with(&prefix)){count+=1;}
    }
    if let Some(existing)=read(&dir,&name)?{return Ok(existing==raw)}
    Ok(count<MAX_PRESERVED)
}
fn inspect_targets(targets:Vec<Target>,resource:&str,revision:&str,journal_seal:&str)->Result<(Review,Vec<Part>)>{
    let mut review=Review{resource:resource.into(),revision:revision.into(),review_token:String::new(),can_reconcile:false,parts:vec![]};
    let mut parts=vec![];
    let mut seal=vec![];
    let mut repairable=!targets.is_empty();let mut changed=false;
    for target in targets{
        let mut row=ReviewPart{label:target.label,files:"mismatch_or_unsafe",manifest:"unverified"};
        let mut part=Part{target,expected:vec![],original:None,change:false};
        match matching(&part.target){
            Ok((files,state)) if !files.is_empty()=>{
                row.files=state;part.expected=canonical(&files)?;
                match sidecar(&part.target){
                    Ok(old)=>{
                        row.manifest=match &old{None=>"missing",Some(raw) if *raw==part.expected=>"current",_=>"conflicting"};
                        part.original=old;part.change=row.manifest!="current";
                        if part.change{
                            if let Some(raw)=&part.original{
                                if !preservation_available(&part.target,raw).unwrap_or(false){row.manifest="preservation_limit";repairable=false;}
                            }
                            changed=true;
                        }
                    },
                    Err(_)=>{row.manifest="unsafe";repairable=false;}
                }
            },
            _=>repairable=false,
        }
        seal.push(serde_json::json!([row,super::revision(&part.expected),part.original.as_ref().map(|b|super::revision(b))]));
        review.parts.push(row);parts.push(part);
    }
    review.can_reconcile=repairable&&changed;
    review.review_token=super::revision(&serde_json::to_vec(&(resource,revision,journal_seal,seal))?);
    Ok((review,parts))
}
fn state(store:&Store,provider:&CompositeProvider,dir:&File,resource:&str)->Result<(Review,Vec<Part>)>{
    let id=resource.strip_prefix("service/").filter(|s|super::id_valid(s)).context("invalid service resource")?;
    let j=journal::read(dir)?.context("failed apply journal required")?;
    ensure!(j.authority==store.filename&&j.phase==ApplyPhase::Applying&&j.artifact_conflict&&j.failed_resource.as_deref()==Some(resource),"no recorded generated artifact conflict");
    let change=j.plan.changes.get(j.completed).context("failed resource unavailable")?;
    ensure!(change.resource==resource&&change.after.is_some(),"removed resources require manual recovery");
    let current=store::current(dir,&store.filename)?.context("configuration unavailable")?;
    ensure!(current.revision==j.plan.desired_revision,"configuration revision changed");
    let desired=current.config()?;
    ensure!(desired.services.contains_key(id)&&change.after.as_deref()==Some(super::plan::service_fingerprint(&desired,id)?.as_str()),"resource fingerprint changed");
    let candidate=journal::candidate(dir,&j)?;
    ensure!(candidate==current.bytes,"failed candidate differs from authoritative configuration");
    let prior=if j.plan.base_revision==super::MISSING_REVISION{None}else{Some(store.read_history(&j.plan.base_revision)?)};
    let (previous,plan)=Store::plan(prior,&candidate,&j.plan.base_revision,&desired)?;
    ensure!(plan.fingerprint==j.plan.fingerprint&&serde_json::to_vec(&plan.changes)?==serde_json::to_vec(&j.plan.changes)?,"failed plan differs");
    let targets=provider.recovery_targets(change,&previous,&desired)?;
    ensure!(!targets.is_empty(),"generated artifact review unavailable");
    let seal=super::revision(&serde_json::to_vec(&j)?);
    inspect_targets(targets,resource,&current.revision,&seal)
}
pub fn review(store:&Store,provider:&CompositeProvider,resource:&str)->Result<Review>{
    let dir=store.directory()?;
    let (review,_)=state(store,provider,&dir,resource)?;
    Ok(review)
}
fn repair(part:&Part)->Result<()>{
    let target=&part.target;
    let dir=parent(target)?.context("artifact metadata parent missing")?;
    let lock=store::LockedDirectory::try_acquire(dir)?;
    check_identity(&lock,&target.sidecar_dir)?;
    ensure!(read(&lock,&target.sidecar)?==part.original,"publication metadata changed");
    let (files,_)=matching(target)?;
    ensure!(canonical(&files)?==part.expected,"generated files changed");
    if let Some(raw)=&part.original{
        ensure!(preservation_available(target,raw)?,"preserved metadata limit reached");
        let name=preserved_name(target,raw);
        if let Some(saved)=read(&lock,&name)?{ensure!(saved==*raw,"preserved metadata differs");}
        else{store::publish(&lock,&name,raw,true)?;}
        lock.sync_all()?;
    }
    check_identity(&lock,&target.sidecar_dir)?;
    ensure!(read(&lock,&target.sidecar)?==part.original&&canonical(&matching(target)?.0)?==part.expected,"publication changed during reconciliation");
    store::publish(&lock,&target.sidecar,&part.expected,part.original.is_none())?;
    Ok(())
}
pub fn reconcile(store:&Store,provider:&CompositeProvider,resource:&str,revision:&str,token:&str)->Result<Review>{
    let dir=store.lock_directory()?;
    super::backup_capture::ensure_idle_locked(store,&dir)?;
    let (review,parts)=state(store,provider,&dir,resource)?;
    ensure!(review.can_reconcile&&review.revision==revision&&review.review_token==token,"artifact recovery review changed");
    for part in &parts{if part.change{repair(part)?;}}
    // Leave the failed apply journal and all native definitions untouched.
    state(store,provider,&dir,resource).map(|(review,_)|review)
}

#[cfg(test)]
mod tests{
    use super::*;
    use std::os::unix::fs::{PermissionsExt,symlink};
    fn fixture()->(tempfile::TempDir,Target){
        let tmp=tempfile::tempdir().unwrap();
        let dir=tmp.path().join("guest");
        fs::create_dir(&dir).unwrap();fs::set_permissions(&dir,fs::Permissions::from_mode(0o700)).unwrap();
        let root=open_dir(&dir).unwrap();store::publish(&root,"instance.yaml",b"desired",true).unwrap();
        let target=Target::dedicated("Native Incus definition",dir,"instance.yaml",Some(b"previous".to_vec()),Some(b"desired".to_vec())).unwrap();
        (tmp,target)
    }
    #[test]
    fn preserves_conflicting_metadata_and_never_rewrites_definitions(){
        let (_tmp,target)=fixture();
        let parent=parent(&target).unwrap().unwrap();store::publish(&parent,&target.sidecar,b"invalid private metadata",true).unwrap();
        assert!(is_conflict(&check_observation(&[target.clone()]).unwrap_err()));
        let (review,parts)=inspect_targets(vec![target.clone()],"service/guest","revision","journal").unwrap();
        assert!(review.can_reconcile);assert_eq!(review.parts[0].files,"desired");assert_eq!(review.parts[0].manifest,"conflicting");
        let encoded=serde_json::to_string(&review).unwrap();assert!(!encoded.contains("invalid private metadata"));
        repair(&parts[0]).unwrap();
        assert_eq!(read(&parent,&preserved_name(&target,b"invalid private metadata")).unwrap().unwrap(),b"invalid private metadata");
        assert_eq!(actual(&target).unwrap(),target.desired);
        assert!(check_observation(&[target.clone()]).is_ok());
        assert!(!inspect_targets(vec![target],"service/guest","revision","journal").unwrap().0.can_reconcile);
    }
    #[test]
    fn missing_sidecar_can_be_repaired_but_stale_review_and_foreign_files_fail(){
        let (_tmp,target)=fixture();
        let (review,parts)=inspect_targets(vec![target.clone()],"service/guest","revision","journal").unwrap();
        assert!(review.can_reconcile);assert_eq!(review.parts[0].manifest,"missing");
        let parent=parent(&target).unwrap().unwrap();store::publish(&parent,&target.sidecar,b"changed",true).unwrap();
        assert!(repair(&parts[0]).is_err());
        let updated=inspect_targets(vec![target.clone()],"service/guest","revision","journal").unwrap().0;
        assert_ne!(review.review_token,updated.review_token);
        let dir=open_dir(&target.dir).unwrap();store::publish(&dir,"foreign",b"do not change",true).unwrap();
        assert!(!inspect_targets(vec![target],"service/guest","revision","journal").unwrap().0.can_reconcile);
    }
    #[tokio::test]
    async fn failed_apply_review_reconcile_and_explicit_retry(){
        use crate::{config::AppConfig,command::CommandRunner};
        let tmp=tempfile::tempdir().unwrap();
        let mut config=AppConfig::default();
        config.paths.applications_dir=tmp.path().join("apps");
        config.paths.storage_dir=tmp.path().join("storage");
        fs::create_dir(&config.paths.applications_dir).unwrap();
        fs::set_permissions(&config.paths.applications_dir,fs::Permissions::from_mode(0o700)).unwrap();
        let provider=CompositeProvider::new(&config,CommandRunner::new(true));
        let store=Store::new(tmp.path().join("authority"));
        let initial=b"version: 1\nservices:\n  guest: {kind: container, engine: docker, backend: compose, containers: {web: {image: alpine}}}\n";
        let plan=store.preview(initial,super::super::MISSING_REVISION).unwrap();
        let saved=store.apply_review(initial,super::super::MISSING_REVISION,&plan.fingerprint,&provider).await.unwrap();
        let sidecar=config.paths.applications_dir.join("guest.server-man-artifacts.json");
        let original=config.paths.applications_dir.join("guest/compose.yaml");
        let native=fs::read(&original).unwrap();
        assert!(sidecar.is_file());
        fs::write(&sidecar,b"conflict").unwrap();
        let next=String::from_utf8(initial.to_vec()).unwrap().replace("alpine","alpine:3");
        let plan=store.preview(next.as_bytes(),&saved.revision).unwrap();
        assert!(store.apply_review(next.as_bytes(),&saved.revision,&plan.fingerprint,&provider).await.is_err());
        let failed=store.pending_apply().unwrap().unwrap();
        assert!(failed.artifact_conflict);
        let reviewed=review(&store,&provider,"service/guest").unwrap();
        assert!(reviewed.can_reconcile);
        assert_eq!(reviewed.parts[0].files,"previous");
        assert!(reconcile(&store,&provider,"service/guest",&reviewed.revision,"stale").is_err());
        let repaired=reconcile(&store,&provider,"service/guest",&reviewed.revision,&reviewed.review_token).unwrap();
        assert!(!repaired.can_reconcile);
        assert_eq!(fs::read(&original).unwrap(),native);
        assert!(store.pending_apply().unwrap().unwrap().artifact_conflict);
        let status=store.retry_apply(&provider).await.unwrap();
        assert_eq!(status["stage"],"complete");
        assert_ne!(fs::read(&original).unwrap(),native);
    }
    #[test]
    fn unsafe_sidecar_is_not_repaired(){
        let (_tmp,target)=fixture();
        symlink(target.dir.join("instance.yaml"),target.sidecar_dir.join(&target.sidecar)).unwrap();
        let (review,_)=inspect_targets(vec![target],"service/guest","revision","journal").unwrap();
        assert!(!review.can_reconcile);assert_eq!(review.parts[0].manifest,"unsafe");
    }
}
