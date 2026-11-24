use std::collections::BTreeSet;

use anyhow::{Result, anyhow, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    command::CommandRunner,
    config::AppConfig,
    core::declarative::{
        ApplyMode, ApplyProvider, Change, Config, ObservedResource, RecoveryProvider,
        compose::ComposeProvider,
        incus::IncusProvider,
        libvirt::LibvirtProvider,
        quadlet::{QuadletArtifacts, QuadletProvider},
        shares::ShareProvider,
        storage::StorageProvider,
    },
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationSummary {
    pub id: String,
    pub name: String,
    pub description: String,
    pub kind: String,
    pub engine: String,
    pub backend: String,
    pub enabled: bool,
    pub state: String,
    pub container_count: usize,
    pub runtime_matches: Option<bool>,
    pub restart_required: bool,
    pub members: Vec<super::quadlet::ContainerMemberStatus>,
    pub lifecycle: bool,
    pub logs: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareSummary {
    pub id: String,
    pub protocol: String,
    pub path: String,
    pub enabled: bool,
    pub read_only: bool,
    pub users: Vec<String>,
    pub clients: Vec<String>,
}

/// Structured log response matching the pinned reference; never included in inventory.
#[derive(Debug, Serialize)]
pub struct ManagedLogs {
    pub text: String,
    pub truncated: bool,
    pub source: &'static str,
}

impl ManagedLogs {
    pub fn display_text(self) -> String {
        if self.truncated {
            format!("[Earlier journal output truncated]\n{}", self.text)
        } else {
            self.text
        }
    }
}

pub struct CompositeProvider {
    simulated: bool,
    journalctl: std::path::PathBuf,
    pub quadlet: Option<QuadletProvider>,
    pub docker_compose: ComposeProvider,
    pub podman_compose: ComposeProvider,
    pub incus: IncusProvider,
    pub libvirt: LibvirtProvider,
    pub storage: StorageProvider,
    pub shares: ShareProvider,
    pub readiness: super::preflight::ReadinessCache,
}

impl CompositeProvider {
    pub fn new(config: &AppConfig, runner: CommandRunner) -> Self {
        let quadlet_artifacts = config.paths.quadlet_dir.as_ref().and_then(|directory| {
            QuadletArtifacts::new(directory.clone(), config.paths.storage_dir.clone()).ok()
        });
        let quadlet = quadlet_artifacts.and_then(|artifacts| {
            QuadletProvider::new(
                artifacts,
                runner.clone(),
                config.commands.systemctl.clone(),
                config.commands.podman.clone(),
            )
            .ok()
        });

        let docker_compose = ComposeProvider::new(
            "docker",
            config.commands.docker.clone(),
            config.paths.applications_dir.clone(),
            config.paths.storage_dir.clone(),
            runner.clone(),
        ).native_paths(config.commands.systemctl.clone());

        let podman_compose = ComposeProvider::new(
            "podman",
            config.commands.podman.clone(),
            config.paths.applications_dir.clone(),
            config.paths.storage_dir.clone(),
            runner.clone(),
        ).native_paths(config.commands.systemctl.clone());

        let incus = IncusProvider::new(config.commands.incus.clone(), runner.clone())
            .native_paths(config.paths.applications_dir.clone());
        let mut libvirt = LibvirtProvider::new(config.commands.virsh.clone(), runner.clone())
            .native_paths(config.paths.applications_dir.clone(),config.paths.storage_dir.clone());
        if let Some(authority) = config.linux_authority.as_ref() {
            libvirt.uri = Some(authority.settings.libvirt_uri.clone());
        }
        let storage = StorageProvider::new(config.paths.storage_dir.clone());
        let shares = ShareProvider::from_config(config,runner.is_test_mode());

        Self {
            simulated: runner.is_test_mode(),
            journalctl: config.commands.journalctl.clone(),
            quadlet,
            docker_compose,
            podman_compose,
            incus,
            libvirt,
            storage,
            shares,
            readiness: Default::default(),
        }
    }

    pub fn list_applications(&self, config: &Config) -> Vec<ApplicationSummary> {
        let mut apps = Vec::new();
        for (id, service) in &config.services {
            let state = if !self.simulated {
                "unknown".to_string()
            } else if service.enabled {
                "running".to_string()
            } else {
                "stopped".to_string()
            };
            apps.push(ApplicationSummary {
                id: id.clone(),
                name: if service.name.is_empty() {
                    id.clone()
                } else {
                    service.name.clone()
                },
                description: service.description.clone(),
                kind: service.kind.clone(),
                engine: service.engine.clone(),
                backend: service.backend.clone(),
                enabled: service.enabled,
                state,
                container_count: service.containers.len(),
                runtime_matches: None,
                restart_required: false,
                members: Vec::new(),
                lifecycle: self.simulated,
                logs: self.simulated,
            });
        }
        apps
    }

    fn compose_provider(&self, engine: &str) -> &ComposeProvider {
        if engine == "docker" { &self.docker_compose } else { &self.podman_compose }
    }

    pub async fn list_live_applications(&self, config: &Config) -> Vec<ApplicationSummary> {
        use futures_util::{StreamExt, stream};
        let mut apps = self.list_applications(config);
        if self.simulated {
            return apps;
        }
        let targets: Vec<_> = apps
            .iter()
            .enumerate()
            .filter(|(_, app)| matches!(app.backend.as_str(),"compose"|"libvirt"|"incus") || (app.backend == "quadlet" && self.quadlet.is_some()))
            .map(|(index, app)| (index, app.id.clone(), app.backend.clone(), app.engine.clone()))
            .collect();
        let mut inspections = stream::iter(targets)
            .map(|(index,id,backend,engine)| async move {
                let result = if backend == "compose" {
                    self.compose_provider(&engine).status_details(config,&id).await
                        .map(|status| (status.state,status.runtime_matches,status.restart_required,status.members,true))
                } else if backend == "incus" {
                    self.incus.status_details(config,&id).await
                        .map(|status| (status.state,status.runtime_matches,status.restart_required,Vec::new(),true))
                } else if backend == "libvirt" {
                    self.libvirt.status_details(config,&id).await
                        .map(|status| (status.state,status.runtime_matches,status.restart_required,Vec::new(),false))
                } else {
                    self.quadlet.as_ref().expect("filtered Quadlet provider").status_details(config,&id).await
                        .map(|status| (status.state,status.runtime_matches,status.restart_required,status.members,self.journalctl.is_file()))
                };
                (index,result)
            }).buffer_unordered(4);
        let mut verified = BTreeSet::new();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while let Some((index, result)) = inspections.next().await {
                if let Ok((state,runtime_matches,restart_required,members,logs_available)) = result {
                    let app = &mut apps[index];
                    if state != "unknown" { verified.insert(index); }
                    app.state = state.into();
                    app.runtime_matches = Some(runtime_matches);
                    app.restart_required = restart_required;
                    app.members = members;
                    app.logs = logs_available;
                }
            }
        })
        .await;
        let ready = lifecycle_ready(
            config,
            verified
                .iter()
                .map(|&index| apps[index].id.as_str())
                .collect(),
        );
        for app in &mut apps {
            app.lifecycle = ready.contains(app.id.as_str());
        }
        apps
    }

    pub fn list_shares(&self, config: &Config) -> Vec<ShareSummary> {
        config
            .shares
            .iter()
            .map(|(id, share)| ShareSummary {
                id: id.clone(),
                protocol: share.protocol.clone(),
                path: share.path.clone(),
                enabled: share.enabled,
                read_only: share.read_only,
                users: share.users.clone(),
                clients: share.clients.clone(),
            })
            .collect()
    }

    pub async fn action(&self, config: &Config, id: &str, action: &str) -> Result<String> {
        let service = config
            .services
            .get(id)
            .ok_or_else(|| anyhow!("service not found: {id}"))?;
        if !self.simulated && service.backend == "compose" {
            return self.compose_provider(&service.engine).native_action(config,id,action).await;
        }
        if !self.simulated && service.backend == "libvirt" {
            return self.libvirt.native_action(config,id,action).await;
        }
        if !self.simulated && service.backend == "incus" {
            return self.incus.native_action(config,id,action).await;
        }
        if !self.simulated && service.backend == "quadlet" {
            let provider = self
                .quadlet
                .as_ref()
                .ok_or_else(|| anyhow!("native Quadlet is unavailable"))?;
            return provider.action(config, id, action).await;
        }
        ensure!(
            self.simulated,
            "Native application lifecycle and logs require verified provider ownership."
        );
        match service.backend.as_str() {
            "compose" => {
                if service.engine == "docker" {
                    self.docker_compose.action(id, action).await
                } else {
                    self.podman_compose.action(id, action).await
                }
            }
            "incus" => self.incus.action(id, action).await,
            "libvirt" => self.libvirt.action(id, action).await,
            "quadlet" => {
                let unit = format!("maestro-{id}");
                Ok(format!("quadlet action {action} on {unit} submitted"))
            }
            other => Err(anyhow!("unsupported backend: {other}")),
        }
    }

    pub async fn managed_logs(
        &self,
        config: &Config,
        id: &str,
        tail: usize,
    ) -> Result<ManagedLogs> {
        ensure!((1..=2000).contains(&tail), "invalid log line limit");
        let service = config
            .services
            .get(id)
            .ok_or_else(|| anyhow!("service not found: {id}"))?;
        if !self.simulated && service.backend == "compose" {
            return self.compose_provider(&service.engine).native_logs(config,id,tail).await;
        }
        if !self.simulated && service.backend == "incus" {
            return self.incus.native_logs(config,id,tail).await;
        }
        if !self.simulated && service.backend == "quadlet" {
            let provider = self
                .quadlet
                .as_ref()
                .ok_or_else(|| anyhow!("native Quadlet is unavailable"))?;
            return provider
                .managed_logs(config, id, &self.journalctl, tail)
                .await;
        }
        ensure!(
            self.simulated,
            "Native logs require verified provider ownership."
        );
        let text = self.logs(config, id, tail).await?;
        let truncated = text.len() > 128 * 1024;
        let mut start = text.len().saturating_sub(128 * 1024);
        while !text.is_char_boundary(start) {
            start += 1;
        }
        Ok(ManagedLogs {
            text: text[start..].to_owned(),
            truncated,
            source: "simulation",
        })
    }

    pub async fn logs(&self, config: &Config, id: &str, tail: usize) -> Result<String> {
        let service = config
            .services
            .get(id)
            .ok_or_else(|| anyhow!("service not found: {id}"))?;
        if !self.simulated && service.backend == "compose" {
            return Ok(self.compose_provider(&service.engine).native_logs(config,id,tail).await?.display_text());
        }
        if !self.simulated && service.backend == "incus" {
            return Ok(self.incus.native_logs(config,id,tail).await?.display_text());
        }
        if !self.simulated && service.backend == "quadlet" {
            let provider = self
                .quadlet
                .as_ref()
                .ok_or_else(|| anyhow!("native Quadlet is unavailable"))?;
            return provider.logs(config, id, &self.journalctl, tail).await;
        }
        ensure!(
            self.simulated,
            "Native application lifecycle and logs require verified provider ownership."
        );
        match service.backend.as_str() {
            "compose" => {
                if service.engine == "docker" {
                    self.docker_compose.logs(id, tail).await
                } else {
                    self.podman_compose.logs(id, tail).await
                }
            }
            "incus" => self.incus.logs(id).await,
            "libvirt" => Ok(format!("Libvirt logs for {id}")),
            "quadlet" => Ok(format!("Quadlet journal for {id}")),
            other => Err(anyhow!("unsupported backend: {other}")),
        }
    }

    pub fn blocked_resources<'a>(&self, changes: &'a [Change]) -> Vec<&'a str> {
        changes
            .iter()
            .filter(|change| self.check_native_scope(change).is_err())
            .map(|change| change.resource.as_str())
            .collect()
    }

    /// Incus cannot defer live hardware changes. Match the reference's
    /// read-only transition guard at review, without touching other adapters.
    pub async fn review_incus_changes(&self, store: &super::Store, bytes: &[u8], plan: &super::Plan) -> Result<()> {
        if self.simulated || !plan.changes.iter().any(|change|change.backend=="incus") {
            return Ok(());
        }
        let current=store.read()?;
        let base=current.as_ref().map_or(super::MISSING_REVISION,|s|s.revision.as_str());
        ensure!(base==plan.base_revision,"configuration revision changed; review again");
        let previous=match current {
            Some(snapshot)=>snapshot.config()?,
            None=>Config{version:1,..Default::default()},
        };
        let desired=super::parse(bytes)?;
        for change in plan.changes.iter().filter(|change|change.backend=="incus") {
            self.incus.review_transition(change,&previous,&desired).await?;
        }
        Ok(())
    }

    fn check_native_scope(&self, change: &Change) -> Result<()> {
        ensure!(
            change.backend != "storage/disk",
            "Owned disk provisioning is unavailable."
        );
        ensure!(
            self.simulated
                || (change.backend == "storage" && change.resource.starts_with("storage/"))
                || (change.resource.starts_with("share/")
                    && matches!(change.backend.as_str(), "samba" | "nfs")
                    && self.shares.native_enabled())
                || (change.resource.starts_with("service/")
                    && matches!(change.backend.as_str(),"docker/compose"|"podman/compose"|"libvirt"|"incus"))
                || (change.resource.starts_with("service/")
                    && change.backend == "podman/quadlet"
                    && self.quadlet.is_some()),
            "Native provider ownership and recovery are not verified for this resource."
        );
        Ok(())
    }
}

