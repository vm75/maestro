//! Native Compose adapter translated from Server Man compose_adapter.go and
//! compose_render.go. Both engines share the same lifecycle and ownership path.
use super::*;
use crate::core::declarative::{ApplyMode, composite::ManagedLogs, plan::service_fingerprint, quadlet::ContainerMemberStatus, revision};
use serde_json::Value;
use std::collections::BTreeSet;

const ENV: &[(&str,&str)] = &[
    ("COMPOSE_ANSI","never"),("COMPOSE_DISABLE_ENV_FILE","1"),
    ("PODMAN_COMPOSE_WARNING_LOGS","false"),
];
const REMOVED: &[&str] = &[
    "DOCKER_CONTEXT","DOCKER_HOST","DOCKER_TLS_VERIFY","DOCKER_CERT_PATH",
    "CONTAINER_HOST","CONTAINER_CONNECTION","PODMAN_CONNECTION",
    "COMPOSE_FILE","COMPOSE_PROJECT_NAME","COMPOSE_ENV_FILES","COMPOSE_PROFILES",
    "PODMAN_COMPOSE_PROVIDER",
];
pub(super) fn project(id: &str) -> String { format!("sm-{id}-{}",&revision(id.as_bytes())[..12]) }
fn unit(id: &str) -> String { format!("sm-compose-{id}.service") }

fn runtime_fingerprint(config: &Config,id: &str) -> Result<String> {
    let mut copy = config.clone();
    let service = copy.services.get_mut(id).ok_or_else(|| anyhow!("unknown Compose service"))?;
    // Boot policy and display metadata do not change a running container.
    service.enabled = false;
    service.name.clear();
    service.description.clear();
    service_fingerprint(&copy,id)
}

fn args(file: &Path,id: &str,action: &[&str]) -> Result<Vec<String>> {
    let file = file.to_str().ok_or_else(|| anyhow!("non-UTF-8 Compose path"))?;
    let mut result = vec!["compose".into(),"--project-name".into(),project(id),
        "--env-file".into(),"/dev/null".into(),"--file".into(),file.into()];
    result.extend(action.iter().map(|arg| (*arg).into()));
    Ok(result)
}

fn env(engine: &str) -> Vec<(&'static str,&'static str)> {
    let mut result = ENV.to_vec();
    if engine == "podman" {
        result.push(("PODMAN_COMPOSE_PROVIDER","podman-compose"));
        result.push(("CONTAINER_HOST",""));
        result.push(("CONTAINER_CONNECTION",""));
    } else {
        result.push(("DOCKER_HOST","unix:///var/run/docker.sock"));
        result.push(("DOCKER_CONTEXT",""));
        result.push(("DOCKER_TLS_VERIFY",""));
        result.push(("DOCKER_CERT_PATH",""));
    }
    result
}

fn unit_arg(value: &str) -> Result<String> {
    ensure!(!value.chars().any(char::is_control),"invalid systemd argument");
    Ok(format!("\"{}\"",value.replace('\\',"\\\\").replace('"',"\\\"").replace('%',"%%").replace('$',"$$")))
}

