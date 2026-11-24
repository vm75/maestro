//! Direct port of Server Man incus.go, incus_render.go and the scoped
//! Incus instance log reader. Native mutations use local project-qualified
//! commands, with definitions on stdin and no implicit profiles.
use std::{collections::BTreeMap,path::PathBuf,time::Duration};
use anyhow::{Result,anyhow,ensure};
use serde::{Deserialize,Serialize,de::DeserializeOwned};
use serde_json::Value;
use crate::command::CommandRunner;
use super::{ApplyMode,ApplyProvider,Change,Config,ObservedResource,Operation,RecoveryProvider,Service,
    artifacts::{check_bytes,read_artifact,publish_artifact},composite::ManagedLogs,plan::service_fingerprint,store};

const ID:&str="user.server-man.id";
const IDENTITY:&str="user.server-man.identity";
const FINGERPRINT:&str="user.server-man.fingerprint";
const RUNTIME:&str="user.server-man.runtime";
const ACTIVE_RUNTIME:&str="user.server-man.active-runtime";
const STOPPED_UPDATE:&str="stop the Incus VM before changing its hardware or native description; Save for next start cannot defer running Incus hardware edits in v1";
const ENV:&[(&str,&str)]=&[("LC_ALL","C")];

#[derive(Clone,Debug,Default,Serialize,Deserialize,PartialEq,Eq)]
#[serde(default)]
struct Definition{
    #[serde(skip_serializing_if="String::is_empty")]
    architecture:String,
    description:String,
    config:BTreeMap<String,String>,
    devices:BTreeMap<String,BTreeMap<String,String>>,
    profiles:Vec<String>,
    ephemeral:bool,
}
#[derive(Debug,Default,Deserialize)]
#[serde(default)]
struct Instance{
    #[serde(flatten)]
    definition:Definition,
    name:String,
    #[serde(rename="type")]
    kind:String,
    status:String,
    status_code:i64,
    location:String,
    expanded_devices:BTreeMap<String,BTreeMap<String,String>>,
}
fn binding_name(value:&str)->bool{
    !value.is_empty()&&value.len()<=128&&value.as_bytes()[0].is_ascii_alphanumeric()
        &&value.bytes().all(|b|b.is_ascii_alphanumeric()||b"_.-".contains(&b))
}
fn instance_name(id:&str)->String{
    if id.len()<=60&&!id.contains('_'){return format!("sm-{id}")}
    let name=id.replace('_',"-");
    format!("sm-{}-{}",&name[..name.len().min(49)],&super::revision(id.as_bytes())[..10])
}
fn target(id:&str)->String{format!("local:{}",instance_name(id))}
fn instance_path(id:&str)->String{format!("/1.0/instances/{}",instance_name(id))}
fn vm<'a>(config:&'a Config,id:&str)->Result<&'a super::VM>{
    let s=config.services.get(id).ok_or_else(||anyhow!("unknown Incus VM"))?;
    ensure!(s.kind=="vm"&&s.backend=="incus","resource is not an Incus VM");
    s.vm.as_ref().ok_or_else(||anyhow!("missing Incus VM definition"))
}
fn project<'a>(config:&'a Config,id:&str)->Result<&'a str>{
    let v=vm(config,id)?;
    let binding=config.bindings.get(&v.project).filter(|b|b.kind=="project"&&binding_name(&b.name))
        .ok_or_else(||anyhow!("Incus requires an explicit valid project binding"))?;
    Ok(&binding.name)
}
fn runtime_fingerprint(config:&Config,id:&str)->Result<String>{
    let mut c=config.clone();
    let service=c.services.get_mut(id).ok_or_else(||anyhow!("unknown Incus VM"))?;
    service.enabled=false;service.name.clear();service.description.clear();
    service_fingerprint(&c,id)
}
fn render(config:&Config,id:&str)->Result<Definition>{
    let mut config=config.clone();config.validate()?;
    ensure!(super::id_valid(id),"invalid Incus service identity");
    let v=vm(&config,id)?;let s=&config.services[id];
    ensure!(!v.uuid.is_empty()&&!v.mac.is_empty(),"persistent VM identity is required");
    project(&config,id)?;
    let pool=config.bindings.get(&v.pool).filter(|b|b.kind=="pool"&&binding_name(&b.name))
        .ok_or_else(||anyhow!("Incus requires an explicit valid pool binding"))?;
    let network=config.bindings.get(&v.network).filter(|b|matches!(b.kind.as_str(),"network"|"bridge")&&binding_name(&b.name))
        .ok_or_else(||anyhow!("invalid Incus network binding"))?;
    ensure!(v.disks.is_empty(),"Incus custom disk mapping is not implemented; root storage is managed by the pool");
    ensure!(s.depends_on.is_empty(),"Incus boot dependency integration is not implemented");
    ensure!(v.machine.is_empty(),"Incus machine-type overrides are unsupported");
    ensure!(v.firmware.is_empty()||v.firmware=="uefi","initial Incus VM profile requires UEFI");
    ensure!(matches!(v.architecture.as_str(),""|"x86_64"|"aarch64"),"unsupported Incus architecture");
    let size=if v.root_size_gib==0{20}else{v.root_size_gib};
    let settings=BTreeMap::from([
        ("limits.cpu".into(),v.cpus.to_string()),("limits.memory".into(),format!("{}MiB",v.memory_mib)),
        ("boot.autostart".into(),s.enabled.to_string()),("security.secureboot".into(),"true".into()),
        (ID.into(),id.into()),(IDENTITY.into(),v.uuid.clone()),(FINGERPRINT.into(),service_fingerprint(&config,id)?),
        (RUNTIME.into(),runtime_fingerprint(&config,id)?),
    ]);
    let root=BTreeMap::from([("type".into(),"disk".into()),("path".into(),"/".into()),
        ("pool".into(),pool.name.clone()),("size".into(),format!("{size}GiB"))]);
    let mut nic=BTreeMap::from([("type".into(),"nic".into()),("name".into(),"eth0".into()),("hwaddr".into(),v.mac.clone())]);
    if network.kind=="network"{nic.insert("network".into(),network.name.clone());}
    else{nic.insert("nictype".into(),"bridged".into());nic.insert("parent".into(),network.name.clone());}
    Ok(Definition{architecture:v.architecture.clone(),description:s.description.clone(),config:settings,
        devices:BTreeMap::from([("root".into(),root),("eth0".into(),nic)]),profiles:vec![],ephemeral:false})
}
fn encode(d:&Definition)->Result<Vec<u8>>{
    let data=format!("# Generated by server-man; edit the authoritative workload configuration.\n{}\n",serde_yaml::to_string(d)?);
    ensure!(data.len()<=super::MAX_CONFIG_BYTES,"Incus definition exceeds limit");
    Ok(data.into_bytes())
}
fn config_value<'a>(d:&'a Definition,key:&str)->&'a str{d.config.get(key).map(String::as_str).unwrap_or("")}
fn matches_definition(want:&Definition,actual:&Instance)->bool{
    let d=&actual.definition;
    !d.ephemeral&&d.profiles.is_empty()&&want.description==d.description
        &&(want.architecture.is_empty()||want.architecture==d.architecture)
        &&want.config.iter().all(|(k,v)|d.config.get(k)==Some(v))&&want.devices==d.devices
}
fn running_update_needs_stop(want:&Definition,actual:&Instance)->bool{
    let d=&actual.definition;
    d.ephemeral||!d.profiles.is_empty()||want.description!=d.description
        ||(!want.architecture.is_empty()&&want.architecture!=d.architecture)||want.devices!=d.devices
        ||["limits.cpu","limits.memory","security.secureboot",RUNTIME].iter().any(|key|config_value(want,key)!=config_value(d,key))
}
fn active_matches(want:&Definition,actual:&Instance)->bool{
    actual.status_code==102||config_value(&actual.definition,ACTIVE_RUNTIME)==config_value(want,RUNTIME)
}
fn merge_volatile(mut desired:Definition,actual:&Instance)->Definition{
    for (key,value) in &actual.definition.config{
        if key.starts_with("volatile.")||key==ACTIVE_RUNTIME{desired.config.insert(key.clone(),value.clone());}
    }
    if desired.architecture.is_empty(){desired.architecture=actual.definition.architecture.clone();}
    if actual.name.is_empty()||actual.status_code==102{
        let loaded=config_value(&desired,RUNTIME).to_owned();
        desired.config.insert(ACTIVE_RUNTIME.into(),loaded);
    }else if config_value(&desired,ACTIVE_RUNTIME).is_empty(){
        let loaded=config_value(&actual.definition,RUNTIME);
        desired.config.insert(ACTIVE_RUNTIME.into(),if store::valid_revision(loaded){loaded.into()}else{"unverified".into()});
    }
    desired
}
fn image_valid(image:&str)->bool{
    !image.is_empty()&&!image.starts_with('-')&&!image.contains([':','\0','\r','\n'])
}
fn check_transition(previous:&Config,desired:&Config,id:&str)->Result<()>{
    if !previous.services.contains_key(id)||!desired.services.contains_key(id){return Ok(())}
    let a=vm(previous,id)?;let b=vm(desired,id)?;
    ensure!(a.uuid==b.uuid&&a.mac==b.mac,"existing VM identity cannot be changed");
    ensure!(a.image==b.image,"changing an existing Incus image requires explicit reprovisioning");
    let size=|s:i64|if s==0{20}else{s};
    ensure!(size(b.root_size_gib)>=size(a.root_size_gib),"shrinking Incus root storage is unsupported");
    ensure!(project(previous,id)?==project(desired,id)?
        &&previous.bindings.get(&a.pool).map(|p|(&p.kind,&p.name))==desired.bindings.get(&b.pool).map(|p|(&p.kind,&p.name)),
        "changing Incus project/pool requires explicit storage relocation");
    Ok(())
}