impl ApplyProvider for CompositeProvider {
    async fn preflight_mode(
        &self,
        change: &Change,
        previous: &Config,
        desired: &Config,
        mode: ApplyMode,
    ) -> Result<()> {
        if mode == ApplyMode::Now {
            return self.preflight(change, previous, desired).await;
        }
        self.check_native_scope(change)?;
        if change.backend == "storage" {
            return self
                .storage
                .preflight_mode(change, previous, desired, mode)
                .await;
        }
        if change.backend == "libvirt" {
            return self.libvirt.preflight_mode(change,previous,desired,mode).await;
        }
        if change.backend == "incus" {
            return self.incus.preflight_mode(change,previous,desired,mode).await;
        }
        if matches!(change.backend.as_str(),"docker/compose"|"podman/compose") {
            return self.compose_provider(if change.backend == "docker/compose" {"docker"} else {"podman"})
                .preflight_mode(change,previous,desired,mode).await;
        }
        if change.backend == "podman/quadlet" {
            if let Some(provider) = &self.quadlet {
                return provider
                    .preflight_mode(change, previous, desired, mode)
                    .await;
            }
        }
        anyhow::bail!("next-start publication is unavailable for this provider")
    }
    async fn apply_mode(
        &self,
        change: &Change,
        previous: &Config,
        desired: &Config,
        mode: ApplyMode,
    ) -> Result<()> {
        if mode == ApplyMode::Now {
            return self.apply(change, previous, desired).await;
        }
        self.check_native_scope(change)?;
        if change.backend == "storage" {
            return self
                .storage
                .apply_mode(change, previous, desired, mode)
                .await;
        }
        if change.backend == "libvirt" {
            return self.libvirt.apply_mode(change,previous,desired,mode).await;
        }
        if change.backend == "incus" {
            return self.incus.apply_mode(change,previous,desired,mode).await;
        }
        if matches!(change.backend.as_str(),"docker/compose"|"podman/compose") {
            return self.compose_provider(if change.backend == "docker/compose" {"docker"} else {"podman"})
                .apply_mode(change,previous,desired,mode).await;
        }
        if change.backend == "podman/quadlet" {
            if let Some(provider) = &self.quadlet {
                return provider.apply_mode(change, previous, desired, mode).await;
            }
        }
        anyhow::bail!("next-start publication is unavailable for this provider")
    }

