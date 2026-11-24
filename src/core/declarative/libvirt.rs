//! Direct port of the reference implementation libvirt.go. Native commands always address a
//! verified persistent UUID on an explicitly selected local connection.
use super::{
    ApplyMode, ApplyProvider, Change, Config, ObservedResource, Operation, RecoveryProvider,
    Service,
    artifacts::{check_bytes, publish_artifact, read_artifact},
    libvirt_render::{self as xml, Domain},
};
use crate::command::CommandRunner;
use anyhow::{Result, anyhow, ensure};
use std::{path::PathBuf, time::Duration};

#[path = "libvirt_backup.rs"]
mod backup;

pub struct LibvirtProvider {
    pub virsh: PathBuf,
    pub uri: Option<String>,
    pub runner: CommandRunner,
    validator: PathBuf,
    directory: PathBuf,
    storage_base: PathBuf,
}
pub struct LibvirtStatus {
    pub state: &'static str,
    pub runtime_matches: bool,
    pub restart_required: bool,
}
struct Observation {
    state: String,
    persistent: Domain,
    active: Option<Domain>,
    enabled: bool,
}
impl Observation {
    fn matches(&self, want: &Domain, enabled: bool) -> bool {
        want.hardware_matches(&self.persistent)
            && self.persistent.fingerprint == want.fingerprint
            && self.persistent.runtime == want.runtime
            && self.enabled == enabled
    }
    fn runtime_matches(&self, want: &Domain) -> bool {
        self.active
            .as_ref()
            .is_none_or(|active| want.hardware_matches(active) && active.runtime == want.runtime)
    }
}
struct Candidate(PathBuf);
impl Candidate {
    fn new(data: &[u8]) -> Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        let path =
            std::env::temp_dir().join(format!("maestro-domain-validate-{}", uuid::Uuid::new_v4()));
        std::fs::DirBuilder::new().mode(0o700).create(&path)?;
        let candidate = Self(path);
        let directory = super::store::open_directory(&candidate.0)?;
        super::store::publish(&directory, "domain.xml", data, true)?;
        Ok(candidate)
    }
    fn file(&self) -> PathBuf {
        self.0.join("domain.xml")
    }
}
impl Drop for Candidate {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.file());
        let _ = std::fs::remove_dir(&self.0);
    }
}
impl LibvirtProvider {
    pub fn new(virsh: PathBuf, runner: CommandRunner) -> Self {
        Self {
            virsh,
            runner,
            uri: Some("qemu:///system".into()),
            validator: "/usr/bin/virt-xml-validate".into(),
            directory: PathBuf::new(),
            storage_base: PathBuf::new(),
        }
    }
    pub fn native_paths(mut self, directory: PathBuf, storage_base: PathBuf) -> Self {
        self.directory = directory.join("libvirt");
        self.storage_base = storage_base;
        self
    }
    pub fn validator(mut self, path: PathBuf) -> Self {
        self.validator = path;
        self
    }
    pub fn matches(&self, service: &Service) -> bool {
        service.kind == "vm" && service.backend == "libvirt"
    }
    fn connection(&self) -> Result<&str> {
        let uri = self
            .uri
            .as_deref()
            .ok_or_else(|| anyhow!("Libvirt connection is not configured"))?;
        ensure!(
            matches!(uri, "qemu:///system" | "qemu:///session"),
            "only local Libvirt connections are supported"
        );
        ensure!(self.virsh.is_absolute(), "virsh must be an absolute path");
        Ok(uri)
    }
    async fn command(
        &self,
        args: &[&str],
        seconds: u64,
        limit: usize,
        discard: bool,
    ) -> Result<Vec<u8>> {
        let mut argv = vec!["--connect", self.connection()?];
        argv.extend_from_slice(args);
        let output = self
            .runner
            .capture_scoped(
                &self.virsh,
                &argv,
                Duration::from_secs(seconds),
                limit,
                discard,
                &[("LC_ALL", "C")],
                None,
                &["LIBVIRT_DEFAULT_URI", "VIRSH_DEFAULT_CONNECT_URI"],
            )
            .await?;
        ensure!(output.status.success(), "native Libvirt command failed");
        Ok(output.stdout)
    }
    pub async fn check_prerequisites(&self) -> Result<()> {
        ensure!(
            self.validator.is_absolute() && self.validator.is_file(),
            "virt-xml-validate is required"
        );
        let uri = self
            .command(&["uri"], 5, 4096, false)
            .await
            .map_err(|_| anyhow!("configured Libvirt connection is unavailable"))?;
        ensure!(
            String::from_utf8_lossy(&uri).trim() == self.connection()?,
            "Libvirt connection identity differs"
        );
        let caps = self
            .command(
                &[
                    "domcapabilities",
                    "--virttype",
                    "kvm",
                    "--arch",
                    "x86_64",
                    "--machine",
                    "q35",
                ],
                5,
                1024 * 1024,
                false,
            )
            .await
            .map_err(|_| {
                anyhow!(
                    "configured Libvirt connection cannot provide the supported KVM guest profile"
                )
            })?;
        ensure!(
            xml::valid_kvm_capabilities(&caps),
            "configured Libvirt connection returned unrecognized KVM capabilities"
        );
        Ok(())
    }
    pub(super) fn recovery_targets(
        &self,
        previous: &Config,
        desired: &Config,
        id: &str,
    ) -> Result<Vec<super::artifact_recovery::Target>> {
        Ok(vec![super::artifact_recovery::Target::dedicated(
            "Native libvirt definition",
            self.directory(id),
            "domain.xml",
            self.artifact(previous, id)?,
            self.artifact(desired, id)?,
        )?])
    }
    fn artifact(&self, config: &Config, id: &str) -> Result<Option<Vec<u8>>> {
        if config.services.contains_key(id) {
            Ok(Some(
                xml::render(config, id, &self.storage_base)?.into_bytes(),
            ))
        } else {
            Ok(None)
        }
    }
    fn directory(&self, id: &str) -> PathBuf {
        self.directory.join(id)
    }
    fn file(&self, id: &str) -> PathBuf {
        self.directory(id).join("domain.xml")
    }
    async fn validate_candidate(&self, data: &[u8]) -> Result<()> {
        ensure!(
            self.validator.is_absolute() && self.validator.is_file(),
            "virt-xml-validate is required"
        );
        let candidate = Candidate::new(data)?;
        let file = candidate.file();
        let file = file
            .to_str()
            .ok_or_else(|| anyhow!("invalid validation path"))?;
        let output = self
            .runner
            .capture_scoped(
                &self.validator,
                &[file, "domain"],
                Duration::from_secs(10),
                0,
                true,
                &[("LC_ALL", "C")],
                None,
                &[],
            )
            .await
            .map_err(|_| anyhow!("native Libvirt schema validation failed"))?;
        ensure!(
            output.status.success(),
            "native Libvirt schema validation failed"
        );
        Ok(())
    }
    pub async fn external_inventory(
        &self,
        config: &Config,
    ) -> Result<Vec<super::native_inventory::NativeWorkload>> {
        use super::native_inventory::{NativeWorkload, reserved, valid_text};
        use futures_util::{StreamExt, TryStreamExt, stream};
        let data = self
            .command(&["list", "--all", "--uuid"], 5, 256 * 1024, false)
            .await?;
        let ids = std::str::from_utf8(&data)?
            .split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        ensure!(ids.len() <= 128, "native Libvirt inventory exceeds limit");
        let scope = self.connection()?.to_owned();
        let workloads =
            stream::iter(ids.into_iter().map(|id| {
                let scope = scope.clone();
                async move {
                    ensure!(
                        uuid::Uuid::parse_str(&id).is_ok(),
                        "invalid native domain UUID"
                    );
                    if config.services.values().any(|s| {
                        s.backend == "libvirt" && s.vm.as_ref().is_some_and(|v| v.uuid == id)
                    }) {
                        return Ok::<Option<NativeWorkload>, anyhow::Error>(None);
                    }
                    let name = self.command(&["domname", &id], 5, 4096, false).await?;
                    let name = std::str::from_utf8(&name)?.trim();
                    if !valid_text(name) || reserved(name) {
                        return Ok(None);
                    }
                    let xml = self
                        .command(
                            &["dumpxml", "--domain", &id],
                            5,
                            super::MAX_CONFIG_BYTES,
                            false,
                        )
                        .await?;
                    if std::str::from_utf8(&xml)?.contains(xml::NS) {
                        return Ok(None);
                    }
                    let state = self.state(&id).await?;
                    let actions = match state.as_str() {
                        "running" => vec!["stop", "restart"],
                        "shut off" => vec!["start", "delete"],
                        _ => vec![],
                    };
                    Ok(Some(NativeWorkload {
                        provider: "libvirt".into(),
                        scope,
                        id: id.clone(),
                        name: name.into(),
                        kind: "virtual-machine".into(),
                        state,
                        image: String::new(),
                        addresses: vec![],
                        actions,
                        logs: false,
                        identity: id,
                    }))
                }
            }))
            .buffer_unordered(8)
            .try_collect::<Vec<_>>();
        let result = tokio::time::timeout(std::time::Duration::from_secs(12), workloads).await??;
        Ok(result.into_iter().flatten().collect())
    }
    pub async fn external_action(
        &self,
        config: &Config,
        id: &str,
        scope: &str,
        action: &str,
    ) -> Result<String> {
        ensure!(
            !self.runner.is_test_mode()
                && self.connection()? == scope
                && uuid::Uuid::parse_str(id).is_ok(),
            "invalid local Libvirt identity"
        );
        ensure!(
            matches!(action, "start" | "stop" | "restart" | "delete"),
            "unsupported external action"
        );
        ensure!(
            !config
                .services
                .values()
                .any(|s| s.backend == "libvirt" && s.vm.as_ref().is_some_and(|v| v.uuid == id)),
            "managed domain"
        );
        let name = self.command(&["domname", id], 5, 4096, false).await?;
        let name = std::str::from_utf8(&name)?.trim();
        ensure!(
            super::native_inventory::valid_text(name) && !super::native_inventory::reserved(name),
            "reserved native domain"
        );
        let data = self
            .command(
                &["dumpxml", "--domain", id],
                5,
                super::MAX_CONFIG_BYTES,
                false,
            )
            .await?;
        ensure!(
            !std::str::from_utf8(&data)?.contains(xml::NS),
            "managed domain metadata"
        );
        let state = self.state(id).await?;
        let command = match (action, state.as_str()) {
            ("start", "shut off") => "start",
            ("stop", "running") => "shutdown",
            ("restart", "running") => "reboot",
            ("delete", "shut off") => {
                // Plain undefine only: do not force shutdown, remove storage,
                // snapshots, managed-save state, or firmware/NVRAM.
                let info = self
                    .command(&["dominfo", "--domain", id], 5, 65536, false)
                    .await?;
                ensure!(
                    std::str::from_utf8(&info)?.lines().any(|line| line
                        .split_once(':')
                        .is_some_and(
                            |(key, val)| key.trim() == "Persistent" && val.trim() == "yes"
                        )),
                    "domain is not persistent"
                );
                let snaps = self
                    .command(
                        &["snapshot-list", "--domain", id, "--name"],
                        5,
                        65536,
                        false,
                    )
                    .await?;
                ensure!(
                    std::str::from_utf8(&snaps)?.trim().is_empty(),
                    "domain has snapshots"
                );
                ensure!(self.state(id).await? == "shut off", "domain state changed");
                "undefine"
            }
            _ => return Err(anyhow!("action unavailable for native domain state")),
        };
        self.command(&[command, "--domain", id], 120, 4096, false)
            .await?;
        Ok(format!(
            "Libvirt {command} requested; refresh to confirm guest state"
        ))
    }
    async fn exists(&self, id: &str, uuid: &str) -> Result<bool> {
        let uuids = self
            .command(&["list", "--all", "--uuid"], 5, 256 * 1024, false)
            .await
            .map_err(|_| anyhow!("Libvirt inventory is unavailable"))?;
        let uuids = std::str::from_utf8(&uuids)?;
        if uuids.split_whitespace().any(|line| line == uuid) {
            return Ok(true);
        }
        let names = self
            .command(&["list", "--all", "--name"], 5, 256 * 1024, false)
            .await
            .map_err(|_| anyhow!("Libvirt name inventory is unavailable"))?;
        ensure!(
            !std::str::from_utf8(&names)?
                .lines()
                .any(|name| name.trim() == xml::domain_name(id)),
            "an existing Libvirt domain conflicts with this managed name"
        );
        Ok(false)
    }
    async fn read_domain(&self, id: &str, uuid: &str, inactive: bool) -> Result<Domain> {
        let mut args = vec!["dumpxml", "--domain", uuid];
        if inactive {
            args.push("--inactive");
        }
        let data = self
            .command(&args, 5, super::MAX_CONFIG_BYTES, false)
            .await
            .map_err(|_| anyhow!("Libvirt domain definition cannot be read"))?;
        let domain = xml::parse_domain(&data)?;
        ensure!(
            domain.owned(id, uuid),
            "Libvirt domain is not owned by this managed definition"
        );
        Ok(domain)
    }
    async fn state(&self, uuid: &str) -> Result<String> {
        let state = self
            .command(&["domstate", "--domain", uuid], 5, 4096, false)
            .await
            .map_err(|_| anyhow!("Libvirt state cannot be read"))?;
        let state = std::str::from_utf8(&state)?.trim().to_owned();
        ensure!(
            matches!(
                state.as_str(),
                "shut off"
                    | "running"
                    | "blocked"
                    | "paused"
                    | "in shutdown"
                    | "crashed"
                    | "pmsuspended"
                    | "idle"
            ),
            "unrecognized Libvirt guest state"
        );
        Ok(state)
    }
    async fn inspect(&self, id: &str, uuid: &str) -> Result<Option<Observation>> {
        if !self.exists(id, uuid).await? {
            return Ok(None);
        }
        let persistent = self.read_domain(id, uuid, true).await?;
        let state = self.state(uuid).await?;
        let info = self
            .command(&["dominfo", "--domain", uuid], 5, 64 * 1024, false)
            .await
            .map_err(|_| anyhow!("Libvirt boot policy cannot be read"))?;
        let info = std::str::from_utf8(&info)?;
        // Transient guests are not owned persistent definitions.
        ensure!(
            !info.lines().any(|line| line
                .split_once(':')
                .is_some_and(|(k, v)| k.trim() == "Persistent" && v.trim() != "yes")),
            "guest is not persistent"
        );
        let enabled = match info.lines().find_map(|line| {
            line.split_once(':')
                .filter(|(k, _)| k.trim() == "Autostart")
                .map(|(_, v)| v.trim())
        }) {
            Some("enable" | "enabled") => true,
            Some("disable" | "disabled") => false,
            _ => return Err(anyhow!("Libvirt boot policy cannot be read")),
        };
        let active = if state != "shut off" {
            Some(self.read_domain(id, uuid, false).await?)
        } else {
            None
        };
        Ok(Some(Observation {
            persistent,
            state,
            enabled,
            active,
        }))
    }
    fn identity<'a>(&self, id: &str, previous: &'a Config, desired: &'a Config) -> Result<&'a str> {
        let old = previous.services.get(id);
        let next = desired.services.get(id);
        let identity = next
            .or(old)
            .ok_or_else(|| anyhow!("VM identity unavailable"))?;
        ensure!(
            self.matches(identity),
            "resource belongs to another provider"
        );
        let vm = identity
            .vm
            .as_ref()
            .ok_or_else(|| anyhow!("VM identity unavailable"))?;
        ensure!(
            !vm.uuid.is_empty() && uuid::Uuid::parse_str(&vm.uuid).is_ok(),
            "persistent VM identity is required"
        );
        if let (Some(old), Some(next)) = (old, next) {
            let a = old
                .vm
                .as_ref()
                .ok_or_else(|| anyhow!("previous VM identity unavailable"))?;
            let b = next
                .vm
                .as_ref()
                .ok_or_else(|| anyhow!("VM identity unavailable"))?;
            ensure!(
                self.matches(old) && self.matches(next) && a.uuid == b.uuid && a.mac == b.mac,
                "existing VM identity cannot be changed"
            );
        }
        Ok(&vm.uuid)
    }
    fn check_change<'a>(&self, change: &'a Change) -> Result<&'a str> {
        let id = change
            .resource
            .strip_prefix("service/")
            .filter(|id| super::id_valid(id))
            .ok_or_else(|| anyhow!("invalid Libvirt service ID"))?;
        ensure!(
            change.backend == "libvirt",
            "resource belongs to another provider"
        );
        Ok(id)
    }
    async fn preflight_state(
        &self,
        change: &Change,
        previous: &Config,
        desired: &Config,
        mode: ApplyMode,
    ) -> Result<(Option<Vec<u8>>, Option<Vec<u8>>, Option<Observation>)> {
        let id = self.check_change(change)?;
        let uuid = self.identity(id, previous, desired)?;
        let before = self.artifact(previous, id)?;
        let after = self.artifact(desired, id)?;
        let old = before.as_deref().map(xml::parse_domain).transpose()?;
        let next = after.as_deref().map(xml::parse_domain).transpose()?;
        self.check_prerequisites().await?;
        let actual = read_artifact(&self.directory(id), "domain.xml")?;
        check_bytes(actual.as_deref(), before.as_deref(), after.as_deref())?;
        let obs = self.inspect(id, uuid).await?;
        if let Some(ref obs) = obs {
            // Exact managed hardware + full reference fingerprint, not just a
            // name or metadata marker, protects externally edited guests.
            let known = |domain: &Domain| {
                domain.hardware_matches(&obs.persistent)
                    && domain.fingerprint == obs.persistent.fingerprint
                    && domain.runtime == obs.persistent.runtime
            };
            ensure!(
                old.as_ref().is_some_and(known) || next.as_ref().is_some_and(known),
                "persistent Libvirt definition differs from reviewed state"
            );
            ensure!(
                before.is_some() || actual.as_deref() == after.as_deref(),
                "preexisting domain has no owned prior artifact"
            );
            if after.is_none() {
                ensure!(
                    obs.state == "shut off",
                    "shut down VM before removing its definition"
                );
            } else if mode == ApplyMode::Now && obs.active.is_some() {
                ensure!(
                    obs.runtime_matches(next.as_ref().unwrap()),
                    "running VM changes require shutdown/start or Save for next start"
                );
            }
        }
        if after.is_none() {
            ensure!(
                mode == ApplyMode::Now,
                "removing a VM requires immediate apply"
            );
        }
        if let Some(data) = &after {
            self.validate_candidate(data).await?;
        }
        Ok((before, after, obs))
    }
    async fn native_apply(
        &self,
        change: &Change,
        previous: &Config,
        desired: &Config,
        mode: ApplyMode,
    ) -> Result<()> {
        let id = self.check_change(change)?;
        let uuid = self.identity(id, previous, desired)?;
        let (before, after, obs) = self
            .preflight_state(change, previous, desired, mode)
            .await?;
        if after.is_none() {
            if obs.is_some() {
                // Recheck current state immediately before undefine. Never
                // force-stop or request storage, NVRAM or TPM deletion.
                ensure!(
                    self.state(uuid).await? == "shut off",
                    "shut down VM before removing its definition"
                );
                self.command(
                    &["undefine", "--domain", uuid, "--keep-nvram", "--keep-tpm"],
                    60,
                    0,
                    true,
                )
                .await
                .map_err(|_| {
                    anyhow!("domain removal failed; disks were not requested for deletion")
                })?;
            }
            ensure!(
                !self.exists(id, uuid).await?,
                "Libvirt domain remains registered"
            );
            publish_artifact(&self.directory(id), "domain.xml", before.as_deref(), None)?;
        } else {
            publish_artifact(
                &self.directory(id),
                "domain.xml",
                before.as_deref(),
                after.as_deref(),
            )?;
            let file = self.file(id);
            let file = file
                .to_str()
                .ok_or_else(|| anyhow!("invalid Libvirt definition path"))?;
            self.command(&["define", file, "--validate"], 60, 0, true)
                .await
                .map_err(|_| anyhow!("persistent Libvirt definition failed"))?;
            let mut args = vec!["autostart", "--domain", uuid];
            if !desired.services[id].enabled {
                args.push("--disable");
            }
            self.command(&args, 5, 0, true)
                .await
                .map_err(|_| anyhow!("Libvirt boot policy update failed"))?;
        }
        ensure!(
            self.native_observe(change, previous, desired, mode).await? == ObservedResource::After,
            "Libvirt publication could not be verified"
        );
        Ok(())
    }
    async fn native_observe(
        &self,
        change: &Change,
        previous: &Config,
        desired: &Config,
        mode: ApplyMode,
    ) -> Result<ObservedResource> {
        let id = self.check_change(change)?;
        let uuid = self.identity(id, previous, desired)?;
        let obs = self.inspect(id, uuid).await?;
        let actual = read_artifact(&self.directory(id), "domain.xml")?;
        let matches = |config: &Config, mode: ApplyMode| -> Result<bool> {
            let artifact = self.artifact(config, id)?;
            if actual.as_deref() != artifact.as_deref() {
                return Ok(false);
            }
            match (&obs, artifact) {
                (None, None) => Ok(true),
                (Some(obs), Some(data)) => {
                    let want = xml::parse_domain(&data)?;
                    Ok(obs.matches(&want, config.services[id].enabled)
                        && (mode == ApplyMode::NextStart || obs.runtime_matches(&want)))
                }
                _ => Ok(false),
            }
        };
        if matches(desired, mode)? {
            Ok(ObservedResource::After)
        } else if matches(previous, ApplyMode::NextStart)? {
            Ok(ObservedResource::Before)
        } else {
            Ok(ObservedResource::Unknown)
        }
    }
    async fn verified(&self, config: &Config, id: &str) -> Result<(Domain, Observation)> {
        ensure!(super::id_valid(id), "invalid Libvirt service");
        let data = self
            .artifact(config, id)?
            .ok_or_else(|| anyhow!("unknown Libvirt VM"))?;
        ensure!(
            read_artifact(&self.directory(id), "domain.xml")?.as_deref() == Some(data.as_slice()),
            "Libvirt artifact differs from authoritative definition"
        );
        let want = xml::parse_domain(&data)?;
        let obs = self
            .inspect(id, &want.uuid)
            .await?
            .ok_or_else(|| anyhow!("Libvirt VM is not registered"))?;
        ensure!(
            obs.matches(&want, config.services[id].enabled),
            "persistent Libvirt definition or boot policy differs"
        );
        Ok((want, obs))
    }
    pub async fn status_details(&self, config: &Config, id: &str) -> Result<LibvirtStatus> {
        let (want, obs) = self.verified(config, id).await?;
        let state = match obs.state.as_str() {
            "running" => "running",
            "shut off" => "stopped",
            "paused" => "paused",
            "crashed" => "failed",
            "in shutdown" => "stopping",
            _ => "unknown",
        };
        let runtime_matches = obs.runtime_matches(&want);
        Ok(LibvirtStatus {
            state,
            runtime_matches,
            restart_required: !runtime_matches,
        })
    }
    pub async fn verify_boot_policy(&self, config: &Config, id: &str) -> Result<()> {
        self.verified(config, id).await.map(|_| ())
    }
    async fn run_start(&self, id: &str, uuid: &str) -> Result<()> {
        self.command(&["start", "--domain", uuid], 60, 0, true)
            .await
            .map_err(|_| anyhow!("Libvirt start failed"))?;
        ensure!(
            self.state(uuid).await? == "running",
            "VM did not reach running state"
        );
        self.read_domain(id, uuid, false).await?;
        Ok(())
    }
    pub async fn native_action(&self, config: &Config, id: &str, action: &str) -> Result<String> {
        ensure!(
            matches!(action, "start" | "stop" | "restart"),
            "unsupported Libvirt action"
        );
        let (want, obs) = self.verified(config, id).await?;
        let uuid = &want.uuid;
        if action == "start" {
            if obs.state != "running" {
                self.run_start(id, uuid).await?;
            }
        } else {
            if obs.state != "shut off" {
                self.command(&["shutdown", "--domain", uuid], 5, 0, true)
                    .await
                    .map_err(|_| anyhow!("guest shutdown request failed"))?;
                let result = tokio::time::timeout(Duration::from_secs(90), async {
                    loop {
                        if self.state(uuid).await? == "shut off" {
                            return Ok::<(), anyhow::Error>(());
                        }
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                })
                .await
                .map_err(|_| anyhow!("guest did not shut down; force-stop was not requested"))?;
                result?;
            }
            if action == "restart" {
                self.verified(config, id).await?;
                self.run_start(id, uuid).await?;
            }
        }
        let status = self.status_details(config, id).await?;
        ensure!(
            if action == "stop" {
                status.state == "stopped"
            } else {
                status.state == "running" && status.runtime_matches
            },
            "guest did not reach the requested runtime state"
        );
        Ok(format!("Libvirt {action} completed"))
    }
    pub async fn action(&self, service_id: &str, action: &str) -> Result<String> {
        ensure!(
            self.runner.is_test_mode(),
            "native Libvirt action requires authoritative configuration"
        );
        ensure!(
            super::id_valid(service_id)
                && matches!(action, "start" | "stop" | "restart" | "shutdown" | "reboot"),
            "unsupported Libvirt action"
        );
        Ok(format!("SIMULATED: Libvirt {action} on {service_id}"))
    }
}