#[path="incus_backup.rs"]
mod backup;
pub(crate) use backup::verify_archive as verify_backup_archive;

pub struct IncusStatus{
    pub state:&'static str,pub runtime_matches:bool,pub restart_required:bool,
}
pub struct IncusProvider{
    pub binary:PathBuf,
    pub runner:CommandRunner,
    qemu:PathBuf,
    directory:PathBuf,
}
impl IncusProvider{
    pub fn new(binary:PathBuf,runner:CommandRunner)->Self{
        let qemu=match std::env::consts::ARCH{"x86_64"=>"/usr/bin/qemu-system-x86_64","aarch64"=>"/usr/bin/qemu-system-aarch64",_=>""};
        Self{binary,runner,qemu:qemu.into(),directory:PathBuf::new()}
    }
    pub fn native_paths(mut self,base:PathBuf)->Self{self.directory=base.join("incus");self}
    #[cfg(test)]
    fn qemu_path(mut self,path:PathBuf)->Self{self.qemu=path;self}
    pub fn matches(&self,s:&Service)->bool{s.kind=="vm"&&s.backend=="incus"}
    fn directory(&self,id:&str)->PathBuf{self.directory.join(id)}
    async fn get<T:DeserializeOwned>(&self,project:Option<&str>,path:&str)->Result<T>{
        ensure!(self.binary.is_absolute(),"Incus client requires an absolute path");
        let mut path=format!("local:{path}");
        if let Some(project)=project{
            ensure!(binding_name(project),"invalid Incus project");
            let separator=if path.contains('?'){'&'}else{'?'};
            path.push(separator);path.push_str("project=");path.push_str(project);
        }
        let output=self.runner.capture_scoped(&self.binary,&["--force-local","query","--request","GET","--raw",&path],
            Duration::from_secs(5),super::MAX_CONFIG_BYTES,false,ENV,None,&[]).await
            .map_err(|_|anyhow!("Incus local query failed"))?;
        ensure!(output.status.success(),"Incus local query failed");
        #[derive(Deserialize)]
        struct Reply{#[serde(rename="type")]kind:String,error_code:i64,metadata:Value}
        let response:Reply=serde_json::from_slice(&output.stdout).map_err(|_|anyhow!("invalid or failed Incus response"))?;
        ensure!(response.kind=="sync"&&response.error_code==0,"invalid or failed Incus response");
        serde_json::from_value(response.metadata).map_err(|_|anyhow!("unsupported Incus response metadata"))
    }
    async fn run(&self,project:&str,definition:Option<&Definition>,args:&[&str],seconds:u64)->Result<()>{
        ensure!(self.binary.is_absolute()&&binding_name(project),"invalid Incus client or project");
        let mut argv=vec!["--force-local","--project",project];argv.extend_from_slice(args);
        let payload=definition.map(serde_json::to_vec).transpose()?;
        ensure!(payload.as_ref().is_none_or(|data|data.len()<=super::MAX_CONFIG_BYTES),"Incus definition exceeds limit");
        let output=self.runner.capture_scoped_input(&self.binary,&argv,Duration::from_secs(seconds),0,true,
            ENV,None,&[],payload.as_deref()).await.map_err(|_|anyhow!("Incus native operation failed"))?;
        ensure!(output.status.success(),"Incus native operation failed");
        Ok(())
    }
    pub async fn check_prerequisites(&self)->Result<()>{
        let _:BTreeMap<String,Value>=self.get(None,"/1.0").await.map_err(|_|anyhow!("The local Incus daemon is unavailable"))?;
        ensure!(self.qemu.is_absolute()&&self.qemu.is_file(),"The native QEMU executable required for Incus VMs is missing");
        let output=self.runner.capture_scoped(&self.qemu,&["-spice","help"],Duration::from_secs(3),64*1024,false,ENV,None,&[]).await
            .map_err(|_|anyhow!("QEMU SPICE support required for Incus VMs is unavailable"))?;
        let help=String::from_utf8_lossy(&output.stdout);
        ensure!((output.status.success()||output.status.code()==Some(1))&&help.starts_with("spice options:\n")
            &&help.contains("\n  unix=<bool")&&help.contains("\n  disable-ticketing=<bool"),
            "QEMU SPICE support required for Incus VMs is unavailable");
        Ok(())
    }
    pub async fn check_resource(&self,config:&Config,id:&str)->Result<()>{
        let want=render(config,id)?;let project=project(config,id)?;
        let _:BTreeMap<String,Value>=self.get(None,&format!("/1.0/projects/{project}")).await
            .map_err(|_|anyhow!("Incus project binding is unavailable"))?;
        let pool=&want.devices["root"]["pool"];
        let _:BTreeMap<String,Value>=self.get(Some(project),&format!("/1.0/storage-pools/{pool}")).await
            .map_err(|_|anyhow!("Incus storage-pool binding is unavailable"))?;
        if let Some(network)=want.devices["eth0"].get("network"){
            let _:BTreeMap<String,Value>=self.get(Some(project),&format!("/1.0/networks/{network}")).await
                .map_err(|_|anyhow!("Incus network binding is unavailable"))?;
        }
        Ok(())
    }
    async fn read(&self,config:&Config,id:&str)->Result<Option<Instance>>{
        ensure!(super::id_valid(id),"invalid Incus service ID");
        let project=project(config,id)?;let path=instance_path(id);
        let instances:Vec<String>=self.get(Some(project),"/1.0/instances").await?;
        ensure!(instances.len()<=1024,"Incus instance limit exceeded");
        let count=instances.iter().filter(|entry|entry.split('?').next()==Some(path.as_str())).count();
        ensure!(count<=1,"ambiguous Incus instance identity");
        if count==0{return Ok(None)}
        let actual:Instance=self.get(Some(project),&path).await?;
        ensure!(actual.name==instance_name(id)&&actual.kind=="virtual-machine"
            &&config_value(&actual.definition,ID)==id&&config_value(&actual.definition,IDENTITY)==vm(config,id)?.uuid,
            "Incus instance ownership does not match the desired definition");
        ensure!(actual.status_code>0,"Incus instance state is unavailable");
        Ok(Some(actual))
    }
    fn artifact(&self,config:&Config,id:&str)->Result<Option<Vec<u8>>>{
        if config.services.contains_key(id){Ok(Some(encode(&render(config,id)?)?))}else{Ok(None)}
    }
    fn check_change<'a>(&self,change:&'a Change)->Result<&'a str>{
        let id=change.resource.strip_prefix("service/").filter(|id|super::id_valid(id)).ok_or_else(||anyhow!("invalid Incus service ID"))?;
        ensure!(change.backend=="incus","resource belongs to another provider");
        Ok(id)
    }
    /// Reference transition check: reject unsafe edits at review, and
    /// repeat the same native guard in the serialized apply preflight.
    pub async fn review_transition(&self,change:&Change,previous:&Config,desired:&Config)->Result<()> {
        let id=self.check_change(change)?;
        check_transition(previous,desired,id)?;
        if self.runner.is_test_mode(){return Ok(())}
        if desired.services.contains_key(id) {
            let want=render(desired,id)?;
            if let Some(actual)=self.read(desired,id).await? {
                ensure!(actual.status_code==102||!running_update_needs_stop(&want,&actual),"{STOPPED_UPDATE}");
            } else {
                ensure!(image_valid(&vm(desired,id)?.image),"initial Incus creation requires a locally available image alias or fingerprint");
            }
        } else {
            ensure!(self.read(previous,id).await?.is_none(),"Incus definition removal requires an explicit data-preserving export workflow");
        }
        Ok(())
    }
    async fn preflight_state(&self,change:&Change,previous:&Config,desired:&Config,mode:ApplyMode)->Result<(Option<Vec<u8>>,Option<Vec<u8>>,Option<Instance>)>{
        let id=self.check_change(change)?;
        check_transition(previous,desired,id)?;
        let before=self.artifact(previous,id)?;let after=self.artifact(desired,id)?;
        let current=read_artifact(&self.directory(id),"instance.yaml")?;
        check_bytes(current.as_deref(),before.as_deref(),after.as_deref())?;
        let identity=if after.is_some(){desired}else{previous};
        let actual=self.read(identity,id).await?;
        if after.is_none(){
            ensure!(mode==ApplyMode::Now,"removal requires immediate apply");
            ensure!(actual.is_none(),"Incus definition removal requires an explicit data-preserving export workflow");
        }else{
            self.check_prerequisites().await?;
            self.check_resource(desired,id).await?;
            let want=render(desired,id)?;
            if let Some(a)=&actual{
                // No takeover of an unknown preexisting native instance.
                ensure!(before.is_some()||current.as_deref()==after.as_deref()||matches_definition(&want,a),
                    "preexisting Incus instance has no owned prior definition");
                if a.status_code!=102{ensure!(!running_update_needs_stop(&want,a),"{STOPPED_UPDATE}");}
            }else{
                ensure!(image_valid(&vm(desired,id)?.image),"initial Incus creation requires a locally available image alias or fingerprint");
            }
        }
        Ok((before,after,actual))
    }
    async fn native_apply(&self,change:&Change,previous:&Config,desired:&Config,mode:ApplyMode)->Result<()>{
        let id=self.check_change(change)?;
        let (before,after,actual)=self.preflight_state(change,previous,desired,mode).await?;
        if after.is_none(){
            // Absent failed creation: remove only the exact owned artifact.
            // Never delete an instance or a storage object.
            ensure!(self.read(previous,id).await?.is_none(),"Incus instance exists; data-preserving export is required");
            publish_artifact(&self.directory(id),"instance.yaml",before.as_deref(),None)?;
        }else{
            let want=render(desired,id)?;let project=project(desired,id)?;let target=target(id);
            if let Some(actual)=actual{
                if actual.status_code!=102{
                    ensure!(!running_update_needs_stop(&want,&actual),"{STOPPED_UPDATE}");
                    // Only boot and persistent fingerprint are mutable live.
                    // Bound the pair as in the reference and leave hardware
                    // and native volatile identity untouched.
                    tokio::time::timeout(Duration::from_secs(20),async{
                        for key in ["boot.autostart",FINGERPRINT]{
                            let value=config_value(&want,key);
                            if config_value(&actual.definition,key)!=value{
                                self.run(project,None,&["config","set",&target,&format!("{key}={value}")],20).await?;
                            }
                        }
                        Ok::<(),anyhow::Error>(())
                    }).await.map_err(|_|anyhow!("Incus boot policy update timed out"))??;
                }else{
                    let latest=self.read(desired,id).await?.filter(|a|a.status_code==102)
                        .ok_or_else(||anyhow!("{STOPPED_UPDATE}"))?;
                    let payload=merge_volatile(want,&latest);
                    self.run(project,Some(&payload),&["config","edit",&target],1200).await?;
                }
            }else{
                let payload=merge_volatile(want,&Instance::default());
                let image=format!("local:{}",vm(desired,id)?.image);
                self.run(project,Some(&payload),&["init",&image,&target,"--vm","--no-profiles"],1200).await?;
            }
            publish_artifact(&self.directory(id),"instance.yaml",before.as_deref(),after.as_deref())?;
        }
        ensure!(self.native_observe(change,previous,desired,mode).await?==ObservedResource::After,"Incus publication could not be verified");
        Ok(())
    }
    async fn native_observe(&self,change:&Change,previous:&Config,desired:&Config,_mode:ApplyMode)->Result<ObservedResource>{
        let id=self.check_change(change)?;check_transition(previous,desired,id)?;
        let identity=if desired.services.contains_key(id){desired}else{previous};
        let instance=self.read(identity,id).await?;
        let file=read_artifact(&self.directory(id),"instance.yaml")?;
        let matches=|config:&Config|->Result<bool>{
            if file!=self.artifact(config,id)?{return Ok(false)}
            if !config.services.contains_key(id){return Ok(instance.is_none())}
            let want=render(config,id)?;
            Ok(instance.as_ref().is_some_and(|a|matches_definition(&want,a)))
        };
        // Reference Incus runtime markers can be unverified after native boot.
        // Persistent publication is still exact; status reports restart needed.
        if matches(desired)?{Ok(ObservedResource::After)}
        else if matches(previous)?{Ok(ObservedResource::Before)}
        else{Ok(ObservedResource::Unknown)}
    }
    async fn verified(&self,config:&Config,id:&str)->Result<(Definition,Instance)>{
        let want=render(config,id)?;
        ensure!(read_artifact(&self.directory(id),"instance.yaml")?.as_deref()==Some(encode(&want)?.as_slice()),"Incus artifact differs from authoritative definition");
        let actual=self.read(config,id).await?.ok_or_else(||anyhow!("Incus VM does not exist"))?;
        ensure!(matches_definition(&want,&actual),"Incus persistent definition differs from authoritative state");
        Ok((want,actual))
    }
    pub async fn status_details(&self,config:&Config,id:&str)->Result<IncusStatus>{
        let (want,a)=self.verified(config,id).await?;
        let state=match a.status_code{102=>"stopped",103=>"running",_=>match a.status.to_ascii_lowercase().as_str(){"frozen"=>"paused","error"=>"failed","starting"=>"starting","stopping"=>"stopping",_=>"unknown"}};
        let runtime_matches=active_matches(&want,&a);
        Ok(IncusStatus{state,runtime_matches,restart_required:!runtime_matches})
    }
    pub async fn verify_boot_policy(&self,config:&Config,id:&str)->Result<()>{self.verified(config,id).await.map(|_|())}
    async fn start(&self,config:&Config,id:&str)->Result<()>{
        let (want,actual)=self.verified(config,id).await?;
        ensure!(actual.status_code==102,"Incus VM is not stopped");
        let project=project(config,id)?;let target=target(id);
        let payload=merge_volatile(want,&actual);
        self.run(project,Some(&payload),&["config","edit",&target],1200).await?;
        self.run(project,None,&["start",&target],1200).await?;
        ensure!(self.read(config,id).await?.is_some_and(|a|a.status_code==103),"Incus VM did not reach running state");
        Ok(())
    }
    pub async fn native_action(&self,config:&Config,id:&str,action:&str)->Result<String>{
        ensure!(matches!(action,"start"|"stop"|"restart"),"unsupported Incus action");
        if action=="start"{self.start(config,id).await?;}
        else{
            // Stop uses native ownership and the native saved runtime marker,
            // not the desired hardware of a possibly failed apply.
            let actual=self.read(config,id).await?.ok_or_else(||anyhow!("Incus VM does not exist"))?;
            let project=project(config,id)?;let target=target(id);
            if actual.status_code!=102{self.run(project,None,&["stop",&target,"--timeout","90"],100).await?;}
            let stopped=self.read(config,id).await?.filter(|a|a.status_code==102).ok_or_else(||anyhow!("Incus VM did not reach stopped state"))?;
            let loaded=config_value(&stopped.definition,RUNTIME);
            ensure!(store::valid_revision(loaded),"Incus VM stopped, but native runtime tracking is invalid; inspect its definition before restart");
            if config_value(&stopped.definition,ACTIVE_RUNTIME)!=loaded{
                self.run(project,None,&["config","set",&target,&format!("{ACTIVE_RUNTIME}={loaded}")],10).await
                    .map_err(|_|anyhow!("Incus VM stopped, but its runtime tracking could not be updated"))?;
            }
            if action=="restart"{self.start(config,id).await?;}
        }
        Ok(format!("Incus {action} completed"))
    }
    pub async fn native_logs(&self,config:&Config,id:&str,tail:usize)->Result<ManagedLogs>{
        ensure!((1..=2000).contains(&tail),"invalid log line limit");
        ensure!(self.read(config,id).await?.is_some(),"The managed Incus VM does not exist");
        let project=project(config,id)?;let target=target(id);
        let output=self.runner.capture_scoped(&self.binary,&["--force-local","--project",project,"info",&target,"--show-log"],
            Duration::from_secs(15),128*1024,true,ENV,None,&[]).await?;
        ensure!(output.status.success(),"managed Incus logs could not be read");
        let mut text=String::from_utf8_lossy(&output.stdout).into_owned();
        let mut truncated=output.truncated;
        let end=text.strip_suffix('\n').map_or(text.len(),str::len);
        if let Some(index)=text.as_bytes()[..end].iter().enumerate().rev().filter(|(_,b)|**b==b'\n').nth(tail-1).map(|(i,_)|i){
            text=text[index+1..].to_owned();truncated=true;
        }
        Ok(ManagedLogs{text,truncated,source:"incus-instance"})
    }
    pub async fn action(&self,id:&str,action:&str)->Result<String>{
        ensure!(self.runner.is_test_mode(),"native Incus action requires authoritative configuration");
        ensure!(super::id_valid(id)&&matches!(action,"start"|"stop"|"restart"),"unsupported Incus action");
        Ok(format!("SIMULATED: Incus {action} on {id}"))
    }
    pub async fn logs(&self,id:&str)->Result<String>{
        ensure!(self.runner.is_test_mode()&&super::id_valid(id),"native Incus logs require authoritative configuration");
        Ok(format!("SIMULATED: Incus logs for {id}"))
    }
}
impl ApplyProvider for IncusProvider{
    async fn preflight(&self,change:&Change,previous:&Config,desired:&Config)->Result<()>{
        self.preflight_mode(change,previous,desired,ApplyMode::Now).await
    }
    async fn apply(&self,change:&Change,previous:&Config,desired:&Config)->Result<()>{
        self.apply_mode(change,previous,desired,ApplyMode::Now).await
    }
    async fn preflight_mode(&self,change:&Change,previous:&Config,desired:&Config,mode:ApplyMode)->Result<()>{
        self.check_change(change)?;
        if self.runner.is_test_mode(){return Ok(())}
        self.preflight_state(change,previous,desired,mode).await.map(|_|())
    }
    async fn apply_mode(&self,change:&Change,previous:&Config,desired:&Config,mode:ApplyMode)->Result<()>{
        self.check_change(change)?;
        if self.runner.is_test_mode(){return Ok(())}
        self.native_apply(change,previous,desired,mode).await
    }
}
impl RecoveryProvider for IncusProvider{
    async fn observe(&self,change:&Change,previous:&Config,desired:&Config)->Result<ObservedResource>{
        self.observe_mode(change,previous,desired,ApplyMode::Now).await
    }
    async fn observe_mode(&self,change:&Change,previous:&Config,desired:&Config,mode:ApplyMode)->Result<ObservedResource>{
        if self.runner.is_test_mode(){return Ok(if change.operation==Operation::Create{ObservedResource::Before}else{ObservedResource::After})}
        self.native_observe(change,previous,desired,mode).await
    }
}