    async fn preflight(&self, change: &Change, previous: &Config, desired: &Config) -> Result<()> {
        self.check_native_scope(change)?;
        if change.resource.starts_with("storage/") {
            return self.storage.preflight(change, previous, desired).await;
        }
        if change.resource.starts_with("share/") {
            return self.shares.preflight(change, previous, desired).await;
        }
        if let Some(id) = change.resource.strip_prefix("service/") {
            let service = desired
                .services
                .get(id)
                .or_else(|| previous.services.get(id));
            if let Some(service) = service {
                match service.backend.as_str() {
                    "quadlet" => {
                        if let Some(q) = &self.quadlet {
                            return q.preflight(change, previous, desired).await;
                        }
                    }
                    "compose" => {
                        if service.engine == "docker" {
                            return self
                                .docker_compose
                                .preflight(change, previous, desired)
                                .await;
                        } else {
                            return self
                                .podman_compose
                                .preflight(change, previous, desired)
                                .await;
                        }
                    }
                    "incus" => return self.incus.preflight(change, previous, desired).await,
                    "libvirt" => return self.libvirt.preflight(change, previous, desired).await,
                    _ => {}
                }
            }
        }
        Ok(())
    }

    async fn apply(&self, change: &Change, previous: &Config, desired: &Config) -> Result<()> {
        self.check_native_scope(change)?;
        if change.resource.starts_with("storage/") {
            return self.storage.apply(change, previous, desired).await;
        }
        if change.resource.starts_with("share/") {
            return self.shares.apply(change, previous, desired).await;
        }
        if let Some(id) = change.resource.strip_prefix("service/") {
            let service = desired
                .services
                .get(id)
                .or_else(|| previous.services.get(id));
            if let Some(service) = service {
                match service.backend.as_str() {
                    "quadlet" => {
                        if let Some(q) = &self.quadlet {
                            return q.apply(change, previous, desired).await;
                        }
                    }
                    "compose" => {
                        if service.engine == "docker" {
                            return self.docker_compose.apply(change, previous, desired).await;
                        } else {
                            return self.podman_compose.apply(change, previous, desired).await;
                        }
                    }
                    "incus" => return self.incus.apply(change, previous, desired).await,
                    "libvirt" => return self.libvirt.apply(change, previous, desired).await,
                    _ => {}
                }
            }
        }
        Ok(())
    }
}

