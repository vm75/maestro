mod apply;
pub mod backup_inventory;
pub mod browse;
mod container_health;
mod revert;
pub use apply::{ApplyMode, ApplyProvider, ObservedResource, RecoveryProvider};
pub mod compose;
pub mod composite;
pub mod credentials;
pub mod edit;
pub mod incus;
mod journal;
pub use journal::{ApplyJournal, ApplyPhase};
pub mod libvirt;
mod model;
mod plan;
pub mod quadlet;
pub use composite::{ApplicationSummary, CompositeProvider, ShareSummary};
pub use plan::{Change, Operation, Plan};
pub use quadlet::{QuadletArtifacts, QuadletProvider, managed_unit, render_quadlet};
pub mod server;
pub mod shares;
pub mod storage;
mod store;
pub use store::{MISSING_REVISION, RevisionConflict, Snapshot, Store};
mod yaml;

use std::{collections::BTreeSet, path::Path};

use anyhow::{Result, anyhow, bail, ensure};
use serde_yaml::Value;

pub use model::*;

pub const MAX_CONFIG_BYTES: usize = 1024 * 1024;

/// A revision identifies the exact authored bytes, including comments and line endings.
pub fn revision(input: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(input))
}

pub fn parse(input: &[u8]) -> Result<Config> {
    ensure!(
        input.len() <= MAX_CONFIG_BYTES,
        "configuration exceeds 1 MiB"
    );
    yaml::check(input)?;
    let shape: Value = serde_yaml::from_slice(input)
        .map_err(|_| anyhow!("invalid or duplicate YAML mapping key"))?;
    ensure!(shape.is_mapping(), "configuration must be a YAML mapping");
    ensure!(string_keys(&shape), "YAML mapping keys must be strings");
    server::read(
        input,
        &server::Settings::defaults(Path::new("/"), Path::new("/")),
    )?;
    let mut config: Config = serde_yaml::from_slice(input)
        .map_err(|_| anyhow!("unknown configuration field or incorrect field type"))?;
    config.validate()?;
    Ok(config)
}

fn string_keys(value: &Value) -> bool {
    match value {
        Value::Mapping(map) => map
            .iter()
            .all(|(key, value)| key.is_string() && string_keys(value)),
        Value::Sequence(items) => items.iter().all(string_keys),
        _ => true,
    }
}

fn id_valid(id: &str) -> bool {
    id.len() <= 63
        && id.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_-".contains(&byte))
}

fn path_valid(path: &str, absolute: bool) -> bool {
    if path.is_empty()
        || path.contains(['\0', '\r', '\n'])
        || Path::new(path).is_absolute() != absolute
    {
        return false;
    }
    if matches!(path, "." | "/") {
        return true;
    }
    path.trim_start_matches('/')
        .split('/')
        .all(|part| !matches!(part, "" | "." | ".."))
        && !path.starts_with("//")
}

fn valid_mac(value: &str) -> bool {
    if value.len() != 17 {
        return false;
    }
    let mut bytes = value.split(':');
    let Some(first) = bytes
        .next()
        .and_then(|byte| u8::from_str_radix(byte, 16).ok())
    else {
        return false;
    };
    first & 1 == 0
        && value.split(':').count() == 6
        && value.split(':').all(|byte| {
            byte.len() == 2
                && byte
                    .bytes()
                    .all(|digit| digit.is_ascii_digit() || (b'a'..=b'f').contains(&digit))
        })
}