impl ApplyProvider for LibvirtProvider {
    async fn preflight(&self, change: &Change, previous: &Config, desired: &Config) -> Result<()> {
        self.preflight_mode(change, previous, desired, ApplyMode::Now)
            .await
    }
    async fn apply(&self, change: &Change, previous: &Config, desired: &Config) -> Result<()> {
        self.apply_mode(change, previous, desired, ApplyMode::Now)
            .await
    }
    async fn preflight_mode(
        &self,
        change: &Change,
        previous: &Config,
        desired: &Config,
        mode: ApplyMode,
    ) -> Result<()> {
        self.check_change(change)?;
        if self.runner.is_test_mode() {
            return Ok(());
        }
        self.preflight_state(change, previous, desired, mode)
            .await
            .map(|_| ())
    }
    async fn apply_mode(
        &self,
        change: &Change,
        previous: &Config,
        desired: &Config,
        mode: ApplyMode,
    ) -> Result<()> {
        self.check_change(change)?;
        if self.runner.is_test_mode() {
            return Ok(());
        }
        self.native_apply(change, previous, desired, mode).await
    }
}
impl RecoveryProvider for LibvirtProvider {
    async fn observe(
        &self,
        change: &Change,
        previous: &Config,
        desired: &Config,
    ) -> Result<ObservedResource> {
        self.observe_mode(change, previous, desired, ApplyMode::Now)
            .await
    }
    async fn observe_mode(
        &self,
        change: &Change,
        previous: &Config,
        desired: &Config,
        mode: ApplyMode,
    ) -> Result<ObservedResource> {
        if self.runner.is_test_mode() {
            return Ok(if change.operation == Operation::Create {
                ObservedResource::Before
            } else {
                ObservedResource::After
            });
        }
        self.native_observe(change, previous, desired, mode).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Direct port of the reference recorder lifecycle tests. No real libvirt,
    // disk image, KVM device or host service is needed.
    #[tokio::test]
    async fn reference_define_defer_restart_and_remove() {
        use std::{fs, os::unix::fs::PermissionsExt};
        let tmp = tempfile::tempdir().unwrap();
        fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let log = tmp.path().join("commands");
        let persistent = tmp.path().join("persistent.xml");
        let active = tmp.path().join("active.xml");
        let running = tmp.path().join("running");
        let enabled = tmp.path().join("enabled");
        let fail = tmp.path().join("fail-shutdown");
        let binary = tmp.path().join("virsh");
        let validator = tmp.path().join("virt-xml-validate");
        let uuid = "c3d699a2-921d-4009-8b63-1d0144fbb7dc";
        let script = format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> '{log}'
[ "$1" = --connect ] && [ "$2" = qemu:///session ] || exit 1
shift 2
case "$1" in
 uri) echo qemu:///session;;
 domcapabilities) echo '<domainCapabilities><domain>kvm</domain><arch>x86_64</arch><machine>q35</machine></domainCapabilities>';;
 list) if [ -f '{persistent}' ]; then
  if [ "$3" = --uuid ]; then echo '{uuid}'; else echo maestro-guest; fi
 fi;;
 dumpxml) if [ "$4" != --inactive ] && [ -f '{running}' ]; then cat '{active}'; else cat '{persistent}'; fi;;
 domstate) if [ -f '{running}' ]; then echo running; else echo 'shut off'; fi;;
 dominfo) echo 'Persistent: yes'
 if [ -f '{enabled}' ]; then echo 'Autostart: enable'; else echo 'Autostart: disable'; fi;;
 define) cp "$2" '{persistent}';;
 autostart) if [ "$4" = --disable ]; then rm -f '{enabled}'; else : > '{enabled}'; fi;;
 start) cp '{persistent}' '{active}'; : > '{running}';;
 shutdown) [ ! -f '{fail}' ] || exit 1; rm -f '{running}';;
 undefine) [ ! -f '{running}' ] || exit 1; rm -f '{persistent}' '{enabled}';;
 *) exit 1;;
