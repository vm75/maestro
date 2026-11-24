//! Portable Incus backup discovery and exports, translated from the reference implementation's
//! incus_backup, incus_backup_history, incus_disk_reference and incus_export.
use super::*;
use crate::core::declarative::{
    backup_capture::{self as capture, NativeExport, NativeObject},
    backup_inventory::BackupInput,
};
use std::{collections::BTreeSet, fs::File, io::Read, time::Instant};

type Devices = BTreeMap<String, BTreeMap<String, String>>;
type References = BTreeSet<(String, String)>;

#[derive(Debug, PartialEq, Eq)]
struct GuestDisks {
    root: String,
    volumes: References,
}

fn portable_path(path: &str) -> bool {
    super::super::path_valid(path, false)
}

fn guest_disks(devices: &Devices) -> Result<GuestDisks> {
    ensure!(devices.len() <= 64, "Incus device limit exceeded");
    let mut root = None;
    let mut volumes = BTreeSet::new();
    for device in devices.values() {
        let get = |key: &str| device.get(key).map(String::as_str).unwrap_or("");
        if get("type") == "nic" { continue; }
        ensure!(get("type") == "disk" && get("project").is_empty(), "unsupported Incus native device");
        let pool = get("pool");
        let source = get("source");
        ensure!(binding_name(pool), "Incus host/external disk is not covered");
        if get("path") == "/" {
            ensure!(source.is_empty() && root.replace(pool.to_owned()).is_none(), "ambiguous Incus root disk");
        } else {
            let (name, subpath) = source.split_once('/').map_or((source, None), |(n, p)| (n, Some(p)));
            ensure!(binding_name(name) && subpath.is_none_or(portable_path), "unsupported Incus custom disk reference");
            volumes.insert((pool.to_owned(), name.to_owned()));
        }
    }
    Ok(GuestDisks { root: root.ok_or_else(|| anyhow!("Incus root disk missing"))?, volumes })
}

fn metadata_disks(guest: &Value) -> Result<GuestDisks> {
    let profiles = guest.get("profiles").filter(|v| !v.is_null());
    let expanded = guest.get("expanded_devices").filter(|v| !v.is_null());
    if let Some(p) = profiles {
        ensure!(p.is_array(), "invalid Incus profile metadata");
        ensure!(p.as_array().is_some_and(Vec::is_empty) || expanded.is_some(), "Incus expanded profile devices unavailable");
    }
    let devices: Devices = serde_json::from_value(expanded.unwrap_or(&guest["devices"]).clone())?;
    guest_disks(&devices)
}

fn owned(guest: &Value, c: &Config, id: &str) -> Result<()> {
    ensure!(guest["config"][ID].as_str() == Some(id)
        && guest["config"][IDENTITY].as_str() == Some(vm(c, id)?.uuid.as_str()),
        "Incus historical guest ownership does not match");
    Ok(())
}

fn snapshot_records(rows: &[Value], c: &Config, id: &str) -> Result<(BTreeSet<String>, References)> {
    ensure!(rows.len() <= 2048, "Incus snapshot limit exceeded");
    let mut names = BTreeSet::new();
    let mut references = BTreeSet::new();
    for row in rows {
        let name = row["name"].as_str().filter(|s| binding_name(s)).ok_or_else(|| anyhow!("invalid Incus snapshot name"))?;
        ensure!(names.insert(name.to_owned()), "duplicate Incus snapshot");
        owned(row, c, id)?;
        references.extend(metadata_disks(row)?.volumes);
        ensure!(references.len() <= 2048, "Incus historical dependency limit exceeded");
    }
    Ok((names, references))
}

#[derive(Debug, PartialEq, Eq)]
struct Discovery {
    instance: NativeObject,
    snapshots: BTreeSet<String>,
    references: References,
    volumes: BTreeMap<NativeObject, BTreeSet<String>>,
}

fn consumer(c: &Config, usage: &str) -> Result<String> {
    ensure!(usage.starts_with("/1.0/instances/") && !usage.contains('#'), "unmanaged Incus volume consumer");
    let uri = url::Url::parse(&format!("http://localhost{usage}"))?;
    ensure!(usage.split('?').next()==Some(uri.path()) && uri.query_pairs().all(|(key, _)| key=="project"), "unsupported Incus volume consumer URI");
    let mut projects = uri.query_pairs().filter(|(k, _)| k == "project");
    let scope = projects.next().map(|(_, v)| v.into_owned()).unwrap_or_else(|| "default".into());
    ensure!(projects.next().is_none() && binding_name(&scope), "ambiguous Incus consumer project");
    let path = uri.path();
    let base = if let Some((base, snapshot)) = path.split_once("/snapshots/") {
        ensure!(binding_name(snapshot), "invalid Incus snapshot consumer");
        base
    } else { path };
    for (id, service) in &c.services {
        if service.kind == "vm" && service.backend == "incus"
            && project(c, id)? == scope && instance_path(id) == base {
            return Ok(format!("service/{id}"));
        }
    }
    Err(anyhow!("Incus volume has unmanaged or differently scoped consumers"))
}