impl Config {
    fn validate(&mut self) -> Result<()> {
        ensure!(self.version == 1, "unsupported configuration version");
        ensure!(
            self.services.len() <= 128
                && self.storage.len() <= 256
                && self.shares.len() <= 64
                && self.bindings.len() <= 128
                && self.secrets.len() <= 128,
            "configuration resource limit exceeded"
        );
        let mut paths = BTreeSet::new();
        for (id, storage) in &self.storage {
            ensure!(
                id_valid(id) && matches!(storage.kind.as_str(), "" | "directory" | "file"),
                "invalid storage definition"
            );
            ensure!(
                path_valid(&storage.path, storage.external) && paths.insert(&storage.path),
                "invalid or duplicate storage path"
            );
            ensure!(
                storage.external || !storage.path.split('/').any(|part| part == storage::MARKER),
                "managed storage path contains the reserved ownership marker name"
            );
            ensure!(
                storage.format.is_empty()
                    || (storage.kind == "file"
                        && matches!(storage.format.as_str(), "raw" | "qcow2")),
                "invalid storage format"
            );
            ensure!(
                !storage.path.chars().any(char::is_control)
                    && !storage.mount.chars().any(char::is_control),
                "invalid storage path characters"
            );
            if let Some(source) = &storage.source {
                ensure!(
                    !storage.external
                        && storage.kind == "file"
                        && matches!(storage.format.as_str(), "raw" | "qcow2")
                        && matches!(source.format.as_str(), "raw" | "qcow2")
                        && path_valid(&source.path, false)
                        && source.path.starts_with("assets/")
                        && source.sha256.len() == 64
                        && source.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()),
                    "invalid owned disk source"
                );
                ensure!(
                    !self.storage.values().any(|other| {
                        other.path == source.path
                            || other.kind != "file"
                                && (other.path == "."
                                    || source
                                        .path
                                        .strip_prefix(&other.path)
                                        .is_some_and(|rest| rest.starts_with('/')))
                    }) && !self.secrets.values().any(|path| path == &source.path),
                    "disk source overlaps managed storage or a private file"
                );
            }
        }
        for (id, path) in &self.secrets {
            ensure!(
                id_valid(id) && path_valid(path, false),
                "invalid private file reference"
            );
        }
        for (id, binding) in &self.bindings {
            ensure!(
                id_valid(id)
                    && !binding.name.is_empty()
                    && !binding.name.contains(['\0', '\r', '\n'])
                    && matches!(
                        binding.kind.as_str(),
                        "network" | "bridge" | "pool" | "project"
                    ),
                "invalid host binding"
            );
        }
        let mut container_names = BTreeSet::new();
        let mut vm_uuids = BTreeSet::new();
        let mut vm_macs = BTreeSet::new();
        for (id, service) in &mut self.services {
            ensure!(id_valid(id), "invalid service identity");
            ensure!(
                service.name.len() <= 128
                    && service.description.len() <= 4096
                    && !service.name.chars().any(char::is_control),
                "invalid service display fields"
            );
            ensure!(
                service.depends_on.iter().collect::<BTreeSet<_>>().len()
                    == service.depends_on.len(),
                "duplicate dependency"
            );
            match service.kind.as_str() {
                "container" => {
                    ensure!(
                        service.vm.is_none() && (1..=32).contains(&service.containers.len()),
                        "invalid container application"
                    );
                    if service.engine.is_empty() {
                        service.engine = "podman".into();
                    }
                    if service.backend.is_empty() {
                        service.backend = if service.engine == "docker" {
                            "compose"
                        } else {
                            "quadlet"
                        }
                        .into();
                    }
                    ensure!(
                        matches!(
                            (service.engine.as_str(), service.backend.as_str()),
                            ("podman", "compose" | "quadlet") | ("docker", "compose")
                        ),
                        "unsupported container backend"
                    );
                    for (name, container) in &service.containers {
                        ensure!(
                            id_valid(name)
                                && !container.image.is_empty()
                                && container.memory_mib >= 0
                                && container.cpus >= 0,
                            "invalid container definition"
                        );
                        ensure!(
                            container.image.len() <= 512
                                && !container.image.starts_with('-')
                                && !container
                                    .image
                                    .chars()
                                    .any(|character| character.is_whitespace()
                                        || character.is_control()),
                            "invalid container image"
                        );
                        ensure!(
                            container.container_name.is_empty()
                                || container_names.insert(container.container_name.as_str()),
                            "duplicate native container name"
                        );
                        ensure!(
                            (0..=1024).contains(&container.cpus)
                                && (0..=1 << 30).contains(&container.memory_mib)
                                && container.command.len() <= 128
                                && container.environment.len() <= 128
                                && container.ports.len() <= 64
                                && container.mounts.len() <= 64
                                && container.env_files.len() <= 32,
                            "container resource or field limit exceeded"
                        );
                        ensure!(
                            container
                                .mounts
                                .iter()
                                .map(|mount| &mount.target)
                                .collect::<BTreeSet<_>>()
                                .len()
                                == container.mounts.len(),
                            "duplicate container mount target"
                        );
                        ensure!(
                            container.mounts.iter().all(|mount| {
                                self.storage.contains_key(&mount.storage)
                                    && path_valid(&mount.target, true)
                                    && (mount.path.is_empty() || path_valid(&mount.path, false))
                            }),
                            "invalid container mount reference or path"
                        );
                        ensure!(
                            container
                                .env_files
                                .iter()
                                .all(|file| self.secrets.contains_key(file)),
                            "missing environment file reference"
                        );
                    }
                    container_health::validate(&service.containers)?;
                    dependency_order(service.containers.keys().map(String::as_str), |name| {
                        service.containers.get(name).map(|container| {
                            container.depends_on.keys().map(String::as_str).collect()
                        })
                    })?;
                }
                "vm" => {
                    let vm = service
                        .vm
                        .as_ref()
                        .ok_or_else(|| anyhow!("missing VM definition"))?;
                    ensure!(
                        service.engine.is_empty()
                            && service.containers.is_empty()
                            && matches!(service.backend.as_str(), "libvirt" | "incus")
                            && vm.cpus >= 1
                            && vm.memory_mib >= 128,
                        "invalid VM resources or backend"
                    );
                    ensure!(
                        self.bindings
                            .get(&vm.network)
                            .is_some_and(|binding| matches!(
                                binding.kind.as_str(),
                                "network" | "bridge"
                            ))
                            && vm.disks.iter().all(|disk| self
                                .storage
                                .get(disk)
                                .is_some_and(|storage| storage.kind == "file")),
                        "invalid VM network or disk reference"
                    );
                    ensure!(
                        (1..=1024).contains(&vm.cpus)
                            && (128..=1 << 30).contains(&vm.memory_mib)
                            && (0..=1 << 20).contains(&vm.root_size_gib)
                            && (!vm.image.is_empty() || !vm.disks.is_empty()),
                        "invalid VM resources or image"
                    );
                    for (reference, kind) in [(&vm.pool, "pool"), (&vm.project, "project")] {
                        ensure!(
                            reference.is_empty()
                                || self
                                    .bindings
                                    .get(reference)
                                    .is_some_and(|binding| binding.kind == kind),
                            "invalid VM pool or project binding"
                        );
                    }
                    if !vm.uuid.is_empty() {
                        let uuid = uuid::Uuid::parse_str(&vm.uuid)
                            .map_err(|_| anyhow!("invalid VM UUID"))?;
                        ensure!(
                            uuid.hyphenated().to_string() == vm.uuid && vm_uuids.insert(uuid),
                            "invalid or duplicate VM UUID"
                        );
                    }
                    if !vm.mac.is_empty() {
                        ensure!(
                            valid_mac(&vm.mac) && vm_macs.insert(vm.mac.as_str()),
                            "invalid or duplicate VM MAC"
                        );
                    }
                }
                _ => bail!("unsupported service kind"),
            }
        }
        self.service_order()?;
        for service in self.services.values().filter(|service| service.enabled) {
            ensure!(
                service
                    .depends_on
                    .iter()
                    .all(|dependency| self.services[dependency].enabled),
                "enabled service depends on disabled service"
            );
        }
        for (id, share) in &self.shares {
            ensure!(
                id_valid(id)
                    && matches!(share.protocol.as_str(), "samba" | "nfs")
                    && self
                        .storage
                        .get(&share.storage)
                        .is_some_and(|storage| storage.kind != "file")
                    && (share.path.is_empty() || path_valid(&share.path, false))
                    && (share.protocol != "nfs" || !share.clients.is_empty()),
                "invalid share or storage reference"
            );
        }
        Ok(())
    }

    pub fn service_order(&self) -> Result<Vec<&str>> {
        dependency_order(self.services.keys().map(String::as_str), |id| {
            self.services
                .get(id)
                .map(|service| service.depends_on.iter().map(String::as_str).collect())
        })
    }
}

