//! Direct port of Server Man libvirt.go. Native commands always address a
//! verified persistent UUID on an explicitly selected local connection.
use std::{path::{Path,PathBuf},time::Duration};
use anyhow::{Result,anyhow,ensure};
use crate::command::CommandRunner;
use super::{ApplyMode,ApplyProvider,Change,Config,ObservedResource,Operation,RecoveryProvider,Service,
    artifacts::{check_bytes,read_artifact,publish_artifact},
    libvirt_render::{self as xml,Domain}};

pub struct LibvirtProvider{
    pub virsh:PathBuf,
    pub uri:Option<String>,
    pub runner:CommandRunner,
    validator:PathBuf,
    directory:PathBuf,
    storage_base:PathBuf,
}
pub struct LibvirtStatus{
    pub state:&'static str,
    pub runtime_matches:bool,
    pub restart_required:bool,
}
struct Observation{state:String,persistent:Domain,active:Option<Domain>,enabled:bool}
impl Observation{
    fn matches(&self,want:&Domain,enabled:bool)->bool{
        want.hardware_matches(&self.persistent)&&self.persistent.fingerprint==want.fingerprint
            &&self.persistent.runtime==want.runtime&&self.enabled==enabled
    }
    fn runtime_matches(&self,want:&Domain)->bool{
        self.active.as_ref().is_none_or(|active|want.hardware_matches(active)&&active.runtime==want.runtime)
    }
}
struct Candidate(PathBuf);
impl Candidate{
    fn new(data:&[u8])->Result<Self>{
        use std::os::unix::fs::DirBuilderExt;
        let path=std::env::temp_dir().join(format!("maestro-domain-validate-{}",uuid::Uuid::new_v4()));
        std::fs::DirBuilder::new().mode(0o700).create(&path)?;
        let candidate=Self(path);
        let directory=super::store::open_directory(&candidate.0)?;
        super::store::publish(&directory,"domain.xml",data,true)?;
        Ok(candidate)
    }
    fn file(&self)->PathBuf{self.0.join("domain.xml")}
}
impl Drop for Candidate{
    fn drop(&mut self){let _=std::fs::remove_file(self.file());let _=std::fs::remove_dir(&self.0);}
}
impl LibvirtProvider{
    pub fn new(virsh:PathBuf,runner:CommandRunner)->Self{
        Self{virsh,runner,uri:Some("qemu:///system".into()),validator:"/usr/bin/virt-xml-validate".into(),
            directory:PathBuf::new(),storage_base:PathBuf::new()}
    }
    pub fn native_paths(mut self,directory:PathBuf,storage_base:PathBuf)->Self{
        self.directory=directory.join("libvirt");
        self.storage_base=storage_base;self
    }
    pub fn validator(mut self,path:PathBuf)->Self{self.validator=path;self}
    pub fn matches(&self,service:&Service)->bool{service.kind=="vm"&&service.backend=="libvirt"}
    fn connection(&self)->Result<&str>{
        let uri=self.uri.as_deref().ok_or_else(||anyhow!("Libvirt connection is not configured"))?;
        ensure!(matches!(uri,"qemu:///system"|"qemu:///session"),"only local Libvirt connections are supported");
        ensure!(self.virsh.is_absolute(),"virsh must be an absolute path");
        Ok(uri)
    }
    async fn command(&self,args:&[&str],seconds:u64,limit:usize,discard:bool)->Result<Vec<u8>>{
        let mut argv=vec!["--connect",self.connection()?];argv.extend_from_slice(args);
        let output=self.runner.capture_scoped(&self.virsh,&argv,Duration::from_secs(seconds),limit,discard,
            &[("LC_ALL","C")],None,&["LIBVIRT_DEFAULT_URI","VIRSH_DEFAULT_CONNECT_URI"]).await?;
        ensure!(output.status.success(),"native Libvirt command failed");
        Ok(output.stdout)
    }
    pub async fn check_prerequisites(&self)->Result<()>{
        ensure!(self.validator.is_absolute()&&self.validator.is_file(),"virt-xml-validate is required");
        let uri=self.command(&["uri"],5,4096,false).await.map_err(|_|anyhow!("configured Libvirt connection is unavailable"))?;
        ensure!(String::from_utf8_lossy(&uri).trim()==self.connection()?,"Libvirt connection identity differs");
        let caps=self.command(&["domcapabilities","--virttype","kvm","--arch","x86_64","--machine","q35"],5,1024*1024,false).await
            .map_err(|_|anyhow!("configured Libvirt connection cannot provide the supported KVM guest profile"))?;
        ensure!(xml::valid_kvm_capabilities(&caps),"configured Libvirt connection returned unrecognized KVM capabilities");
        Ok(())
    }
    fn artifact(&self,config:&Config,id:&str)->Result<Option<Vec<u8>>>{
        if config.services.contains_key(id){Ok(Some(xml::render(config,id,&self.storage_base)?.into_bytes()))}else{Ok(None)}
    }
    fn directory(&self,id:&str)->PathBuf{self.directory.join(id)}
    fn file(&self,id:&str)->PathBuf{self.directory(id).join("domain.xml")}
    async fn validate_candidate(&self,data:&[u8])->Result<()>{
        ensure!(self.validator.is_absolute()&&self.validator.is_file(),"virt-xml-validate is required");
        let candidate=Candidate::new(data)?;
        let file=candidate.file();let file=file.to_str().ok_or_else(||anyhow!("invalid validation path"))?;
        let output=self.runner.capture_scoped(&self.validator,&[file,"domain"],Duration::from_secs(10),0,true,
            &[("LC_ALL","C")],None,&[]).await.map_err(|_|anyhow!("native Libvirt schema validation failed"))?;
        ensure!(output.status.success(),"native Libvirt schema validation failed");
        Ok(())
    }
    async fn exists(&self,id:&str,uuid:&str)->Result<bool>{
        let uuids=self.command(&["list","--all","--uuid"],5,256*1024,false).await.map_err(|_|anyhow!("Libvirt inventory is unavailable"))?;
        let uuids=std::str::from_utf8(&uuids)?;
        if uuids.split_whitespace().any(|line|line==uuid){return Ok(true)}
        let names=self.command(&["list","--all","--name"],5,256*1024,false).await.map_err(|_|anyhow!("Libvirt name inventory is unavailable"))?;
        ensure!(!std::str::from_utf8(&names)?.lines().any(|name|name.trim()==xml::domain_name(id)),"an existing Libvirt domain conflicts with this managed name");
        Ok(false)
    }
    async fn read_domain(&self,id:&str,uuid:&str,inactive:bool)->Result<Domain>{
        let mut args=vec!["dumpxml","--domain",uuid];if inactive{args.push("--inactive");}
        let data=self.command(&args,5,super::MAX_CONFIG_BYTES,false).await.map_err(|_|anyhow!("Libvirt domain definition cannot be read"))?;
        let domain=xml::parse_domain(&data)?;
        ensure!(domain.owned(id,uuid),"Libvirt domain is not owned by this managed definition");
        Ok(domain)
    }
    async fn state(&self,uuid:&str)->Result<String>{
        let state=self.command(&["domstate","--domain",uuid],5,4096,false).await.map_err(|_|anyhow!("Libvirt state cannot be read"))?;
        let state=std::str::from_utf8(&state)?.trim().to_owned();
        ensure!(matches!(state.as_str(),"shut off"|"running"|"blocked"|"paused"|"in shutdown"|"crashed"|"pmsuspended"|"idle"),"unrecognized Libvirt guest state");
        Ok(state)
    }
    async fn inspect(&self,id:&str,uuid:&str)->Result<Option<Observation>>{
        if !self.exists(id,uuid).await?{return Ok(None)}
        let persistent=self.read_domain(id,uuid,true).await?;
        let state=self.state(uuid).await?;
        let info=self.command(&["dominfo","--domain",uuid],5,64*1024,false).await.map_err(|_|anyhow!("Libvirt boot policy cannot be read"))?;
        let info=std::str::from_utf8(&info)?;
        // Transient guests are not owned persistent definitions.
        ensure!(!info.lines().any(|line|line.split_once(':').is_some_and(|(k,v)|k.trim()=="Persistent"&&v.trim()!="yes")),"guest is not persistent");
        let enabled=match info.lines().find_map(|line|line.split_once(':').filter(|(k,_)|k.trim()=="Autostart").map(|(_,v)|v.trim())){
            Some("enable"|"enabled")=>true,Some("disable"|"disabled")=>false,_=>return Err(anyhow!("Libvirt boot policy cannot be read")),
        };
        let active=if state!="shut off"{Some(self.read_domain(id,uuid,false).await?)}else{None};
        Ok(Some(Observation{persistent,state,enabled,active}))
    }
    fn identity<'a>(&self,id:&str,previous:&'a Config,desired:&'a Config)->Result<&'a str>{
        let old=previous.services.get(id);let next=desired.services.get(id);
        let identity=next.or(old).ok_or_else(||anyhow!("VM identity unavailable"))?;
        ensure!(self.matches(identity),"resource belongs to another provider");
        let vm=identity.vm.as_ref().ok_or_else(||anyhow!("VM identity unavailable"))?;
        ensure!(!vm.uuid.is_empty()&&uuid::Uuid::parse_str(&vm.uuid).is_ok(),"persistent VM identity is required");
        if let (Some(old),Some(next))=(old,next){
            let a=old.vm.as_ref().ok_or_else(||anyhow!("previous VM identity unavailable"))?;
            let b=next.vm.as_ref().ok_or_else(||anyhow!("VM identity unavailable"))?;
            ensure!(self.matches(old)&&self.matches(next)&&a.uuid==b.uuid&&a.mac==b.mac,"existing VM identity cannot be changed");
        }
        Ok(&vm.uuid)
    }
    fn check_change<'a>(&self,change:&'a Change)->Result<&'a str>{
        let id=change.resource.strip_prefix("service/").filter(|id|super::id_valid(id)).ok_or_else(||anyhow!("invalid Libvirt service ID"))?;
        ensure!(change.backend=="libvirt","resource belongs to another provider");Ok(id)
    }
    async fn preflight_state(&self,change:&Change,previous:&Config,desired:&Config,mode:ApplyMode)->Result<(Option<Vec<u8>>,Option<Vec<u8>>,Option<Observation>)>{
        let id=self.check_change(change)?;
        let uuid=self.identity(id,previous,desired)?;
        let before=self.artifact(previous,id)?;let after=self.artifact(desired,id)?;
        let old=before.as_deref().map(xml::parse_domain).transpose()?;
        let next=after.as_deref().map(xml::parse_domain).transpose()?;
        self.check_prerequisites().await?;
        let actual=read_artifact(&self.directory(id),"domain.xml")?;
        check_bytes(actual.as_deref(),before.as_deref(),after.as_deref())?;
        let obs=self.inspect(id,uuid).await?;
        if let Some(ref obs)=obs{
            // Exact managed hardware + full reference fingerprint, not just a
            // name or metadata marker, protects externally edited guests.
            let known=|domain:&Domain|domain.hardware_matches(&obs.persistent)
                &&domain.fingerprint==obs.persistent.fingerprint&&domain.runtime==obs.persistent.runtime;
            ensure!(old.as_ref().is_some_and(known)||next.as_ref().is_some_and(known),"persistent Libvirt definition differs from reviewed state");
            ensure!(before.is_some()||actual.as_deref()==after.as_deref(),"preexisting domain has no owned prior artifact");
            if after.is_none(){
                ensure!(obs.state=="shut off","shut down VM before removing its definition");
            }else if mode==ApplyMode::Now&&obs.active.is_some(){
                ensure!(obs.runtime_matches(next.as_ref().unwrap()),"running VM changes require shutdown/start or Save for next start");
            }
        }
        if after.is_none(){ensure!(mode==ApplyMode::Now,"removing a VM requires immediate apply");}
        if let Some(data)=&after{self.validate_candidate(data).await?;}
        Ok((before,after,obs))
    }
    async fn native_apply(&self,change:&Change,previous:&Config,desired:&Config,mode:ApplyMode)->Result<()>{
        let id=self.check_change(change)?;let uuid=self.identity(id,previous,desired)?;
        let (before,after,obs)=self.preflight_state(change,previous,desired,mode).await?;
        if after.is_none(){
            if obs.is_some(){
                // Recheck current state immediately before undefine. Never
                // force-stop or request storage, NVRAM or TPM deletion.
                ensure!(self.state(uuid).await?=="shut off","shut down VM before removing its definition");
                self.command(&["undefine","--domain",uuid,"--keep-nvram","--keep-tpm"],60,0,true).await
                    .map_err(|_|anyhow!("domain removal failed; disks were not requested for deletion"))?;
            }
            ensure!(!self.exists(id,uuid).await?,"Libvirt domain remains registered");
            publish_artifact(&self.directory(id),"domain.xml",before.as_deref(),None)?;
        }else{
            publish_artifact(&self.directory(id),"domain.xml",before.as_deref(),after.as_deref())?;
            let file=self.file(id);let file=file.to_str().ok_or_else(||anyhow!("invalid Libvirt definition path"))?;
            self.command(&["define",file,"--validate"],60,0,true).await.map_err(|_|anyhow!("persistent Libvirt definition failed"))?;
            let mut args=vec!["autostart","--domain",uuid];
            if !desired.services[id].enabled{args.push("--disable");}
            self.command(&args,5,0,true).await.map_err(|_|anyhow!("Libvirt boot policy update failed"))?;
        }
        ensure!(self.native_observe(change,previous,desired,mode).await?==ObservedResource::After,"Libvirt publication could not be verified");
        Ok(())
    }
    async fn native_observe(&self,change:&Change,previous:&Config,desired:&Config,mode:ApplyMode)->Result<ObservedResource>{
        let id=self.check_change(change)?;let uuid=self.identity(id,previous,desired)?;
        let obs=self.inspect(id,uuid).await?;
        let actual=read_artifact(&self.directory(id),"domain.xml")?;
        let matches=|config:&Config,mode:ApplyMode|->Result<bool>{
            let artifact=self.artifact(config,id)?;
            if actual.as_deref()!=artifact.as_deref(){return Ok(false)}
            match (&obs,artifact){
                (None,None)=>Ok(true),
                (Some(obs),Some(data))=>{
                    let want=xml::parse_domain(&data)?;
                    Ok(obs.matches(&want,config.services[id].enabled)&&(mode==ApplyMode::NextStart||obs.runtime_matches(&want)))
                },
                _=>Ok(false),
            }
        };
        if matches(desired,mode)?{Ok(ObservedResource::After)}
        else if matches(previous,ApplyMode::NextStart)?{Ok(ObservedResource::Before)}
        else{Ok(ObservedResource::Unknown)}
    }
    async fn verified(&self,config:&Config,id:&str)->Result<(Domain,Observation)>{
        ensure!(super::id_valid(id),"invalid Libvirt service");
        let data=self.artifact(config,id)?.ok_or_else(||anyhow!("unknown Libvirt VM"))?;
        ensure!(read_artifact(&self.directory(id),"domain.xml")?.as_deref()==Some(data.as_slice()),"Libvirt artifact differs from authoritative definition");
        let want=xml::parse_domain(&data)?;
        let obs=self.inspect(id,&want.uuid).await?.ok_or_else(||anyhow!("Libvirt VM is not registered"))?;
        ensure!(obs.matches(&want,config.services[id].enabled),"persistent Libvirt definition or boot policy differs");
        Ok((want,obs))
    }
    pub async fn status_details(&self,config:&Config,id:&str)->Result<LibvirtStatus>{
        let (want,obs)=self.verified(config,id).await?;
        let state=match obs.state.as_str(){"running"=>"running","shut off"=>"stopped","paused"=>"paused","crashed"=>"failed","in shutdown"=>"stopping",_=>"unknown"};
        let runtime_matches=obs.runtime_matches(&want);
        Ok(LibvirtStatus{state,runtime_matches,restart_required:!runtime_matches})
    }
    pub async fn verify_boot_policy(&self,config:&Config,id:&str)->Result<()>{
        self.verified(config,id).await.map(|_|())
    }
    async fn run_start(&self,id:&str,uuid:&str)->Result<()>{
        self.command(&["start","--domain",uuid],60,0,true).await.map_err(|_|anyhow!("Libvirt start failed"))?;
        ensure!(self.state(uuid).await?=="running","VM did not reach running state");
        self.read_domain(id,uuid,false).await?;
        Ok(())
    }
    pub async fn native_action(&self,config:&Config,id:&str,action:&str)->Result<String>{
        ensure!(matches!(action,"start"|"stop"|"restart"),"unsupported Libvirt action");
        let (want,obs)=self.verified(config,id).await?;let uuid=&want.uuid;
        if action=="start"{
            if obs.state!="running"{self.run_start(id,uuid).await?;}
        }else{
            if obs.state!="shut off"{
                self.command(&["shutdown","--domain",uuid],5,0,true).await.map_err(|_|anyhow!("guest shutdown request failed"))?;
                let result=tokio::time::timeout(Duration::from_secs(90),async{
                    loop{
                        if self.state(uuid).await?=="shut off"{return Ok::<(),anyhow::Error>(());}
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                }).await.map_err(|_|anyhow!("guest did not shut down; force-stop was not requested"))?;
                result?;
            }
            if action=="restart"{
                self.verified(config,id).await?;
                self.run_start(id,uuid).await?;
            }
        }
        let status=self.status_details(config,id).await?;
        ensure!(if action=="stop"{status.state=="stopped"}else{status.state=="running"&&status.runtime_matches},"guest did not reach the requested runtime state");
        Ok(format!("Libvirt {action} completed"))
    }
    pub async fn action(&self,service_id:&str,action:&str)->Result<String>{
        ensure!(self.runner.is_test_mode(),"native Libvirt action requires authoritative configuration");
        ensure!(super::id_valid(service_id)&&matches!(action,"start"|"stop"|"restart"|"shutdown"|"reboot"),"unsupported Libvirt action");
        Ok(format!("SIMULATED: Libvirt {action} on {service_id}"))
    }
}

impl ApplyProvider for LibvirtProvider{
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
impl RecoveryProvider for LibvirtProvider{
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
    #[tokio::test]
    async fn lifecycle_does_not_require_native_capture_in_simulation(){
        let provider=LibvirtProvider::new("/nonexistent/virsh".into(),CommandRunner::new(true));
        assert!(provider.action("demo","restart").await.unwrap().starts_with("SIMULATED:"));
        assert!(provider.action("demo","invalid").await.is_err());
    }
}