fn input(kind: &'static str, object: NativeObject, resources: Vec<String>) -> Result<BackupInput> {
    let id = format!("native/incus/{kind}/{}", super::super::revision(&serde_json::to_vec(&object)?));
    Ok(BackupInput { id, kind, strategy: "native_export", path: None, bundle_path: None,
        included_by: None, resources, required: true, sensitive: true, inspection: "native_dependencies_discovered",
        logical_bytes: None, allocated_bytes: None, native: Some(object) })
}

impl IncusProvider {
    // Backup observes native ownership rather than reconciling hardware. A native
    // disk attachment must not be silently removed when a quiesced VM is resumed.
    async fn backup_local(&self, c: &Config, id: &str) -> Result<Instance> {
        ensure!(!vm(c,id)?.is_system_container(), "Incus system-container filesystem export is not yet verified; capture blocked");
        let server: Value = self.get(None, "/1.0").await?;
        let env = &server["environment"];
        let server_name = env["server_name"].as_str().filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("Incus daemon identity is unknown"))?;
        ensure!(env["server_clustered"].as_bool() == Some(false), "backup requires a verified non-clustered Incus daemon");
        let actual = self.read(c, id).await?.ok_or_else(|| anyhow!("Incus VM does not exist"))?;
        ensure!(actual.location.is_empty() || actual.location == "none" || actual.location == server_name, "Incus guest is not local");
        ensure!(matches!(actual.status_code, 102 | 103), "Incus guest state is not stable for capture");
        ensure!(!actual.definition.ephemeral && actual.definition.profiles.is_empty(), "Incus ephemeral guests or native profiles require explicit backup policy");
        Ok(actual)
    }

    pub(crate) async fn backup_state(&self, c: &Config, id: &str) -> Result<&'static str> {
        Ok(if self.backup_local(c, id).await?.status_code == 102 { "stopped" } else { "running" })
    }

    pub(crate) async fn backup_action(&self, c: &Config, id: &str, action: &str) -> Result<()> {
        let actual = self.backup_local(c, id).await?;
        let scope = project(c, id)?;
        let target = target(id);
        match (action, actual.status_code) {
            ("stop", 103) => self.run(scope, None, &["stop", &target, "--timeout", "90"], 100).await?,
            ("start", 102) => self.run(scope, None, &["start", &target], 1200).await?,
            ("stop", 102) | ("start", 103) => (),
            _ => return Err(anyhow!("unsupported Incus backup action")),
        }
        ensure!(self.backup_state(c, id).await? == if action == "stop" { "stopped" } else { "running" },
            "Incus backup action did not reach expected state");
        Ok(())
    }

    async fn discover(&self, c: &Config, id: &str) -> Result<Discovery> {
        let actual = self.backup_local(c, id).await?;
        let scope = project(c, id)?;
        let want = render(c, id)?;
        let devices = if actual.expanded_devices.is_empty() { &actual.definition.devices } else { &actual.expanded_devices };
        let current = guest_disks(devices)?;
        let declared = guest_disks(&actual.definition.devices)?;
        ensure!(declared.root==current.root && declared.volumes.is_subset(&current.volumes), "Incus expanded devices omit native disks");
        ensure!(current.root == want.devices["root"]["pool"], "Incus current root pool differs from authoritative binding");
        let snapshot_rows: Vec<Value> = self.get(Some(scope), &format!("{}/snapshots?recursion=1", instance_path(id))).await?;
        let (snapshots, historical) = snapshot_records(&snapshot_rows, c, id)?;
        let references: References = current.volumes.union(&historical).cloned().collect();
        let instance = NativeObject { backend: "incus".into(), name: instance_name(id),
            project: scope.into(), pool: current.root, content_type: "block".into() };
        let mut volumes = BTreeMap::new();
        if !references.is_empty() {
            let info: Value = self.get(None, &format!("/1.0/projects/{scope}")).await?;
            ensure!(info["name"].as_str() == Some(scope), "Incus project identity cannot be verified");
            ensure!(info["config"].is_object(), "Incus project config unavailable");
            let volume_scope = match info["config"].get("features.storage.volumes") {
                Some(v) if v.as_str()==Some("true") => scope,
                Some(v) if matches!(v.as_str(),Some("false" | "")) => "default",
                None => "default",
                _ => return Err(anyhow!("Incus storage project feature is unknown")),
            };
            let resource = format!("service/{id}");
            for (pool, name) in &references {
                let info: Value = self.get(Some(volume_scope), &format!("/1.0/storage-pools/{pool}/volumes/custom/{name}")).await?;
                let content_type = info["content_type"].as_str().filter(|s| matches!(*s, "filesystem" | "block" | "iso"))
                    .ok_or_else(|| anyhow!("unsupported Incus volume content type"))?;
                ensure!(info["name"].as_str() == Some(name.as_str()) && info["type"].as_str() == Some("custom"), "Incus volume identity differs");
                ensure!(info.get("used_by").is_some(), "Incus volume usage unavailable");
                let usages = rows(&info["used_by"])?;
                ensure!(usages.len() <= 1024, "Incus volume consumer limit exceeded");
                let mut users = BTreeSet::new();
                for usage in usages {
                    users.insert(consumer(c, usage.as_str().ok_or_else(|| anyhow!("invalid Incus consumer"))?)?);
                }
                ensure!(!current.volumes.contains(&(pool.clone(), name.clone())) || users.contains(&resource), "Incus current volume usage unverified");
                let object = NativeObject { backend: "incus".into(), name: name.clone(), pool: pool.clone(),
                    project: volume_scope.into(), content_type: content_type.into() };
                volumes.insert(object, users);
            }
        }
        Ok(Discovery { instance, snapshots, references, volumes })
    }

    // Deduplicate by native identity, not guest or device name. Historical-only
    // consumers may be absent from used_by, but every reported consumer must be
    // inventoried and must independently resolve this same object.
    async fn graph(&self, c: &Config) -> Result<(Vec<BackupInput>, BTreeMap<String, Discovery>)> {
        let mut inputs = Vec::new();
        let mut discovered = BTreeMap::new();
        let mut volumes: BTreeMap<NativeObject, (BTreeSet<String>, BTreeSet<String>)> = BTreeMap::new();
        let mut identities = BTreeMap::new();
        for (id, service) in &c.services {
            if service.kind != "vm" || service.backend != "incus" { continue; }
            let fresh = self.discover(c, id).await?;
            let resource = format!("service/{id}");
            inputs.push(input("incus_instance_export", fresh.instance.clone(), vec![resource.clone()])?);
            for (object, users) in &fresh.volumes {
                let identity = (&object.project, &object.pool, &object.name);
                let key = (identity.0.clone(), identity.1.clone(), identity.2.clone());
                ensure!(identities.insert(key, object.content_type.clone()).is_none_or(|old| old == object.content_type), "Incus volume content type changed");
                let (actual, reported) = volumes.entry(object.clone()).or_default();
                actual.insert(resource.clone());
                reported.extend(users.iter().cloned());
            }
            discovered.insert(id.clone(), fresh);
            ensure!(inputs.len() + volumes.len() <= 2048, "Incus backup object limit exceeded");
        }
        for (object, (actual, reported)) in volumes {
            ensure!(reported.is_subset(&actual) && actual.len() <= 128, "Incus shared volume consumer coverage differs");
            inputs.push(input("incus_custom_volume", object, actual.into_iter().collect())?);
        }
        Ok((inputs, discovered))
    }

    pub(crate) async fn backup_inputs(&self, c: &Config) -> Result<Vec<BackupInput>> {
        Ok(self.graph(c).await?.0)
    }

    async fn checked_graph(&self, c: &Config, reviewed: &BackupInput, stopped: bool) -> Result<BTreeMap<String, Discovery>> {
        let (inputs, graph) = self.graph(c).await?;
        ensure!(inputs.iter().any(|i| i.kind == reviewed.kind && i.native == reviewed.native && i.resources == reviewed.resources),
            "Incus export identity or consumers changed");
        if stopped {
            for resource in &reviewed.resources {
                let id = resource.strip_prefix("service/").ok_or_else(|| anyhow!("invalid Incus consumer"))?;
                ensure!(self.backup_state(c, id).await? == "stopped", "all Incus export consumers must be stopped");
            }
        }
        Ok(graph)
    }

    pub(crate) async fn validate_backup(&self, c: &Config, reviewed: &BackupInput, stopped: bool) -> Result<()> {
        self.checked_graph(c, reviewed, stopped).await?;
        let help_args: &[&str] = if reviewed.kind == "incus_instance_export" {
            &["--force-local", "export", "--help"]
        } else { &["--force-local", "storage", "volume", "export", "--help"] };
        let help = self.runner.capture_scoped(&self.binary, help_args, Duration::from_secs(5), 64*1024, false, ENV, None, &[]).await?;
        let text = String::from_utf8_lossy(&help.stdout);
        ensure!(help.status.success() && text.contains("--compression") && (reviewed.kind != "incus_instance_export" || text.contains("--root-only")),
            "Incus portable export flags are unavailable");
        Ok(())
    }

    pub(crate) async fn backup_export(&self, c: &Config, reviewed: &BackupInput, root: &File, end: Instant, budget: u64) -> Result<NativeExport> {
        self.validate_backup(c, reviewed, true).await?;
        let before = self.checked_graph(c, reviewed, true).await?;
        let object = reviewed.native.as_ref().ok_or_else(|| anyhow!("missing Incus export object"))?;
        let extension = if object.content_type == "iso" { "iso" } else { "tar" };
        let prefix = if reviewed.kind == "incus_custom_volume" { "volume-" } else { "" };
        let path = format!("native/incus/{prefix}{}.{}", super::super::revision(&serde_json::to_vec(object)?), extension);
        let file = capture::create_payload(root, &path)?;
        let instance = format!("local:{}", object.name);
        let pool = format!("local:{}", object.pool);
        let args: Vec<&str> = if reviewed.kind == "incus_instance_export" {
            vec!["--force-local", "--project", &object.project, "--quiet", "export", &instance, "-", "--compression", "none", "--root-only"]
        } else {
            vec!["--force-local", "--project", &object.project, "--quiet", "storage", "volume", "export", &pool, &object.name, "-", "--compression", "none"]
        };
        self.runner.stream_backup(&self.binary, &args, file, end.saturating_duration_since(Instant::now()), budget).await?;
        ensure!(before == self.checked_graph(c, reviewed, true).await?, "Incus dependencies changed during export");
        let export = capture::native_export(root, &path, reviewed.kind, reviewed.resources.clone(), object.clone(), end)?;
        let archive = inspect_archive(c, &export, capture::read_export(root, &path)?, end)?;
        if reviewed.kind == "incus_instance_export" {
            let id = reviewed.resources[0].strip_prefix("service/").ok_or_else(|| anyhow!("invalid Incus resource"))?;
            let live = &before[id];
            ensure!(archive.snapshots == live.snapshots && archive.references == live.references, "Incus archive snapshot or device coverage changed");
        }
        Ok(export)
    }
}