#[cfg(test)]
mod tests{
    use super::*;
    fn fixture()->Config{super::super::parse(br#"version: 1
bindings:
  lan: {kind: network, name: default}
  pool: {kind: pool, name: default}
  project: {kind: project, name: managed}
services:
  guest:
    kind: vm
    backend: incus
    description: private-definition-value
    vm:
      image: ubuntu-24.04
      uuid: "c3d699a2-921d-4009-8b63-1d0144fbb7dc"
      mac: "52:54:00:12:34:56"
      cpus: 2
      memory_mib: 1024
      network: lan
      pool: pool
      project: project
"#).unwrap()}
    #[test]
    fn reference_definition_and_live_update_guards(){
        let c=fixture();let d=render(&c,"guest").unwrap();
        assert!(d.profiles.is_empty());assert!(!d.ephemeral);
        assert_eq!(d.devices["root"]["size"],"20GiB");
        assert_eq!(d.devices["eth0"]["network"],"default");
        let mut actual=Instance{definition:merge_volatile(d.clone(),&Instance::default()),status_code:103,..Default::default()};
        actual.definition.config.insert("volatile.uuid".into(),"daemon-owned".into());
        let mut next=c.clone();next.services.get_mut("guest").unwrap().enabled=true;
        let boot=render(&next,"guest").unwrap();
        assert!(!running_update_needs_stop(&boot,&actual));
        assert_eq!(config_value(&merge_volatile(boot,&actual),"volatile.uuid"),"daemon-owned");
        next.services.get_mut("guest").unwrap().vm.as_mut().unwrap().memory_mib=2048;
        assert!(running_update_needs_stop(&render(&next,"guest").unwrap(),&actual));
        next.services.get_mut("guest").unwrap().vm.as_mut().unwrap().root_size_gib=10;
        assert!(check_transition(&c,&next,"guest").is_err());
        assert_ne!(instance_name("a_b"),instance_name("a-b"));
        assert!(instance_name(&"a".repeat(63)).len()<=63);
        assert!(!image_valid("images:ubuntu/24.04"));
    }
    #[tokio::test]
    async fn reference_native_scoping_stopped_edits_and_non_destructive_removal(){
        use std::{fs,path::Path,os::unix::fs::PermissionsExt};
        // This fixture needs only Python's standard library, not Incus or KVM.
        if !Path::new("/usr/bin/python3").is_file() {
            eprintln!("Skipping fake Incus executable test: /usr/bin/python3 not installed");
            return;
        }
        let tmp=tempfile::tempdir().unwrap();
        fs::set_permissions(tmp.path(),fs::Permissions::from_mode(0o700)).unwrap();
        let cli=tmp.path().join("incus");
        let qemu=tmp.path().join("qemu");
        let state=tmp.path().join("instance.json");
        let log=tmp.path().join("commands");
        let script=r#"#!/usr/bin/python3
import json, pathlib, sys
from urllib.parse import urlsplit, parse_qs
root = pathlib.Path(__file__).parent
state = root / "instance.json"
args = sys.argv[1:]
with (root / "commands").open("a") as f:
    f.write(json.dumps(args) + "\n")
assert args[0] == "--force-local"
assert "private-definition-value" not in " ".join(args)
actual = json.loads(state.read_text()) if state.exists() else None
if args[1] == "query":
    assert args[2:5] == ["--request", "GET", "--raw"]
    assert args[5].startswith("local:")
    url = urlsplit(args[5][len("local:"):])
    if url.path.startswith("/1.0/instances") or url.path.startswith("/1.0/storage-pools") or url.path.startswith("/1.0/networks"):
        assert parse_qs(url.query) == {"project": ["managed"]}
    if url.path == "/1.0/instances":
        metadata = ["/1.0/instances/sm-guest?project=managed"] if actual else []
    elif url.path == "/1.0/instances/sm-guest":
        assert actual
        metadata = actual
    elif url.path in ("/1.0", "/1.0/projects/managed", "/1.0/storage-pools/default", "/1.0/networks/default"):
        metadata = {}
    else:
        sys.exit(1)
    print(json.dumps({"type":"sync","error_code":0,"metadata":metadata}))
    sys.exit(0)
assert args[1:3] == ["--project", "managed"]
op = args[3:]
if op[0] == "init":
    assert op == ["init", "local:ubuntu-24.04", "local:sm-guest", "--vm", "--no-profiles"]
    assert actual is None
    actual = json.load(sys.stdin)
    assert actual["profiles"] == [] and not actual["ephemeral"]
    assert actual["devices"]["root"]["pool"] == "default"
    assert actual["description"] == "private-definition-value"
    actual.update(name="sm-guest", type="virtual-machine", status="Stopped", status_code=102)
    actual["config"]["volatile.uuid"] = "daemon-owned"
    actual["config"]["volatile.vm.rtc_offset"] = "123"
elif op[:2] == ["config", "edit"]:
    assert op[2:] == ["local:sm-guest"] and actual["status_code"] == 102
    payload = json.load(sys.stdin)
    assert payload["config"]["volatile.uuid"] == "daemon-owned"
    assert payload["config"]["volatile.vm.rtc_offset"] == "123"
    actual.update(payload)
elif op[:2] == ["config", "set"]:
    assert op[2] == "local:sm-guest"
    key, value = op[3].split("=", 1)
    assert key in ("boot.autostart", "user.server-man.fingerprint", "user.server-man.active-runtime")
    actual["config"][key] = value
elif op == ["start", "local:sm-guest"]:
    assert actual["status_code"] == 102
    actual.update(status="Running", status_code=103)
elif op == ["stop", "local:sm-guest", "--timeout", "90"]:
    if (root / "fail-stop").exists():
        sys.exit(1)
    actual.update(status="Stopped", status_code=102)
elif op == ["info", "local:sm-guest", "--show-log"]:
    print("first\nsecond\nthird")
    sys.exit(0)
else:
    sys.exit(1)
state.write_text(json.dumps(actual))
"#;
        fs::write(&cli,script).unwrap();
        fs::write(&qemu,"#!/bin/sh\n[ \"$*\" = '-spice help' ] || exit 2\nprintf 'spice options:\\n  unix=<bool (on/off)>\\n  disable-ticketing=<bool (on/off)>\\n'\nexit 1\n").unwrap();
        for path in [&cli,&qemu]{fs::set_permissions(path,fs::Permissions::from_mode(0o700)).unwrap();}
        let provider=IncusProvider::new(cli,CommandRunner::new(false)).native_paths(tmp.path().join("generated")).qemu_path(qemu);
        let c=fixture();let empty=Config{version:1,..Default::default()};
        let change=|operation,before:Option<&Config>,after:Option<&Config>|Change{
            resource:"service/guest".into(),backend:"incus".into(),operation,
            before:before.map(|c|service_fingerprint(c,"guest").unwrap()),
            after:after.map(|c|service_fingerprint(c,"guest").unwrap()),
        };
        let create=change(Operation::Create,None,Some(&c));
        provider.apply(&create,&empty,&c).await.unwrap();
        assert_eq!(provider.status_details(&c,"guest").await.unwrap().state,"stopped");
        provider.native_action(&c,"guest","start").await.unwrap();
        let mut next=c.clone();next.services.get_mut("guest").unwrap().enabled=true;
        let toggle=change(Operation::Update,Some(&c),Some(&next));
        let prior=fs::read_to_string(&log).unwrap();
        provider.apply_mode(&toggle,&c,&next,ApplyMode::NextStart).await.unwrap();
        let commands=fs::read_to_string(&log).unwrap();
        assert!(commands[prior.len()..].contains("boot.autostart=true"));
        assert!(!commands[prior.len()..].contains("\"edit\""));
        assert!(!commands[prior.len()..].contains("\"start\""));
        let previous=next.clone();next.services.get_mut("guest").unwrap().vm.as_mut().unwrap().memory_mib=2048;
        let update=change(Operation::Update,Some(&previous),Some(&next));
        for mode in [ApplyMode::Now,ApplyMode::NextStart]{
            assert!(provider.apply_mode(&update,&previous,&next,mode).await.is_err());
        }
        fs::write(tmp.path().join("fail-stop"),b"fail").unwrap();
        let prior=fs::read_to_string(&log).unwrap();
        assert!(provider.native_action(&previous,"guest","restart").await.is_err());
        assert!(!fs::read_to_string(&log).unwrap()[prior.len()..].contains("\"start\""));
        fs::remove_file(tmp.path().join("fail-stop")).unwrap();
        provider.native_action(&previous,"guest","stop").await.unwrap();
        provider.apply(&update,&previous,&next).await.unwrap();
        provider.native_action(&next,"guest","start").await.unwrap();
        assert!(provider.status_details(&next,"guest").await.unwrap().runtime_matches);
        let logs=provider.native_logs(&next,"guest",2).await.unwrap();
        assert_eq!(logs.text,"second\nthird\n");assert!(logs.truncated);
        let remove=change(Operation::Remove,Some(&next),None);
        assert!(provider.apply(&remove,&next,&empty).await.is_err());
        provider.native_action(&next,"guest","stop").await.unwrap();
        assert!(provider.apply(&remove,&next,&empty).await.is_err());
        let mut foreign:Value=serde_json::from_slice(&fs::read(&state).unwrap()).unwrap();
        foreign["config"][IDENTITY]=serde_json::json!("foreign");
        fs::write(&state,serde_json::to_vec(&foreign).unwrap()).unwrap();
        assert!(provider.native_action(&next,"guest","start").await.is_err());
        // A failed/absent instance may have its exact artifact removed.
        fs::remove_file(&state).unwrap();
        provider.apply(&remove,&next,&empty).await.unwrap();
        assert_eq!(provider.observe(&remove,&next,&empty).await.unwrap(),ObservedResource::After);
        let commands=fs::read_to_string(&log).unwrap();
        assert!(!commands.contains("\"delete\""));
        assert!(!commands.contains("\"--force\""));
        assert!(!commands.contains("private-definition-value"));
    }

    #[tokio::test]
    async fn simulation_lifecycle_and_logs(){
        let provider=IncusProvider::new("/nonexistent/incus".into(),CommandRunner::new(true));
        assert!(provider.action("demo","restart").await.unwrap().starts_with("SIMULATED:"));
        assert!(provider.logs("demo").await.unwrap().starts_with("SIMULATED:"));
        assert!(provider.action("demo","invalid").await.is_err());
    }
}