esac
"#,
            log = log.display(),
            persistent = persistent.display(),
            uuid = uuid,
            running = running.display(),
            active = active.display(),
            enabled = enabled.display(),
            fail = fail.display()
        );
        fs::write(&binary, script).unwrap();
        fs::write(
            &validator,
            "#!/bin/sh\n[ -f \"$1\" ] && [ \"$2\" = domain ]\n",
        )
        .unwrap();
        for path in [&binary, &validator] {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let disk = tmp.path().join("root.qcow2");
        fs::write(&disk, b"fixture disk; do not delete").unwrap();
        let mut provider = LibvirtProvider::new(binary, CommandRunner::new(false))
            .native_paths(tmp.path().join("generated"), tmp.path().into())
            .validator(validator);
        provider.uri = Some("qemu:///session".into());
        let config = super::super::parse(
            br#"version: 1
storage:
  root: {kind: file, format: qcow2, path: root.qcow2}
bindings:
  lan: {kind: network, name: default}
services:
  guest:
    kind: vm
    backend: libvirt
    enabled: false
    vm:
      uuid: "c3d699a2-921d-4009-8b63-1d0144fbb7dc"
      mac: "52:54:00:12:34:56"
      cpus: 2
      memory_mib: 1024
      network: lan
      disks: [root]
"#,
        )
        .unwrap();
        let empty = Config {
            version: 1,
            ..Default::default()
        };
        let change = |op, before: Option<&Config>, after: Option<&Config>| Change {
            resource: "service/guest".into(),
            backend: "libvirt".into(),
            operation: op,
            before: before.map(|c| super::super::plan::service_fingerprint(c, "guest").unwrap()),
            after: after.map(|c| super::super::plan::service_fingerprint(c, "guest").unwrap()),
        };
        let create = change(Operation::Create, None, Some(&config));
        provider.apply(&create, &empty, &config).await.unwrap();
        assert!(!running.exists());
        assert_eq!(
            provider
                .status_details(&config, "guest")
                .await
                .unwrap()
                .state,
            "stopped"
        );
        provider
            .native_action(&config, "guest", "start")
            .await
            .unwrap();
        let mut next = config.clone();
        next.services
            .get_mut("guest")
            .unwrap()
            .vm
            .as_mut()
            .unwrap()
            .memory_mib = 2048;
        let update = change(Operation::Update, Some(&config), Some(&next));
        assert!(provider.apply(&update, &config, &next).await.is_err());
        provider
            .apply_mode(&update, &config, &next, ApplyMode::NextStart)
            .await
            .unwrap();
        assert!(
            provider
                .status_details(&next, "guest")
                .await
                .unwrap()
                .restart_required
        );
        fs::write(&fail, b"fail").unwrap();
        let before = fs::read_to_string(&log).unwrap();
        assert!(
            provider
                .native_action(&next, "guest", "restart")
                .await
                .is_err()
        );
        assert!(
            !fs::read_to_string(&log).unwrap()[before.len()..]
                .lines()
                .any(|line| line.contains(" start ")
                    || line.contains(" destroy ")
                    || line.contains(" reboot "))
        );
        fs::remove_file(&fail).unwrap();
        provider
            .native_action(&next, "guest", "restart")
            .await
            .unwrap();
        assert!(
            provider
                .status_details(&next, "guest")
                .await
                .unwrap()
                .runtime_matches
        );
        let prior = next.clone();
        next.services.get_mut("guest").unwrap().enabled = true;
        let toggle = change(Operation::Update, Some(&prior), Some(&next));
        let before = fs::read_to_string(&log).unwrap();
        provider.apply(&toggle, &prior, &next).await.unwrap();
        assert!(enabled.exists() && running.exists());
        assert!(
            !fs::read_to_string(&log).unwrap()[before.len()..]
                .lines()
                .any(|line| line.contains(" shutdown ") || line.contains(" start "))
        );
        let remove = change(Operation::Remove, Some(&next), None);
        assert!(provider.apply(&remove, &next, &empty).await.is_err());
        provider
            .native_action(&next, "guest", "stop")
            .await
            .unwrap();
        let owned = fs::read_to_string(&persistent).unwrap();
        fs::write(
            &persistent,
            owned.replace(xml::NS, "https://foreign.invalid/"),
        )
        .unwrap();
        assert!(provider.apply(&remove, &next, &empty).await.is_err());
        fs::write(&persistent, &owned).unwrap();
        provider.apply(&remove, &next, &empty).await.unwrap();
        assert_eq!(
            provider.observe(&remove, &next, &empty).await.unwrap(),
            ObservedResource::After
        );
        assert_eq!(fs::read(&disk).unwrap(), b"fixture disk; do not delete");
        let commands = fs::read_to_string(&log).unwrap();
        assert!(commands.contains("undefine --domain"));
        assert!(commands.contains("--keep-nvram --keep-tpm"));
        assert!(!commands.contains("--remove-all-storage"));
        assert!(!commands.lines().any(|line| line.contains(" destroy ")));
    }

    #[tokio::test]
    async fn lifecycle_does_not_require_native_capture_in_simulation() {
        let provider = LibvirtProvider::new("/nonexistent/virsh".into(), CommandRunner::new(true));
        assert!(
            provider
                .action("demo", "restart")
                .await
                .unwrap()
                .starts_with("SIMULATED:")
        );
        assert!(provider.action("demo", "invalid").await.is_err());
    }
}