#[derive(Debug, Default)]
struct ArchiveCoverage {
    snapshots: BTreeSet<String>,
    references: References,
}

// Inspect without extracting; retain the portable payload for every indexed generation.
fn inspect_archive(c: &Config, export: &NativeExport, file: File, end: Instant) -> Result<ArchiveCoverage> {
    let object = &export.native;
    let instance = export.kind == "incus_instance_export";
    ensure!(object.backend == "incus" && binding_name(&object.name) && binding_name(&object.pool) && binding_name(&object.project)
        && if instance { object.content_type == "block" && export.resources.len() == 1 }
        else { export.kind == "incus_custom_volume" && matches!(object.content_type.as_str(), "block" | "filesystem" | "iso") },
        "invalid Incus archive identity");
    if object.content_type == "iso" {
        // Incus exports ISO custom volumes as raw bytes. The common bundle
        // verifier checks their size and digest, not a nonexistent tar index.
        ensure!(file.metadata()?.len() > 0 && export.bundle_path.ends_with(".iso"), "invalid Incus ISO export");
        return Ok(ArchiveCoverage::default());
    }
    ensure!(export.bundle_path.ends_with(".tar"), "invalid Incus tar export path");
    let mut archive=tar::Archive::new(file);
    let mut members=BTreeMap::new();let mut index=None;
    for row in archive.entries()?{
        capture::check_time(end)?;ensure!(members.len()<50000,"Incus archive verification exceeded bounds");
        let mut row=row?;
        let raw=row.path_bytes();
        let name=std::str::from_utf8(&raw)?.trim_end_matches('/').to_owned();
        ensure!(super::super::path_valid(&name,false)&&(name=="backup"||name.starts_with("backup/"))&&name.len()<=4096,"invalid Incus archive member");
        if let Some(extensions)=row.pax_extensions()?{
            for extension in extensions{let extension=extension?;ensure!(!extension.key_bytes().starts_with(b"GNU.sparse"),"sparse Incus archive extensions are unsupported");}
        }
        let kind=row.header().entry_type();
        ensure!(kind.is_file() || kind.is_dir() || kind.is_symlink() || kind.is_hard_link()
            || kind.is_character_special() || kind.is_block_special() || kind.is_fifo(), "unsupported Incus archive entry type");
        let size=row.header().size()?;
        ensure!(kind.is_file() || size==0,"invalid Incus archive entry size");
        let link = row.link_name()?.map(|p| p.to_str().map(str::to_owned).ok_or_else(||anyhow!("invalid Incus archive link"))).transpose()?;
        ensure!(if kind.is_hard_link() { link.as_deref().is_some_and(portable_path) }
            else if kind.is_symlink() { link.as_deref().is_some_and(|s| !s.is_empty() && !s.contains('\0')) }
            else { link.is_none() }, "invalid Incus archive link");
        ensure!(members.insert(name.clone(),(kind,size,link)).is_none(),"duplicate Incus archive member");
        if name=="backup/index.yaml"{
            ensure!(kind.is_file()&&size>0&&size<=super::super::MAX_CONFIG_BYTES as u64,"invalid Incus index");
            let mut bytes=Vec::new();row.read_to_end(&mut bytes)?;
            super::super::yaml::check(&bytes)?;
            index=Some(serde_yaml::from_slice::<serde_yaml::Value>(&bytes)?);
        }else{
            let mut buffer=[0;65536];let mut count=0u64;
            loop{capture::check_time(end)?;let n=row.read(&mut buffer)?;if n==0{break}count+=n as u64;}
            ensure!(count==size,"truncated Incus archive member");
        }
    }
    // tar stops at a zero header; reject concatenated archives and
    // non-zero bytes after the end marker.
    let mut file=archive.into_inner();
    let mut trailing=[0;65536];let mut count=0u64;
    loop {
        capture::check_time(end)?;
        let n=file.read(&mut trailing)?;if n==0{break}
        ensure!(trailing[..n].iter().all(|b|*b==0),"unexpected data after Incus tar end");
        count+=n as u64;
    }
    ensure!(count>=512,"Incus archive end marker is incomplete");
    let x = serde_json::to_value(index.ok_or_else(|| anyhow!("Incus archive index missing"))?)?;
    let cfg = &x["config"];
    let volume = &cfg["volume"];
    let volume_type = if instance { "virtual-machine" } else { "custom" };
    ensure!(x["name"].as_str() == Some(object.name.as_str()) && x["pool"].as_str() == Some(object.pool.as_str())
        && x["type"].as_str() == Some(volume_type) && x["optimized"].as_bool() == Some(false)
        && x["optimized_header"].as_bool() == Some(false)
        && x["backend"].as_str().is_some_and(|s| !s.is_empty())
        && volume["name"].as_str() == Some(object.name.as_str()) && volume["type"].as_str() == Some(volume_type)
        && volume["content_type"].as_str() == Some(object.content_type.as_str())
        && (volume["project"].is_null() || volume["project"].as_str() == Some("") || volume["project"].as_str() == Some(object.project.as_str()))
        && empty_rows(&cfg["dependent_volumes"])?, "Incus archive identity or dependency coverage differs");
    if instance || !cfg["pool"].is_null() {
        ensure!(cfg["pool"]["name"].as_str() == Some(object.pool.as_str()) && cfg["pool"]["driver"] == x["backend"], "Incus archive pool differs");
    }

    let mut coverage = ArchiveCoverage::default();
    for name in rows(&x["snapshots"])? {
        let name = name.as_str().filter(|s| binding_name(s)).ok_or_else(|| anyhow!("invalid Incus archive snapshot name"))?;
        ensure!(coverage.snapshots.insert(name.to_owned()), "duplicate Incus archive snapshot");
    }
    let mut volume_names = BTreeSet::new();
    for snapshot in rows(&cfg["volume_snapshots"])? {
        let name = snapshot["name"].as_str().ok_or_else(|| anyhow!("invalid Incus volume snapshot record"))?;
        ensure!(coverage.snapshots.contains(name) && volume_names.insert(name.to_owned())
            && snapshot["content_type"].as_str() == Some(object.content_type.as_str())
            && (snapshot["project"].is_null() || snapshot["project"].as_str() == Some("") || snapshot["project"].as_str() == Some(object.project.as_str())),
            "Incus volume snapshot identity differs");
    }
    ensure!(volume_names == coverage.snapshots, "Incus volume snapshot metadata incomplete");
    if instance {
        let id = export.resources[0].strip_prefix("service/").ok_or_else(|| anyhow!("invalid Incus resource"))?;
        let guest = &cfg["container"];
        ensure!(object.name == instance_name(id) && object.project == project(c, id)?
            && object.pool == render(c, id)?.devices["root"]["pool"]
            && guest["name"].as_str() == Some(object.name.as_str()) && guest["type"].as_str() == Some("virtual-machine"),
            "Incus archive guest differs");
        owned(guest, c, id)?;
        let current = metadata_disks(guest)?;
        ensure!(current.root == object.pool, "Incus archive root pool differs");
        let (names, historical) = snapshot_records(rows(&cfg["snapshots"])?, c, id)?;
        ensure!(names == coverage.snapshots, "Incus guest snapshot metadata incomplete");
        coverage.references = current.volumes.union(&historical).cloned().collect();
    } else {
        ensure!(cfg["container"].is_null() && empty_rows(&cfg["snapshots"])?, "custom volume archive contains guest data");
    }

    let (prefix, snapshot_prefix) = if instance { ("virtual-machine", "virtual-machine-snapshots") } else { ("volume", "volume-snapshots") };
    let block = object.content_type == "block";
    let mut roots = BTreeSet::new();
    let mut blocks = BTreeSet::new();
    let bases = std::iter::once(format!("backup/{prefix}"))
        .chain(coverage.snapshots.iter().map(|n| format!("backup/{snapshot_prefix}/{n}")));
    for base in bases {
        if block {
            let path = format!("{base}.img");
            ensure!(members.get(&path).is_some_and(|(kind, size, _)| kind.is_file() && *size > 0), "Incus block payload missing");
            blocks.insert(path);
        }
        if !block || instance {
            ensure!(members.get(&base).is_some_and(|(kind, _, _)| kind.is_dir()), "Incus filesystem payload missing");
            roots.insert(base);
        }
    }
    let section = |name: &str| roots.iter().find(|root| name == root.as_str() || name.strip_prefix(root.as_str()).is_some_and(|rest| rest.starts_with('/')));
    for (name, (kind, _, link)) in &members {
        let mut parent = std::path::Path::new(name).parent();
        while let Some(p) = parent.filter(|p| !p.as_os_str().is_empty()) {
            ensure!(members.get(p.to_str().unwrap_or("")).is_none_or(|(kind, _, _)| kind.is_dir()), "Incus archive parent is not a directory");
            parent = p.parent();
        }
        if name == "backup/index.yaml" || blocks.contains(name) { continue; }
        if (name == "backup" || name == &format!("backup/{snapshot_prefix}")) && kind.is_dir() { continue; }
        let root = section(name).ok_or_else(|| anyhow!("unexpected Incus archive payload"))?;
        if kind.is_hard_link() {
            let target = link.as_deref().ok_or_else(|| anyhow!("missing Incus hardlink target"))?;
            ensure!(section(target) == Some(root) && members.get(target).is_some_and(|(kind, _, _)| kind.is_file()), "invalid Incus archive hardlink target");
        }
    }
    Ok(coverage)
}