impl RecoveryProvider for CompositeProvider {
    async fn observe_mode(
        &self,
        change: &Change,
        previous: &Config,
        desired: &Config,
        mode: ApplyMode,
    ) -> Result<ObservedResource> {
        if mode == ApplyMode::Now {
            return self.observe(change, previous, desired).await;
        }
        self.check_native_scope(change)?;
        if change.backend == "storage" {
            return self
                .storage
                .observe_mode(change, previous, desired, mode)
                .await;
        }
        if change.backend == "libvirt" {
            return self.libvirt.observe_mode(change,previous,desired,mode).await;
        }
        if change.backend == "incus" {
            return self.incus.observe_mode(change,previous,desired,mode).await;
        }
        if matches!(change.backend.as_str(),"docker/compose"|"podman/compose") {
            return self.compose_provider(if change.backend == "docker/compose" {"docker"} else {"podman"})
                .observe_mode(change,previous,desired,mode).await;
        }
        if change.backend == "podman/quadlet" {
            if let Some(provider) = &self.quadlet {
                return provider.observe_mode(change, previous, desired, mode).await;
            }
        }
        anyhow::bail!("next-start inspection is unavailable for this provider")
    }

    async fn observe(
        &self,
        change: &Change,
        previous: &Config,
        desired: &Config,
    ) -> Result<ObservedResource> {
        if self.check_native_scope(change).is_err() {
            return Ok(ObservedResource::Unknown);
        }
        if change.resource.starts_with("storage/") {
            return self.storage.observe(change, previous, desired).await;
        }
        if change.resource.starts_with("share/") {
            return self.shares.observe(change, previous, desired).await;
        }
        if let Some(id) = change.resource.strip_prefix("service/") {
            let service = desired
                .services
                .get(id)
                .or_else(|| previous.services.get(id));
            if let Some(service) = service {
                match service.backend.as_str() {
                    "quadlet" => {
                        if let Some(q) = &self.quadlet {
                            return q.observe(change, previous, desired).await;
                        }
                    }
                    "compose" => {
                        if service.engine == "docker" {
                            return self.docker_compose.observe(change, previous, desired).await;
                        } else {
                            return self.podman_compose.observe(change, previous, desired).await;
                        }
                    }
                    "incus" => return self.incus.observe(change, previous, desired).await,
                    "libvirt" => return self.libvirt.observe(change, previous, desired).await,
                    _ => {}
                }
            }
        }
        Ok(ObservedResource::Unknown)
    }
}

