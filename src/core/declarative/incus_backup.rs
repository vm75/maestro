//! Local, portable stopped Incus instance export. Custom-volume and snapshot
//! coverage is deliberately blocked until the reference dependency graph is ported.
use super::*;
use crate::core::declarative::backup_capture::{self as capture,NativeObject,NativeExport};
use std::{fs::File,io::Read,time::Instant};

impl IncusProvider{
    pub(crate) async fn backup_dependency(&self,c:&Config,id:&str)->Result<NativeObject>{
        let server:Value=self.get(None,"/1.0").await?;
        let env=&server["environment"];
        let server_name=env["server_name"].as_str().filter(|s|!s.is_empty()).ok_or_else(||anyhow!("Incus daemon identity is unknown"))?;
        ensure!(env["server_clustered"].as_bool()==Some(false),"backup requires a verified non-clustered Incus daemon");
        let (want,actual)=self.verified(c,id).await?;
        ensure!(actual.location.is_empty()||actual.location=="none"||actual.location==server_name,"Incus guest is not local");
        ensure!(matches!(actual.status_code,102|103),"Incus guest state is not stable for capture");
        ensure!(actual.definition.profiles.is_empty()&&actual.expanded_devices==want.devices,"Incus additional native devices or profiles need explicit capture support");
        let project=project(c,id)?;
        let snapshots:Vec<String>=self.get(Some(project),&format!("{}/snapshots",instance_path(id))).await?;
        ensure!(snapshots.is_empty(),"Incus snapshot dependencies require explicit capture support");
        Ok(NativeObject{backend:"incus".into(),name:instance_name(id),project:project.into(),
            pool:want.devices["root"]["pool"].clone(),content_type:"block".into()})
    }
    pub(crate) async fn validate_backup(&self,c:&Config,id:&str,object:&NativeObject,stopped:bool)->Result<()>{
        ensure!(*object==self.backup_dependency(c,id).await?,"Incus export identity changed");
        if stopped{ensure!(self.read(c,id).await?.is_some_and(|a|a.status_code==102),"Incus export requires a verified stopped VM");}
        let help=self.runner.capture_scoped(&self.binary,&["--force-local","export","--help"],Duration::from_secs(5),64*1024,false,ENV,None,&[]).await?;
        let text=String::from_utf8_lossy(&help.stdout);
        ensure!(help.status.success()&&text.contains("--compression")&&text.contains("--root-only"),"Incus portable export flags are unavailable");
        Ok(())
    }
    pub(crate) async fn backup_export(&self,c:&Config,id:&str,object:&NativeObject,root:&File,end:Instant,budget:u64)->Result<NativeExport>{
        self.validate_backup(c,id,object,true).await?;
        let path=format!("native/incus/{}.tar",super::super::revision(&serde_json::to_vec(object)?));
        let file=capture::create_payload(root,&path)?;
        let target=format!("local:{}",object.name);
        self.runner.stream_backup(&self.binary,&["--force-local","--project",&object.project,"--quiet","export",&target,"-","--compression","none","--root-only"],
            file,end.saturating_duration_since(Instant::now()),budget).await?;
        self.validate_backup(c,id,object,true).await?;
        let export=capture::native_export(root,&path,"incus_instance_export",vec![format!("service/{id}")],object.clone(),end)?;
        verify_archive(c,id,object,capture::read_export(root,&path)?,end)?;
        Ok(export)
    }
}
// Inspect without extracting. Require the reviewed portable root disk and
// matching archive identity; reject additional volumes, snapshots and links.
pub(crate) fn verify_archive(c:&Config,id:&str,object:&NativeObject,file:File,end:Instant)->Result<()>{
    let want=render(c,id)?;
    ensure!(object.backend=="incus"&&object.name==instance_name(id)&&object.project==project(c,id)?
        &&object.pool==want.devices["root"]["pool"]&&object.content_type=="block","Incus archive object differs from the authoritative definition");
    let mut archive=tar::Archive::new(file);
    let mut members=BTreeMap::new();let mut index=None;
    for row in archive.entries()?{
        ensure!(Instant::now()<end&&members.len()<50000,"Incus archive verification exceeded bounds");
        let mut row=row?;
        let raw=row.path_bytes();
        let name=std::str::from_utf8(&raw)?.trim_end_matches('/').to_owned();
        ensure!(super::super::path_valid(&name,false)&&(name=="backup"||name.starts_with("backup/"))&&name.len()<=4096,"invalid Incus archive member");
        if let Some(extensions)=row.pax_extensions()?{
            for extension in extensions{let extension=extension?;ensure!(!extension.key_bytes().starts_with(b"GNU.sparse"),"sparse Incus archive extensions are unsupported");}
        }
        let kind=row.header().entry_type();
        ensure!(kind.is_file()||kind.is_dir(),"unsupported Incus archive entry type");
        let size=row.header().size()?;
        ensure!(!kind.is_dir()||size==0,"invalid Incus archive directory");
        ensure!(members.insert(name.clone(),(kind.is_dir(),size)).is_none(),"duplicate Incus archive member");
        if name=="backup/index.yaml"{
            ensure!(kind.is_file()&&size>0&&size<=super::super::MAX_CONFIG_BYTES as u64,"invalid Incus index");
            let mut bytes=Vec::new();row.read_to_end(&mut bytes)?;
            super::super::yaml::check(&bytes)?;
            index=Some(serde_yaml::from_slice::<serde_yaml::Value>(&bytes)?);
        }else{
            let mut buffer=[0;65536];let mut count=0u64;
            loop{ensure!(Instant::now()<end,"Incus archive verification timed out");let n=row.read(&mut buffer)?;if n==0{break}count+=n as u64;}
            ensure!(count==size,"truncated Incus archive member");
        }
    }
    // tar stops at a zero header; reject concatenated archives and
    // non-zero bytes after the end marker.
    let mut file=archive.into_inner();
    let mut trailing=[0;65536];let mut count=0u64;
    loop {
        ensure!(Instant::now()<end,"Incus archive verification timed out");
        let n=file.read(&mut trailing)?;if n==0{break}
        ensure!(trailing[..n].iter().all(|b|*b==0),"unexpected data after Incus tar end");
        count+=n as u64;
    }
    ensure!(count>=512,"Incus archive end marker is incomplete");
    let x=index.ok_or_else(||anyhow!("Incus archive index missing"))?;
    ensure!(x["name"].as_str()==Some(object.name.as_str())&&x["pool"].as_str()==Some(object.pool.as_str())&&x["type"]=="virtual-machine"
        &&x["optimized"].as_bool()==Some(false)&&x["optimized_header"].as_bool()==Some(false)
        &&x["backend"].as_str().is_some_and(|s|!s.is_empty())
        &&(x["snapshots"].is_null()||x["snapshots"].as_sequence().is_some_and(Vec::is_empty)),"Incus archive is not the reviewed portable guest");
    let cfg=&x["config"];let guest=&cfg["container"];let volume=&cfg["volume"];
    ensure!(guest["name"].as_str()==Some(object.name.as_str())&&guest["type"]=="virtual-machine"&&guest["config"][ID]==id
        &&guest["config"][IDENTITY].as_str()==Some(vm(c,id)?.uuid.as_str())&&volume["name"].as_str()==Some(object.name.as_str())&&volume["type"]=="virtual-machine"
        &&volume["content_type"]=="block"&&(volume["project"].is_null()||volume["project"].as_str()==Some(object.project.as_str()))
        &&cfg["pool"]["name"].as_str()==Some(object.pool.as_str())&&cfg["pool"]["driver"]==x["backend"]
        &&(cfg["dependent_volumes"].is_null()||cfg["dependent_volumes"].as_sequence().is_some_and(Vec::is_empty)),
        "Incus archive identity or dependency coverage differs");
    let devices:BTreeMap<String,BTreeMap<String,String>>=serde_yaml::from_value(guest["devices"].clone())?;
    ensure!(devices==want.devices&&(guest["profiles"].is_null()||guest["profiles"].as_sequence().is_some_and(Vec::is_empty))
        &&guest["stateful"].as_bool()!=Some(true),"Incus archived device or state coverage differs");
    ensure!(members.get("backup/virtual-machine.img").is_some_and(|(dir,size)|!*dir&&*size>0)
        &&members.get("backup/virtual-machine").is_some_and(|(dir,_)|*dir),"Incus archive root payload missing");
    for name in members.keys(){
        ensure!(matches!(name.as_str(),"backup"|"backup/index.yaml"|"backup/virtual-machine"|"backup/virtual-machine.img")
            ||name.starts_with("backup/virtual-machine/"),"unexpected Incus archive payload");
        let mut parent=std::path::Path::new(name).parent();
        while let Some(p)=parent.filter(|p|!p.as_os_str().is_empty()){
            ensure!(members.get(p.to_str().unwrap_or("")).is_none_or(|(dir,_)|*dir),"Incus archive parent is not a directory");
            parent=p.parent();
        }
    }
    Ok(())
}