fn rows(value: &Value) -> Result<&[Value]> {
    if value.is_null() { return Ok(&[]); }
    let values = value.as_array().filter(|v| v.len() <= 2048).ok_or_else(|| anyhow!("invalid Incus archive records"))?;
    Ok(values)
}
fn empty_rows(value: &Value) -> Result<bool> { Ok(rows(value)?.is_empty()) }

// Offline bundle verification: every disk in each guest generation resolves
// to exactly one separately captured volume, with an exact consumer set.
pub(crate) fn verify_bundle_dependencies(c: &Config, exports: &[NativeExport], root: &File, end: Instant) -> Result<()> {
    let mut volumes: BTreeMap<usize, BTreeSet<String>> = BTreeMap::new();
    let mut identities = BTreeSet::new();
    for (at, export) in exports.iter().enumerate().filter(|(_, e)| e.backend == "incus" && e.kind == "incus_custom_volume") {
        ensure!(identities.insert((&export.native.project, &export.native.pool, &export.native.name)), "duplicate Incus volume identity");
        volumes.insert(at, BTreeSet::new());
    }
    for export in exports.iter().filter(|e| e.backend == "incus") {
        let coverage = inspect_archive(c, export, capture::read_export(root, &export.bundle_path)?, end)?;
        if export.kind != "incus_instance_export" { continue; }
        resolve_references(exports, &mut volumes, export, coverage.references, end)?;
    }
    for (at, actual) in volumes {
        let expected: BTreeSet<_> = exports[at].resources.iter().cloned().collect();
        ensure!(expected == actual, "Incus custom volume consumer coverage differs");
    }
    Ok(())
}