fn dependency_order<'a>(
    names: impl Iterator<Item = &'a str>,
    dependencies: impl Fn(&str) -> Option<Vec<&'a str>>,
) -> Result<Vec<&'a str>> {
    fn visit<'a>(
        name: &'a str,
        dependencies: &impl Fn(&str) -> Option<Vec<&'a str>>,
        visiting: &mut BTreeSet<&'a str>,
        done: &mut BTreeSet<&'a str>,
        result: &mut Vec<&'a str>,
    ) -> Result<()> {
        if done.contains(name) {
            return Ok(());
        }
        ensure!(visiting.insert(name), "dependency cycle");
        for dependency in dependencies(name).ok_or_else(|| anyhow!("missing dependency"))? {
            visit(dependency, dependencies, visiting, done, result)?;
        }
        visiting.remove(name);
        done.insert(name);
        result.push(name);
        Ok(())
    }
    let mut result = Vec::new();
    let mut visiting = BTreeSet::new();
    let mut done = BTreeSet::new();
    for name in names {
        visit(name, &dependencies, &mut visiting, &mut done, &mut result)?;
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKLOAD: &str = "
version: 1
storage:
  cache: {path: volumes/cache, kind: directory}
secrets:
  credentials: private/credentials.env
services:
  cache:
    kind: container
    enabled: true
    containers:
      default: {image: docker.io/library/redis:7}
  web:
    kind: container
    engine: docker
    enabled: true
    depends_on: [cache]
    containers:
      app:
        image: docker.io/library/nginx:alpine
        env_files: [credentials]
        mounts: [{storage: cache, target: /var/cache}]
shares:
  cache: {protocol: samba, storage: cache, enabled: false}
";

    #[test]
    fn typed_workload_defaults_and_dependency_order() {
        let config = parse(WORKLOAD.as_bytes()).unwrap();
        assert_eq!(config.services["cache"].backend, "quadlet");
        assert_eq!(config.services["web"].backend, "compose");
        assert_eq!(config.service_order().unwrap(), ["cache", "web"]);
        assert_eq!(config.shares["cache"].storage, "cache");
    }

    #[test]
    fn rejects_storage_paths_that_collide_with_ownership_markers() {
        let yaml = b"version: 1\nstorage:\n  parent: {kind: directory, path: volumes}\n  child: {kind: directory, path: volumes/.maestro-storage.json/nested}\n";
        let error = parse(yaml).err().unwrap();
        assert!(error.to_string().contains("reserved ownership marker"));
    }

    #[test]
    fn rejects_structural_errors_without_echoing_values() {
        for yaml in [
            "version: 1\nversion: 1",
            "version: !!int 1",
            "version: !unsafe sensitive",
            "version: &one 1",
            "version: *one",
            "version: 1\nunknown: sensitive",
            "version: 1\nserver: {token: sensitive}",
            "version: 1\nsecrets: {42: sensitive}",
            "version: 1\n---\nversion: 1",
            "- version: 1",
        ] {
            let error = parse(yaml.as_bytes()).err().expect(yaml);
            assert!(!error.to_string().contains("sensitive"));
        }
        let deep = format!(
            "version: 1\nservices: {}x{}",
            "[".repeat(130),
            "]".repeat(130)
        );
        assert!(parse(deep.as_bytes()).is_err());
    }

    #[test]
    fn detects_missing_references_and_service_cycles() {
        let cycle = WORKLOAD.replace(
            "    kind: container\n    enabled: true",
            "    kind: container\n    depends_on: [web]\n    enabled: true",
        );
        assert_eq!(
            parse(cycle.as_bytes()).err().unwrap().to_string(),
            "dependency cycle"
        );
        let missing = WORKLOAD.replace("env_files: [credentials]", "env_files: [missing]");
        assert!(parse(missing.as_bytes()).is_err());
        let escaping = WORKLOAD.replace("volumes/cache", "../cache");
        assert!(parse(escaping.as_bytes()).is_err());
    }

    #[test]
    fn rejects_duplicate_guest_identity_and_wrong_binding_kind() {
        let yaml = r#"
version: 1
bindings:
  lan: {kind: bridge, name: br0}
services:
  first:
    kind: vm
    backend: incus
    vm: {image: images:debian/12, cpus: 2, memory_mib: 512, network: lan, mac: "02:01:02:03:04:05"}
  second:
    kind: vm
    backend: incus
    vm: {image: images:debian/12, cpus: 2, memory_mib: 512, network: lan, mac: "02:01:02:03:04:05"}
"#;
        assert_eq!(
            parse(yaml.as_bytes()).err().unwrap().to_string(),
            "invalid or duplicate VM MAC"
        );
        assert!(
            parse(
                yaml.replacen("02:01:02:03:04:05", "02:01:02:03:04:06", 1)
                    .as_bytes()
            )
            .is_ok()
        );
        assert!(parse(yaml.replace("kind: bridge", "kind: pool").as_bytes()).is_err());
    }

    #[test]
    fn revisions_cover_original_bytes() {
        let before = b"version: 1\n";
        let after = b"# comment\nversion: 1\n";
        assert_eq!(revision(before).len(), 64);
        assert_ne!(revision(before), revision(after));
        assert_eq!(
            parse(before).unwrap().version,
            parse(after).unwrap().version
        );
    }
}