fn render_boot(config: &Config,id: &str,binary: &Path,file: &Path) -> Result<Vec<u8>> {
    let service = config.services.get(id).ok_or_else(|| anyhow!("unknown Compose service"))?;
    ensure!(binary.is_absolute() && file.is_absolute(),"Compose bootstrap paths must be absolute");
    let directory = file.parent().and_then(Path::to_str).ok_or_else(|| anyhow!("invalid Compose directory"))?;
    ensure!(directory.trim()==directory && !directory.chars().any(|c| c.is_control() || matches!(c,'\\'|'"'|'\'')),"Compose working directory contains unsupported whitespace or path escapes");
    let mut result = format!("{HEADER}\n[Unit]\nDescription=Maestro Compose boot startup\n");
    if service.engine == "podman" {
        result.push_str("Wants=podman-user-wait-network-online.service\nAfter=podman-user-wait-network-online.service\n");
    }
    for dependency in &service.depends_on {
        let other = config.services.get(dependency).ok_or_else(|| anyhow!("missing Compose dependency"))?;
        ensure!(other.engine == service.engine && other.backend == "compose","Compose dependencies require the same engine/backend");
        result.push_str(&format!("Requires={}\nAfter={}\n",unit(dependency),unit(dependency)));
    }
    result.push_str("\n[Service]\nType=oneshot\nRemainAfterExit=yes\nTimeoutStartSec=900\n");
    for key in REMOVED { result.push_str(&format!("UnsetEnvironment={key}\n")); }
    // UnsetEnvironment is applied last by systemd; do not unset variables we
    // explicitly select. Emit only non-overlapping unset entries.
    result = result.lines().filter(|line| {
        !line.strip_prefix("UnsetEnvironment=").is_some_and(|key| env(&service.engine).iter().any(|(k,_)| *k==key))
    }).map(|line| format!("{line}\n")).collect();
    for (key,value) in env(&service.engine) { result.push_str(&format!("Environment={}\n",unit_arg(&format!("{key}={value}"))?)); }
    result.push_str(&format!("WorkingDirectory={}\nExecStart=",directory.replace('%',"%%")));
    let mut argv = vec![binary.to_str().ok_or_else(|| anyhow!("invalid engine path"))?.to_owned()];
    if service.engine == "podman" { argv.push("--remote=false".into()); }
    argv.extend(args(file,id,&["up","--detach","--no-build"])?);
    result.push_str(&argv.iter().map(|a|unit_arg(a)).collect::<Result<Vec<_>>>()?.join(" "));
    result.push_str("\n\n[Install]\nWantedBy=default.target\n");
    Ok(result.into_bytes())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Member { name: String, fingerprint: String, state: String, native_name: String }

fn labels(value: &Value) -> Result<BTreeMap<String,String>> {
    if let Some(map) = value.as_object() {
        return map.iter().map(|(key,value)| Ok((key.clone(),value.as_str().ok_or_else(||anyhow!("invalid native label"))?.into())) .collect();
    }
    let text = value.as_str().ok_or_else(|| anyhow!("native labels missing"))?;
    let mut result = BTreeMap::new();
    for pair in text.split(',') {
        if let Some((key,value))=pair.split_once('=') {
            ensure!(result.insert(key.to_owned(),value.to_owned()).is_none(),"duplicate native label");
        }
    }
    Ok(result)
}

fn parse_inventory(data: &[u8],id: &str) -> Result<BTreeMap<String,Member>> {
    let text = std::str::from_utf8(data)?.trim();
    let rows = if text.starts_with('[') { serde_json::from_str::<Vec<Value>>(text)? }
        else { text.lines().map(serde_json::from_str).collect::<std::result::Result<Vec<Value>,_>>()? };
    ensure!(rows.len()<=256,"Compose inventory exceeds limit");
    let mut members = BTreeMap::new();
    for row in rows {
        let labels=labels(row.get("Labels").or_else(||row.get("labels")).ok_or_else(||anyhow!("native labels missing"))?)?;
        ensure!(labels.get("io.server-man.service").is_some_and(|v|v==id),"foreign container in Compose project");
        let name=labels.get("io.server-man.container").filter(|v|super::super::id_valid(v)).ok_or_else(||anyhow!("missing native member identity"))?.clone();
        let fingerprint=labels.get("io.server-man.fingerprint").filter(|v|store::valid_revision(v)).ok_or_else(||anyhow!("invalid native runtime fingerprint"))?.clone();
        let expected_project=project(id);
        ensure!(["com.docker.compose.project","io.podman.compose.project"].iter().any(|key|labels.get(*key)==Some(&expected_project)),"native project identity differs");
        ensure!(["com.docker.compose.service","io.podman.compose.service"].iter().any(|key|labels.get(*key)==Some(&name)),"native Compose member identity differs");
        let native_name=row.get("Names").or_else(||row.get("names")).ok_or_else(||anyhow!("missing native name"))?;
        let native_name=if let Some(name)=native_name.as_str(){name.to_owned()}else{
            let names=native_name.as_array().filter(|v|v.len()==1).ok_or_else(||anyhow!("ambiguous native name"))?;
            names[0].as_str().ok_or_else(||anyhow!("invalid native name"))?.into()
        };
        let state=row.get("State").or_else(||row.get("state")).and_then(Value::as_str).ok_or_else(||anyhow!("missing native state"))?.to_ascii_lowercase();
        ensure!(matches!(state.as_str(),"running"|"created"|"configured"|"exited"|"stopped"|"paused"|"restarting"|"dead"),"unknown native state");
        ensure!(members.insert(name.clone(),Member{name,fingerprint,state,native_name}).is_none(),"duplicate native member");
    }
    Ok(members)
}

fn expected_member(config: &Config,id: &str,member: &Member) -> bool {
    config.services.get(id).and_then(|s|s.containers.get(&member.name)).is_some_and(|container|{
        let name=if container.container_name.is_empty(){format!("maestro-{id}-{}",member.name)}else{container.container_name.clone()};
        member.native_name==name
    })
}

fn properties(bytes: &[u8]) -> Result<BTreeMap<String,String>> {
    let result: BTreeMap<String,String>=BTreeMap::from_iter(std::str::from_utf8(bytes)?.lines().filter_map(|line|line.split_once('=')).map(|(k,v)|(k.into(),v.into())));
    ensure!(result.contains_key("LoadState") && result.contains_key("ActiveState"),"cannot inspect Compose boot unit");
    Ok(result)
}

#[derive(Debug)]
pub struct ComposeStatus {
    pub state: &'static str,
    pub runtime_matches: bool,
    pub restart_required: bool,
    pub members: Vec<ContainerMemberStatus>,
}

struct Artifacts { compose: Vec<u8>, boot: Vec<u8> }

fn read_artifact(directory: &Path,name: &str) -> Result<Option<Vec<u8>>> {
    let Some(dir)=optional(store::open_directory(directory))? else{return Ok(None)};
    optional(store::read_private(&dir,name,super::super::MAX_CONFIG_BYTES))
}

fn check_bytes(actual: Option<&[u8]>,before: Option<&[u8]>,after: Option<&[u8]>) -> Result<()> {
    ensure!(actual==before || actual==after,"Compose artifact differs from the reviewed before and after definitions");
    Ok(())
}

// Open every newly created directory relative to its trusted parent. The
// existing store helpers reject symlinks and foreign or non-private ownership.
fn ensure_dir(path: &Path) -> Result<File> {
    if let Some(dir)=optional(store::open_directory(path))? {return Ok(dir);}
    let parent=path.parent().ok_or_else(||anyhow!("missing artifact parent"))?;
    let name=path.file_name().and_then(|n|n.to_str()).ok_or_else(||anyhow!("invalid artifact name"))?;
    let dir=ensure_dir(parent)?;
    match unix_fs::mkdirat(&dir,name,Mode::from_bits_truncate(0o700)) {
        Ok(())=>dir.sync_all()?,Err(Errno::EXIST)=>{},Err(error)=>return Err(error.into()),
    }
    store::open_child_directory(&dir,name)
}

fn publish_artifact(directory: &Path,name: &str,before: Option<&[u8]>,after: Option<&[u8]>) -> Result<()> {
    let dir=if after.is_some(){ensure_dir(directory)?}else{
        let Some(dir)=optional(store::open_directory(directory))? else {return Ok(())};
        dir
    };
    let lock=store::LockedDirectory::try_acquire(dir)?;
    let actual=optional(store::read_private(&lock,name,super::super::MAX_CONFIG_BYTES))?;
    check_bytes(actual.as_deref(),before,after)?;
    if actual.as_deref()==after{return Ok(())}
    if let Some(data)=after {store::publish(&lock,name,data,actual.is_none())?;}
    else {unix_fs::unlinkat(&lock,name,AtFlags::empty())?;lock.sync_all()?;}
    Ok(())
}

struct TempDefinition(PathBuf);
impl TempDefinition {
    fn new(data: &[u8]) -> Result<Self> {
        let path=std::env::temp_dir().join(format!("maestro-compose-validate-{}",uuid::Uuid::new_v4()));
        fs::DirBuilder::new().mode(0o700).create(&path)?;
        let result=Self(path);
        let dir=store::open_directory(&result.0)?;
        store::publish(&dir,"compose.yaml",data,true)?;
        Ok(result)
    }
}
impl Drop for TempDefinition {fn drop(&mut self){let _=fs::remove_file(self.0.join("compose.yaml"));let _=fs::remove_dir(&self.0);}}

impl ComposeProvider {
    fn unit_file(&self,id:&str)->PathBuf{self.unit_dir.join(id).join(unit(id))}
    fn artifacts(&self,config:&Config,id:&str)->Result<Option<Artifacts>>{
        let Some(service)=config.services.get(id) else{return Ok(None)};
        ensure!(self.matches(service),"resource belongs to another Compose provider");
        let compose=render_compose(config,id,&runtime_fingerprint(config,id)?,&self.storage_base)?.into_bytes();
        let boot=render_boot(config,id,&self.binary,&self.compose_file(id))?;
        ensure!(compose.len()<=super::super::MAX_CONFIG_BYTES && boot.len()<=super::super::MAX_CONFIG_BYTES,"Compose artifacts exceed limit");
        Ok(Some(Artifacts{compose,boot}))
    }
    async fn engine_command(&self,argv:&[&str],cwd:Option<&Path>,seconds:u64,limit:usize,tail:bool)->Result<crate::command::CapturedOutput>{
        ensure!(self.binary.is_absolute(),"Compose engine path must be absolute");
        let mut args=Vec::new();
        if self.engine=="podman"{args.push("--remote=false");}
        args.extend_from_slice(argv);
        let result=self.runner.capture_scoped(&self.binary,&args,Duration::from_secs(seconds),limit,tail,&env(&self.engine),cwd,REMOVED).await?;
        ensure!(result.status.success(),"native Compose engine command failed");
        Ok(result)
    }
    async fn compose_command(&self,id:&str,file:&Path,action:&[&str],seconds:u64,limit:usize,tail:bool)->Result<crate::command::CapturedOutput>{
        let argv=args(file,id,action)?;
        let argv=argv.iter().map(String::as_str).collect::<Vec<_>>();
        self.engine_command(&argv,file.parent(),seconds,limit,tail).await
    }
    async fn user_command(&self,argv:&[&str],seconds:u64)->Result<Vec<u8>>{
        ensure!(self.systemctl.is_absolute(),"systemctl path must be absolute");
        let mut args=vec!["--user","--no-pager","--no-ask-password"];args.extend_from_slice(argv);
        let result=self.runner.capture_scoped(&self.systemctl,&args,Duration::from_secs(seconds),128*1024,false,&[("SYSTEMD_COLORS","0"),("SYSTEMD_URLIFY","0")],None,&[]).await?;
        ensure!(result.status.success(),"user systemd command failed");
        Ok(result.stdout)
    }
    async fn boot(&self,id:&str)->Result<BTreeMap<String,String>>{
        properties(&self.user_command(&["show",&unit(id),"--property=LoadState,ActiveState,UnitFileState,NeedDaemonReload,FragmentPath,DropInPaths"],5).await?)
    }
    fn owned_boot(&self,id:&str,fields:&BTreeMap<String,String>)->bool{
        let Some(path)=fields.get("FragmentPath").map(Path::new).filter(|p|p.is_absolute()) else{return false};
        fs::canonicalize(path).ok().zip(fs::canonicalize(self.unit_file(id)).ok()).is_some_and(|(a,b)|a==b)
            && fields.get("DropInPaths").is_some_and(String::is_empty)
    }
    fn absent_boot(fields:&BTreeMap<String,String>)->bool{
        fields.get("LoadState").is_some_and(|s|s=="not-found")&&fields.get("ActiveState").is_some_and(|s|s=="inactive")
    }
    fn boot_matches(&self,id:&str,config:&Config,fields:&BTreeMap<String,String>)->bool{
        let Some(service)=config.services.get(id) else{return Self::absent_boot(fields)};
        fields.get("LoadState").is_some_and(|s|s=="loaded")&&self.owned_boot(id,fields)
            &&fields.get("NeedDaemonReload").is_some_and(|s|s=="no")
            &&fields.get("UnitFileState").is_some_and(|s|if service.enabled{s=="enabled"}else{matches!(s.as_str(),"linked"|"disabled"|"static")})
    }
    async fn inventory(&self,id:&str)->Result<BTreeMap<String,Member>>{
        ensure!(super::super::id_valid(id),"invalid Compose service");
        let by_owner=format!("label=io.server-man.service={id}");
        let project=project(id);
        let by_project=format!("label=com.docker.compose.project={project}");
        let mut members=parse_inventory(&self.engine_command(&["ps","--all","--filter",&by_owner,"--format","json"],None,5,256*1024,false).await?.stdout,id)?;
        let mut filters=vec![by_project];
        if self.engine=="podman"{filters.push(format!("label=io.podman.compose.project={project}"));}
        for filter in filters {
            let rows=parse_inventory(&self.engine_command(&["ps","--all","--filter",&filter,"--format","json"],None,5,256*1024,false).await?.stdout,id)?;
            for (name,member) in rows{
                if let Some(old)=members.get(&name){ensure!(old==&member,"native Compose inventory changed during inspection");}
                else{members.insert(name,member);}
            }
        }
        Ok(members)
    }
    async fn validate_candidate(&self,config:&Config,id:&str)->Result<()>{
        let art=self.artifacts(config,id)?.ok_or_else(||anyhow!("missing Compose definition"))?;
        let temp=TempDefinition::new(&art.compose)?;
        self.compose_command(id,&temp.0.join("compose.yaml"),&["config"],15,0,true).await
            .map_err(|_|anyhow!("installed Compose provider rejected the generated definition"))?;
        Ok(())
    }
    pub async fn check_prerequisites(&self)->Result<()>{
        self.engine_command(&["compose","version"],None,4,64*1024,false).await?;
        if self.engine=="podman"{
            let info=self.engine_command(&["info","--format","{{.Host.Security.Rootless}}"],None,4,64*1024,false).await?;
            ensure!(String::from_utf8_lossy(&info.stdout).trim()=="true","local rootless Podman is unavailable");
        }else{self.engine_command(&["info","--format","json"],None,4,256*1024,false).await?;}
        self.user_command(&["show","--property=Version"],4).await?;
        Ok(())
    }
    pub async fn check_autostart(&self)->Result<()> {
        if self.engine=="podman" {
            let out=self.user_command(&["show","podman-user-wait-network-online.service","--property=LoadState","--value"],3).await?;
            ensure!(String::from_utf8_lossy(&out).trim()=="loaded","Podman's boot network wait unit is unavailable");
        } else {
            let out=self.runner.capture_status(&self.systemctl,&["--no-pager","--no-ask-password","show","docker.socket","--property=LoadState,UnitFileState"],Duration::from_secs(3),64*1024).await?;
            ensure!(out.status.success(),"Docker boot socket could not be checked");
            let text=String::from_utf8_lossy(&out.stdout);
            ensure!(text.lines().any(|line|line=="LoadState=loaded")&&text.lines().any(|line|line=="UnitFileState=enabled"),"Docker's system socket is missing or not persistently enabled for boot");
        }
        Ok(())
    }
    fn check_members(&self,id:&str,previous:&Config,desired:&Config,members:&BTreeMap<String,Member>)->Result<()>{
        for member in members.values(){
            ensure!(expected_member(previous,id,member)||expected_member(desired,id,member),"unknown or foreign native Compose member");
        }
        Ok(())
    }
    async fn preflight_state(&self,change:&Change,previous:&Config,desired:&Config,mode:ApplyMode)->Result<(Option<Artifacts>,Option<Artifacts>,BTreeMap<String,Member>,BTreeMap<String,String>)>{
        let id=change.resource.strip_prefix("service/").filter(|id|super::super::id_valid(id)).ok_or_else(||anyhow!("invalid Compose service"))?;
        ensure!(change.backend==format!("{}/compose",self.engine),"resource belongs to another Compose provider");
        let before=self.artifacts(previous,id)?;let after=self.artifacts(desired,id)?;
        self.check_prerequisites().await?;
        let members=self.inventory(id).await?;
        self.check_members(id,previous,desired,&members)?;
        let compose=read_artifact(&self.base_dir.join(id),"compose.yaml")?;
        let boot=read_artifact(&self.unit_dir.join(id),&unit(id))?;
        check_bytes(compose.as_deref(),before.as_ref().map(|a|a.compose.as_slice()),after.as_ref().map(|a|a.compose.as_slice()))?;
        check_bytes(boot.as_deref(),before.as_ref().map(|a|a.boot.as_slice()),after.as_ref().map(|a|a.boot.as_slice()))?;
        let fields=self.boot(id).await?;
        ensure!(Self::absent_boot(&fields)||self.owned_boot(id,&fields),"foreign Compose boot unit or unmanaged overrides");
        ensure!(Self::absent_boot(&fields)||fields.get("ActiveState").is_some_and(|s|matches!(s.as_str(),"active"|"inactive"|"failed")),"wait for Compose boot operation to finish");
        // An entirely new definition cannot claim pre-existing containers.
        ensure!(before.is_some()||members.is_empty()||compose.as_deref()==after.as_ref().map(|a|a.compose.as_slice()),"pre-existing project has no owned prior definition");
        if after.is_none(){ensure!(mode==ApplyMode::Now,"removal requires immediate apply");}
        if mode==ApplyMode::NextStart {
            if let (Some(old),Some(next))=(previous.services.get(id),desired.services.get(id)) {
                ensure!(old.containers.keys().all(|name|next.containers.contains_key(name)),"removing containers requires immediate apply");
            }
        }
        if desired.services.get(id).is_some_and(|service|service.enabled) {
            self.check_autostart().await?;
        }
        for member in members.values(){
            if !desired.services.get(id).is_some_and(|s|s.containers.contains_key(&member.name)){
                ensure!(mode==ApplyMode::Now,"removing containers requires immediate apply");
                ensure!(matches!(member.state.as_str(),"exited"|"stopped"|"created"|"configured"|"dead"),"stop removed containers before applying their removal");
            }
        }
        if after.is_some(){self.validate_candidate(desired,id).await?;}
        Ok((before,after,members,fields))
    }
    pub(super) async fn native_preflight(&self,change:&Change,previous:&Config,desired:&Config,mode:ApplyMode)->Result<()>{
        self.preflight_state(change,previous,desired,mode).await.map(|_|())
    }
    pub(super) async fn native_apply(&self,change:&Change,previous:&Config,desired:&Config,mode:ApplyMode)->Result<()>{
        let id=change.resource.trim_start_matches("service/");
        let (before,after,members,fields)=self.preflight_state(change,previous,desired,mode).await?;
        let file=self.compose_file(id);
        let old_compose=before.as_ref().map(|a|a.compose.as_slice());
        let new_compose=after.as_ref().map(|a|a.compose.as_slice());
        let old_boot=before.as_ref().map(|a|a.boot.as_slice());
        let new_boot=after.as_ref().map(|a|a.boot.as_slice());
        // Native removal is performed only with the exact prior file. Never
        // use --volumes or --remove-orphans.
        if after.is_none(){
            if !Self::absent_boot(&fields){
                ensure!(fields.get("NeedDaemonReload").is_some_and(|s|s=="no"),"Compose boot unit has pending external reload");
                for action in ["stop","disable"]{self.user_command(&[action,&unit(id)],60).await?;}
            }
            let actual=read_artifact(&self.base_dir.join(id),"compose.yaml")?;
            if actual.is_some(){
                ensure!(actual.as_deref()==old_compose,"Compose project removal requires exact prior definition");
                self.compose_command(id,&file,&["down"],1200,0,true).await?;
            }else{ensure!(members.is_empty(),"cannot remove project without its owned definition");}
            ensure!(self.inventory(id).await?.is_empty(),"Compose project still has native members");
        }else if let Some(old)=previous.services.get(id){
            let next=&desired.services[id];
            for name in old.containers.keys().filter(|name|!next.containers.contains_key(*name)){
                if members.contains_key(name){
                    ensure!(read_artifact(&self.base_dir.join(id),"compose.yaml")?.as_deref()==old_compose,"stopped member removal requires exact prior definition");
                    self.compose_command(id,&file,&["rm","--force",name],1200,0,true).await?;
                }
            }
        }
        publish_artifact(&self.base_dir.join(id),"compose.yaml",old_compose,new_compose)?;
        publish_artifact(&self.unit_dir.join(id),&unit(id),old_boot,new_boot)?;
        if after.is_some(){
            let unit_path=self.unit_file(id);
            let unit_path=unit_path.to_str().ok_or_else(||anyhow!("invalid Compose boot path"))?;
            self.user_command(&["link",unit_path],60).await?;
            let operation=if desired.services[id].enabled{"enable"}else{"disable"};
            self.user_command(&[operation,&unit(id)],60).await?;
            if operation=="disable"{self.user_command(&["link",unit_path],60).await?;}
        }
        self.user_command(&["daemon-reload"],60).await?;
        if after.is_some()&&mode==ApplyMode::Now{
            let wanted=runtime_fingerprint(desired,id)?;
            let active:Vec<_>=members.values().filter(|m|m.state=="running").collect();
            let changed=active.iter().any(|m|m.fingerprint!=wanted);
            if changed&&!active.is_empty(){
                let mut argv=vec!["up","--detach","--no-build","--no-deps"];
                argv.extend(active.iter().map(|m|m.name.as_str()));
                self.compose_command(id,&file,&argv,1200,0,true).await?;
                let actual=self.inventory(id).await?;
                for member in active{ensure!(actual.get(&member.name).is_some_and(|m|m.state=="running"&&m.fingerprint==wanted),"running Compose member reconciliation failed");}
            }
        }
        ensure!(self.native_observe(change,previous,desired,mode).await?==ObservedResource::After,"native Compose publication could not be verified");
        Ok(())
    }
    async fn matches_state(&self,config:&Config,id:&str,mode:ApplyMode,members:&BTreeMap<String,Member>,fields:&BTreeMap<String,String>)->Result<bool>{
        let art=self.artifacts(config,id)?;
        if read_artifact(&self.base_dir.join(id),"compose.yaml")?.as_deref()!=art.as_ref().map(|a|a.compose.as_slice())
            ||read_artifact(&self.unit_dir.join(id),&unit(id))?.as_deref()!=art.as_ref().map(|a|a.boot.as_slice())
            ||!self.boot_matches(id,config,fields){return Ok(false)}
        if art.is_none(){return Ok(members.is_empty())}
        let fingerprint=runtime_fingerprint(config,id)?;
        Ok(members.values().all(|m|expected_member(config,id,m)&&(mode==ApplyMode::NextStart||m.state!="running"||m.fingerprint==fingerprint)))
    }
    pub(super) async fn native_observe(&self,change:&Change,previous:&Config,desired:&Config,mode:ApplyMode)->Result<ObservedResource>{
        let id=change.resource.strip_prefix("service/").filter(|id|super::super::id_valid(id)).ok_or_else(||anyhow!("invalid Compose service"))?;
        let members=self.inventory(id).await?;let fields=self.boot(id).await?;
        if self.matches_state(desired,id,mode,&members,&fields).await?{Ok(ObservedResource::After)}
        else if self.matches_state(previous,id,ApplyMode::NextStart,&members,&fields).await?{Ok(ObservedResource::Before)}
        else{Ok(ObservedResource::Unknown)}
    }
    pub async fn status_details(&self,config:&Config,id:&str)->Result<ComposeStatus>{
        let members=self.inventory(id).await?;let fields=self.boot(id).await?;
        ensure!(self.matches_state(config,id,ApplyMode::NextStart,&members,&fields).await?,"native Compose identity or artifacts differ");
        let service=&config.services[id];let fingerprint=runtime_fingerprint(config,id)?;
        let running=members.values().filter(|m|m.state=="running").count();
        let state=if running==service.containers.len(){"running"}else if running==0{"stopped"}else{"partial"};
        let runtime_matches=members.values().all(|m|m.fingerprint==fingerprint);
        Ok(ComposeStatus{state,runtime_matches,restart_required:!runtime_matches,members:service.containers.keys().map(|name|ContainerMemberStatus{
            name:name.clone(),state:members.get(name).map_or("missing".into(),|m|m.state.clone())
        }).collect()})
    }
    pub async fn verify_boot_policy(&self,config:&Config,id:&str)->Result<()>{
        self.status_details(config,id).await.map(|_|())
    }
    async fn action_one(&self,config:&Config,id:&str,action:&str)->Result<()>{
        self.status_details(config,id).await?;
        if action!="stop"{self.validate_candidate(config,id).await?;}
        let argv:&[&str]=match action{
            "start"=>&["up","--detach","--no-build"],
            "restart"=>&["up","--detach","--no-build","--force-recreate"],
            "stop"=>&["stop"],_=>return Err(anyhow!("unsupported Compose action"))
        };
        self.compose_command(id,&self.compose_file(id),argv,1200,0,true).await?;
        let status=self.status_details(config,id).await?;
        ensure!(if action=="stop"{status.state=="stopped"}else{status.state=="running"},"Compose group did not reach requested state");
        Ok(())
    }
    pub async fn native_action(&self,config:&Config,id:&str,action:&str)->Result<String>{
        ensure!(matches!(action,"start"|"stop"|"restart"),"unsupported Compose action");
        if action=="stop"{self.action_one(config,id,action).await?;return Ok(format!("Compose {action} completed"))}
        let mut needed=BTreeSet::new();let mut pending=vec![id];
        while let Some(next)=pending.pop(){if needed.insert(next){let service=config.services.get(next).ok_or_else(||anyhow!("unknown Compose dependency"))?;ensure!(self.matches(service),"Compose dependency backend is unsupported");pending.extend(service.depends_on.iter().map(String::as_str));}}
        let order=config.service_order()?;
        // Check every dependency before executing any lifecycle operation.
        for dep in order.iter().copied().filter(|dep|needed.contains(dep)){self.status_details(config,dep).await?;self.validate_candidate(config,dep).await?;}
        for dep in order.into_iter().filter(|dep|needed.contains(dep)){self.action_one(config,dep,if dep==id{action}else{"start"}).await?;}
        Ok(format!("Compose {action} completed"))
    }
    pub async fn native_logs(&self,config:&Config,id:&str,tail:usize)->Result<ManagedLogs>{
        ensure!((1..=2000).contains(&tail),"invalid log line limit");
        self.status_details(config,id).await?;
        let tail=tail.to_string();
        let result=self.compose_command(id,&self.compose_file(id),&["logs","--tail",&tail],15,128*1024,true).await?;
        Ok(ManagedLogs{text:String::from_utf8_lossy(&result.stdout).into_owned(),truncated:result.truncated,source:"compose"})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn project_and_boot_policy_are_explicit(){
        let mut c=super::super::super::parse(b"version: 1\nservices:\n  app: {kind: container, engine: docker, containers: {web: {image: alpine}}}\n").unwrap();
        let fingerprint=runtime_fingerprint(&c,"app").unwrap();
        c.services.get_mut("app").unwrap().enabled=true;
        assert_eq!(runtime_fingerprint(&c,"app").unwrap(),fingerprint);
        let boot=String::from_utf8(render_boot(&c,"app",Path::new("/usr/bin/docker"),Path::new("/home/user/generated/app/compose.yaml")).unwrap()).unwrap();
        assert!(boot.contains("RemainAfterExit=yes"));
        assert!(boot.contains("WorkingDirectory=/home/user/generated/app"));
        assert!(boot.contains("\"--project-name\""));
        assert!(boot.contains("\"--env-file\" \"/dev/null\""));
        assert!(boot.contains("DOCKER_HOST=unix:///var/run/docker.sock"));
        let spaced=String::from_utf8(render_boot(&c,"app",Path::new("/usr/bin/docker"),Path::new("/home/user/has space %percent/compose.yaml")).unwrap()).unwrap();
        assert!(spaced.contains("WorkingDirectory=/home/user/has space %%percent\n"));
        assert!(render_boot(&c,"app",Path::new("/usr/bin/docker"),Path::new("/home/user/bad\npath/compose.yaml")).is_err());
    }
    #[test]
    fn parses_docker_lines_and_podman_arrays_without_claiming_foreign_projects(){
        let project=project("app");
        let fingerprint="a".repeat(64);
        let labels=format!("io.server-man.service=app,io.server-man.container=web,io.server-man.fingerprint={fingerprint},com.docker.compose.project={project},com.docker.compose.service=web");
        let row=serde_json::json!({"Names":"maestro-app-web","State":"running","Labels":labels});
        assert_eq!(parse_inventory(row.to_string().as_bytes(),"app").unwrap()["web"].state,"running");
        let row=serde_json::json!({"Names":["maestro-app-web"],"State":"exited","Labels":{"io.server-man.service":"app","io.server-man.container":"web","io.server-man.fingerprint":fingerprint,"io.podman.compose.project":project,"io.podman.compose.service":"web"}});
        assert_eq!(parse_inventory(serde_json::json!([row.clone()]).to_string().as_bytes(),"app").unwrap()["web"].state,"exited");
        assert!(parse_inventory(serde_json::json!([row.clone(),row]).to_string().as_bytes(),"app").is_err());
        assert!(parse_inventory(br#"{"Names":"foreign","State":"running","Labels":"com.docker.compose.project=wrong"}"#,"app").is_err());
    }
    #[tokio::test]
    async fn reference_define_toggle_and_remove_do_not_start_workloads() {
        use std::os::unix::fs::PermissionsExt;
        for engine in ["docker","podman"] {
            let temp=tempfile::tempdir().unwrap();
            fs::set_permissions(temp.path(),fs::Permissions::from_mode(0o700)).unwrap();
            let log=temp.path().join("commands");
            let unit_link=temp.path().join("unit-link");
            let enabled=temp.path().join("enabled");
            let binary=temp.path().join(engine);
            let systemctl=temp.path().join("systemctl");
            let script=format!(r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
[ "$1" != '--remote=false' ] || shift
case "$1" in
ps) printf '[]\n';;
info) if [ "$3" = '{{{{.Host.Security.Rootless}}}}' ]; then printf 'true\n'; else printf '{{}}\n'; fi;;
compose) exit 0;;
*) exit 1;;
esac
"#,log.display());
            fs::write(&binary,script).unwrap();
            let script=format!(r#"#!/bin/sh
printf '%s\n' "$*" >> '{log}'
if [ "$1" = --user ]; then shift; fi
[ "$1" != --no-pager ] || shift
[ "$1" != --no-ask-password ] || shift
case "$1" in
show)
 case "$2" in
 --property=Version) echo Version=255;;
 docker.socket) printf 'LoadState=loaded\nUnitFileState=enabled\n';;
 podman-user-wait-network-online.service) echo loaded;;
 *)
  if [ -f '{link}' ] && [ -f "$(cat '{link}')" ]; then
   printf 'LoadState=loaded\nActiveState=inactive\nNeedDaemonReload=no\nDropInPaths=\nFragmentPath=%s\n' "$(cat '{link}')"
   if [ -f '{enabled}' ]; then echo UnitFileState=enabled; else echo UnitFileState=linked; fi
  else printf 'LoadState=not-found\nActiveState=inactive\n'; fi;;
 esac;;
link) printf '%s' "$2" > '{link}';;
enable) : > '{enabled}';;
disable) rm -f '{enabled}';;
stop|daemon-reload) :;;
*) exit 1;;
esac
"#,log=log.display(),link=unit_link.display(),enabled=enabled.display());
            fs::write(&systemctl,script).unwrap();
            for path in [&binary,&systemctl]{fs::set_permissions(path,fs::Permissions::from_mode(0o700)).unwrap();}
            let provider=ComposeProvider::new(engine,binary,temp.path().join("generated"),temp.path().into(),CommandRunner::new(false)).native_paths(systemctl);
            let text=format!("version: 1\nservices:\n  app: {{kind: container, engine: {engine}, backend: compose, enabled: false, containers: {{web: {{image: alpine}}}}}}\n");
            let mut desired=super::super::super::parse(text.as_bytes()).unwrap();
            let empty=Config::default();
            let change=Change{resource:"service/app".into(),backend:format!("{engine}/compose"),operation:super::super::super::Operation::Create,before:None,after:Some(service_fingerprint(&desired,"app").unwrap())};
            provider.native_apply(&change,&empty,&desired,ApplyMode::NextStart).await.unwrap();
            assert!(provider.compose_file("app").is_file());
            assert!(provider.unit_file("app").is_file());
            assert_eq!(provider.status_details(&desired,"app").await.unwrap().state,"stopped");
            let previous=desired.clone();
            desired.services.get_mut("app").unwrap().enabled=true;
            let toggle=Change{resource:change.resource.clone(),backend:change.backend.clone(),operation:super::super::super::Operation::Update,before:Some(service_fingerprint(&previous,"app").unwrap()),after:Some(service_fingerprint(&desired,"app").unwrap())};
            provider.native_apply(&toggle,&previous,&desired,ApplyMode::NextStart).await.unwrap();
            assert!(enabled.exists());
            let commands=fs::read_to_string(&log).unwrap();
            assert!(!commands.lines().any(|line|line.contains(" up ")));
            assert!(commands.contains("--env-file /dev/null --file"));
            let remove=Change{resource:change.resource,backend:change.backend,operation:super::super::super::Operation::Remove,before:toggle.after,after:None};
            provider.native_apply(&remove,&desired,&empty,ApplyMode::Now).await.unwrap();
            assert!(!provider.compose_file("app").exists());
            assert!(!provider.unit_file("app").exists());
            let commands=fs::read_to_string(&log).unwrap();
            assert!(commands.lines().any(|line|line.ends_with(" down")));
            assert!(!commands.contains("--volumes"));
        }
    }

    #[test]
    fn artifact_drift_is_not_overwritten(){
        assert!(check_bytes(Some(b"old"),Some(b"old"),Some(b"new")).is_ok());
        assert!(check_bytes(Some(b"new"),Some(b"old"),Some(b"new")).is_ok());
        assert!(check_bytes(Some(b"foreign"),Some(b"old"),Some(b"new")).is_err());
    }
}