fn resolve_references(exports: &[NativeExport], volumes: &mut BTreeMap<usize,BTreeSet<String>>,
    export: &NativeExport, references: References, end: Instant) -> Result<()> {
    let consumer = &export.resources[0];
    for (pool, name) in references {
            capture::check_time(end)?;
            let mut matches = volumes.keys().copied().filter(|at| {
                let candidate = &exports[*at];
                candidate.native.pool == pool && candidate.native.name == name
                    && (candidate.native.project == export.native.project || candidate.native.project == "default")
                    && candidate.resources.contains(consumer)
            });
            let at = matches.next().ok_or_else(|| anyhow!("Incus custom volume capture missing"))?;
            ensure!(matches.next().is_none(), "ambiguous Incus custom volume capture");
            volumes.get_mut(&at).unwrap().insert(consumer.clone());
    }
    Ok(())
}

#[cfg(test)]
fn verify_archive(c: &Config, id: &str, object: &NativeObject, file: File, end: Instant) -> Result<()> {
    let export = NativeExport { resources: vec![format!("service/{id}")], backend: "incus".into(),
        kind: "incus_instance_export".into(), bundle_path: "native/incus/test.tar".into(), size_bytes: file.metadata()?.len(),
        sha256: String::new(), native: object.clone() };
    inspect_archive(c, &export, file, end).map(|_| ())
}
#[cfg(test)]
mod tests{
    use super::*;
    use std::io::{Seek,SeekFrom};
    #[test]
    fn disk_references_include_whole_volume_and_reject_external_data() {
        let devices: Devices = serde_json::from_value(serde_json::json!({
            "root":{"type":"disk","path":"/","pool":"root-pool"},
            "data":{"type":"disk","pool":"data-pool","source":"volume/subdir","path":"/data"},
            "alias":{"type":"disk","pool":"data-pool","source":"volume","path":"/alias"}
        })).unwrap();
        let disks = guest_disks(&devices).unwrap();
        assert_eq!(disks.root, "root-pool");
        assert_eq!(disks.volumes, BTreeSet::from([("data-pool".into(), "volume".into())]));
        let mut bad = devices;
        bad.get_mut("data").unwrap().remove("pool");
        assert!(guest_disks(&bad).is_err());
    }
    #[test]
    fn portable_archive_must_contain_reviewed_guest_and_root_disk(){
        let config=super::super::super::parse(br#"version: 1
bindings:
  lan: {kind: network, name: default}
  pool: {kind: pool, name: default}
  project: {kind: project, name: managed}
services:
  guest:
    kind: vm
    backend: incus
    vm:
      image: ubuntu-24.04
      uuid: "c3d699a2-921d-4009-8b63-1d0144fbb7dc"
      mac: "52:54:00:12:34:56"
      cpus: 2
      memory_mib: 1024
      network: lan
      pool: pool
      project: project
"#).unwrap();
        let object=NativeObject{backend:"incus".into(),name:"maestro-guest".into(),project:"managed".into(),pool:"default".into(),content_type:"block".into()};
        let want=render(&config,"guest").unwrap();
        let index=serde_json::json!({
            "name":"maestro-guest","pool":"default","backend":"dir","type":"virtual-machine",
            "optimized":false,"optimized_header":false,"snapshots":[],
            "config":{
                "container":{"name":"maestro-guest","type":"virtual-machine","config":want.config,"devices":want.devices,"profiles":[],"stateful":false},
                "volume":{"name":"maestro-guest","type":"virtual-machine","content_type":"block","project":"managed"},
                "pool":{"name":"default","driver":"dir"},"dependent_volumes":[]
            }
        });
        let make=|include_disk:bool,index:&Value|{
            let file=tempfile::tempfile().unwrap();
            let mut archive=tar::Builder::new(file);
            let yaml=serde_yaml::to_string(index).unwrap();
            let mut h=tar::Header::new_gnu();h.set_size(yaml.len() as u64);h.set_mode(0o600);h.set_cksum();
            archive.append_data(&mut h,"backup/index.yaml",yaml.as_bytes()).unwrap();
            let mut h=tar::Header::new_gnu();h.set_entry_type(tar::EntryType::Directory);h.set_size(0);h.set_mode(0o700);h.set_cksum();
            archive.append_data(&mut h,"backup/virtual-machine/",std::io::empty()).unwrap();
            if include_disk{
                let mut h=tar::Header::new_gnu();h.set_size(4);h.set_mode(0o600);h.set_cksum();
                archive.append_data(&mut h,"backup/virtual-machine.img",&b"disk"[..]).unwrap();
            }
            let mut file=archive.into_inner().unwrap();file.seek(SeekFrom::Start(0)).unwrap();file
        };
        let end=Instant::now()+Duration::from_secs(10);
        assert!(verify_archive(&config,"guest",&object,make(true,&index),end).is_ok());
        assert!(verify_archive(&config,"guest",&object,make(false,&index),end).is_err());
        let mut historical=index.clone();
        let mut snapshot=historical["config"]["container"].clone();
        snapshot["name"]=serde_json::json!("before");
        snapshot["devices"]["root"]["pool"]=serde_json::json!("previous-root");
        snapshot["devices"]["old-data"]=serde_json::json!({"type":"disk","pool":"data","source":"removed/subpath","path":"/data"});
        let (names, refs)=snapshot_records(&[snapshot.clone()],&config,"guest").unwrap();
        assert_eq!(names,BTreeSet::from(["before".into()]));
        assert_eq!(refs,BTreeSet::from([("data".into(),"removed".into())]));
        assert!(snapshot_records(&[snapshot.clone(),snapshot.clone()],&config,"guest").is_err());
        historical["snapshots"]=serde_json::json!(["before"]);
        historical["config"]["snapshots"]=serde_json::json!([snapshot]);
        historical["config"]["volume_snapshots"]=serde_json::json!([{"name":"before","content_type":"block","project":"managed"}]);
        assert!(verify_archive(&config,"guest",&object,make(true,&historical),end).is_err()); // missing snapshot payload
        let mut file=make(true,&historical);
        // Build a complete archive with both the root and historical VM disks.
        file.set_len(0).unwrap();file.seek(SeekFrom::Start(0)).unwrap();
        let mut builder=tar::Builder::new(file);
        let yaml=serde_yaml::to_string(&historical).unwrap();
        let mut h=tar::Header::new_gnu();h.set_size(yaml.len() as u64);h.set_mode(0o600);h.set_cksum();
        builder.append_data(&mut h,"backup/index.yaml",yaml.as_bytes()).unwrap();
        for base in ["backup/virtual-machine","backup/virtual-machine-snapshots/before"] {
            let mut h=tar::Header::new_gnu();h.set_entry_type(tar::EntryType::Directory);h.set_size(0);h.set_mode(0o700);h.set_cksum();
            builder.append_data(&mut h,base,std::io::empty()).unwrap();
            let mut h=tar::Header::new_gnu();h.set_size(4);h.set_mode(0o600);h.set_cksum();
            builder.append_data(&mut h,format!("{base}.img"),&b"disk"[..]).unwrap();
        }
        let mut file=builder.into_inner().unwrap();file.seek(SeekFrom::Start(0)).unwrap();
        let instance=NativeExport{resources:vec!["service/guest".into()],backend:"incus".into(),kind:"incus_instance_export".into(),bundle_path:"native/incus/guest.tar".into(),size_bytes:file.metadata().unwrap().len(),sha256:String::new(),native:object.clone()};
        let coverage=inspect_archive(&config,&instance,file,end).unwrap();
        assert_eq!(coverage.references,refs);
        let volume=NativeExport{resources:vec!["service/guest".into()],backend:"incus".into(),kind:"incus_custom_volume".into(),bundle_path:"native/incus/volume.tar".into(),size_bytes:1,sha256:String::new(),native:NativeObject{backend:"incus".into(),name:"removed".into(),pool:"data".into(),project:"default".into(),content_type:"filesystem".into()}};
        let mut volumes=BTreeMap::new();
        assert!(resolve_references(&[],&mut volumes,&instance,refs.clone(),end).is_err());
        let exports=vec![volume.clone()];
        volumes.insert(0,BTreeSet::new());
        resolve_references(&exports,&mut volumes,&instance,refs.clone(),end).unwrap();
        assert_eq!(volumes[&0],BTreeSet::from(["service/guest".into()]));
        let mut duplicate=volume.clone();duplicate.native.project="managed".into();
        volumes.insert(1,BTreeSet::new());
        assert!(resolve_references(&[volume.clone(),duplicate],&mut volumes,&instance,refs,end).is_err());
        // A custom filesystem volume must include all indexed snapshot trees.
        let vol_index=serde_json::json!({"name":"removed","pool":"data","backend":"dir","type":"custom","optimized":false,"optimized_header":false,"snapshots":["snap"],"config":{"volume":{"name":"removed","type":"custom","content_type":"filesystem","project":"default"},"volume_snapshots":[{"name":"snap","content_type":"filesystem"}]}});
        for include_snapshot in [false,true] {
            let file=tempfile::tempfile().unwrap();
            let mut builder=tar::Builder::new(file);
            let yaml=serde_yaml::to_string(&vol_index).unwrap();
            let mut h=tar::Header::new_gnu();h.set_size(yaml.len() as u64);h.set_mode(0o600);h.set_cksum();
            builder.append_data(&mut h,"backup/index.yaml",yaml.as_bytes()).unwrap();
            for path in ["backup/volume","backup/volume-snapshots/snap"].into_iter().take(if include_snapshot{2}else{1}) {
                let mut h=tar::Header::new_gnu();h.set_entry_type(tar::EntryType::Directory);h.set_size(0);h.set_mode(0o700);h.set_cksum();
                builder.append_data(&mut h,path,std::io::empty()).unwrap();
            }
            let mut file=builder.into_inner().unwrap();file.seek(SeekFrom::Start(0)).unwrap();
            assert_eq!(inspect_archive(&config,&volume,file,end).is_ok(),include_snapshot);
        }
        let mut wrong=index;wrong["config"]["container"]["config"][IDENTITY]=serde_json::json!("foreign");
        assert!(verify_archive(&config,"guest",&object,make(true,&wrong),end).is_err());
    }
}