fn lifecycle_ready<'a>(config: &'a Config, verified: BTreeSet<&str>) -> BTreeSet<&'a str> {
    let mut ready = BTreeSet::new();
    for id in config.service_order().unwrap_or_default() {
        if verified.contains(id)
            && config.services[id]
                .depends_on
                .iter()
                .all(|dependency| ready.contains(dependency.as_str()))
        {
            ready.insert(id);
        }
    }
    ready
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::CommandRunner;
    use crate::config::AppConfig;
    use tempfile::tempdir;

    #[test]
    fn test_list_applications_and_shares() {
        let temp = tempdir().unwrap();
        let mut config = AppConfig::default();
        config.paths.data_dir = temp.path().join("data");
        config.paths.storage_dir = temp.path().join("storage");
        config.paths.applications_dir = temp.path().join("apps");
        config.paths.samba_conf = temp.path().join("smb.conf");
        config.paths.exports_file = temp.path().join("exports");
        std::fs::create_dir_all(&config.paths.data_dir).unwrap();

        let runner = CommandRunner::new(true);
        let provider = CompositeProvider::new(&config, runner);

        let yaml = r#"
version: 1
storage:
  data:
    path: volumes/data
    kind: directory
services:
  web:
    name: "Web Server"
    description: "Nginx site"
    kind: container
    engine: docker
    backend: compose
    enabled: true
    containers:
      web: { image: "nginx:alpine" }
  db:
    name: "Database"
    kind: container
    engine: podman
    backend: quadlet
    enabled: false
    containers:
      db: { image: "postgres:16" }
shares:
  data:
    protocol: samba
    storage: data
    enabled: true
    read_only: false
    users: ["alice"]
    clients: []
"#;
        let parsed: Config = crate::core::declarative::parse(yaml.as_bytes()).unwrap();
        let apps = provider.list_applications(&parsed);
        assert_eq!(apps.len(), 2);
        let web_app = apps.iter().find(|a| a.id == "web").unwrap();
        assert_eq!(web_app.name, "Web Server");
        assert_eq!(web_app.state, "running");
        assert_eq!(web_app.container_count, 1);

        let db_app = apps.iter().find(|a| a.id == "db").unwrap();
        assert_eq!(db_app.name, "Database");
        assert_eq!(db_app.state, "stopped");

        let shares = provider.list_shares(&parsed);
        assert_eq!(shares.len(), 1);
        assert_eq!(shares[0].id, "data");
        assert_eq!(shares[0].protocol, "samba");
        assert_eq!(shares[0].users, vec!["alice"]);
    }

    #[tokio::test]
    async fn native_dispatch_refuses_unverified_provider_actions() {
        let provider = CompositeProvider::new(&AppConfig::default(), CommandRunner::new(false));
        let desired = crate::core::declarative::parse(
            b"version: 1\nservices:\n  demo: {kind: container, engine: docker, containers: {web: {image: alpine}}}\n",
        ).unwrap();
        let change = Change {
            resource: "service/demo".into(),
            backend: "docker/compose".into(),
            operation: crate::core::declarative::Operation::Create,
            before: None,
            after: Some("a".repeat(64)),
        };
        let previous = Config::default();
        // Compose is now routed to native checks; an unrelated/missing
        // engine still fails closed at preflight and lifecycle verification.
        assert!(provider.blocked_resources(std::slice::from_ref(&change)).is_empty());
        assert!(
            provider
                .preflight(&change, &previous, &desired)
                .await
                .is_err()
        );
        assert!(provider.apply(&change, &previous, &desired).await.is_err());
        assert_eq!(
            provider
                .observe(&change, &previous, &desired)
                .await
                .unwrap_or(ObservedResource::Unknown),
            ObservedResource::Unknown
        );
        assert!(provider.action(&desired,"demo","stop").await.is_err());
        assert_eq!(provider.list_applications(&desired)[0].state, "unknown");
    }

    #[test]
    fn lifecycle_requires_all_transitive_dependencies_to_be_verified() {
        let config = crate::core::declarative::parse(
            br#"version: 1
services:
  db: {kind: container, engine: podman, backend: quadlet, containers: {db: {image: alpine}}}
  api: {kind: container, engine: podman, backend: quadlet, depends_on: [db], containers: {api: {image: alpine}}}
  web: {kind: container, engine: podman, backend: quadlet, depends_on: [api], containers: {web: {image: alpine}}}
"#,
        ).unwrap();

        assert!(lifecycle_ready(&config, BTreeSet::from(["api", "web"])).is_empty());
        assert_eq!(
            lifecycle_ready(&config, BTreeSet::from(["db", "api", "web"])),
            BTreeSet::from(["db", "api", "web"])
        );
    }

    #[tokio::test]
    async fn native_inventory_hides_controls_for_missing_quadlet_artifacts() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = AppConfig::default();
        config.paths.quadlet_dir = Some(temp.path().to_path_buf());
        config.paths.storage_dir = temp.path().join("storage");
        config.commands.systemctl = "/bin/false".into();
        config.commands.journalctl = "/bin/false".into();
        let provider = CompositeProvider::new(&config, CommandRunner::new(false));
        let desired = crate::core::declarative::parse(
            b"version: 1\nservices:\n  demo: {kind: container, engine: podman, backend: quadlet, containers: {web: {image: alpine}}}\n",
        ).unwrap();

        let apps = provider.list_live_applications(&desired).await;
        assert_eq!(apps[0].state, "unknown");
        assert!(!apps[0].lifecycle);
        assert!(!apps[0].logs);
    }

    #[tokio::test]
    async fn owned_disk_is_reviewed_and_cannot_be_silently_published() {
        use crate::core::declarative::{MISSING_REVISION, Store};
        use std::os::unix::fs::PermissionsExt;

        let temp = tempdir().unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let store = Store::new(temp.path().to_path_buf());
        let provider = CompositeProvider::new(&AppConfig::default(), CommandRunner::new(false));
        let yaml = format!(
            "version: 1\nstorage:\n  vm-disk:\n    path: disks/vm.qcow2\n    kind: file\n    format: qcow2\n    source: {{path: assets/base.qcow2, format: qcow2, sha256: '{}'}}\n",
            "a".repeat(64),
        );
        let plan = store.preview(yaml.as_bytes(), MISSING_REVISION).unwrap();
        assert_eq!(plan.changes.len(), 1);
        assert_eq!(plan.changes[0].backend, "storage/disk");
        assert_eq!(
            provider.blocked_resources(&plan.changes),
            ["storage/vm-disk"]
        );
        assert!(
            store
                .apply_review(
                    yaml.as_bytes(),
                    MISSING_REVISION,
                    &plan.fingerprint,
                    &provider
                )
                .await
                .is_err()
        );
        assert!(store.read().unwrap().is_none());
    }

    #[tokio::test]
    async fn reviewed_managed_directory_is_provisioned_without_host_tools() {
        use crate::core::declarative::{MISSING_REVISION, Store};
        use std::os::unix::fs::PermissionsExt;

        let temp = tempdir().unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let config = AppConfig {
            paths: crate::config::PathsConfig {
                storage_dir: temp.path().join("storage"),
                ..Default::default()
            },
            ..Default::default()
        };
        let provider = CompositeProvider::new(&config, CommandRunner::new(false));
        let store = Store::new(temp.path().to_path_buf());
        let yaml = b"version: 1\nstorage:\n  cache: {path: volumes/cache, kind: directory}\n";
        let plan = store.preview(yaml, MISSING_REVISION).unwrap();
        assert_eq!(plan.changes[0].resource, "storage/cache");
        assert!(!config.paths.storage_dir.exists());
        let saved = store
            .apply_review(yaml, MISSING_REVISION, &plan.fingerprint, &provider)
            .await
            .unwrap();
        let directory = config.paths.storage_dir.join("volumes/cache");
        assert!(directory.join(".maestro-storage.json").is_file());
        std::fs::write(directory.join("payload"), b"keep").unwrap();

        let empty = b"version: 1\n";
        let removal = store.preview(empty, &saved.revision).unwrap();
        store
            .apply_review(empty, &saved.revision, &removal.fingerprint, &provider)
            .await
            .unwrap();
        assert!(directory.join("payload").is_file());
        assert!(!directory.join(".maestro-storage.json").exists());
    }
}
