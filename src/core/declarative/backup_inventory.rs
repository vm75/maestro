//! Read-only, bounded inventory for partial (NON-RESTORABLE) Linux capture.
//! This is not a backup/capture/verification implementation. It never opens
//! private file contents, follows links or changes native workloads.
use std::{
    collections::BTreeSet,
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Utc};
use serde::Serialize;

use super::{Config, Store};

const MAX_ENTRIES: usize = 2048;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupInput {
    pub id: String,
    pub kind: &'static str,
    pub strategy: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bundle_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub included_by: Option<String>,
    pub resources: Vec<String>,
    pub required: bool,
    pub sensitive: bool,
    pub inspection: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logical_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allocated_bytes: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Requirement {
    pub resource: String,
    pub kind: &'static str,
    pub description: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Exclusion {
    pub kind: &'static str,
    pub reason: &'static str,
}

#[derive(Clone, Debug, Serialize)]
pub struct Issue {
    pub severity: &'static str,
    pub resource: String,
    pub field: &'static str,
    pub code: &'static str,
    pub message: &'static str,
    pub remedy: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupInventory {
    pub stage: &'static str,
    pub revision: String,
    pub checked_at: DateTime<Utc>,
    pub restorable: bool,
    pub inputs: Vec<BackupInput>,
    pub requirements: Vec<Requirement>,
    pub exclusions: Vec<Exclusion>,
    pub issues: Vec<Issue>,
}

fn issue(
    resource: &str,
    field: &'static str,
    code: &'static str,
    message: &'static str,
    remedy: &'static str,
) -> Issue {
    Issue {
        severity: "error",
        resource: resource.to_owned(),
        field,
        code,
        message,
        remedy,
    }
}

impl BackupInventory {
    fn input(&mut self, mut input: BackupInput) -> Result<()> {
        ensure!(
            self.inputs.len() + self.requirements.len() < MAX_ENTRIES,
            "backup input inventory exceeds its bounded entry limit"
        );
        input.resources.sort_unstable();
        input.resources.dedup();
        self.inputs.push(input);
        Ok(())
    }

    fn requirement(
        &mut self,
        resource: &str,
        kind: &'static str,
        description: impl Into<String>,
    ) -> Result<()> {
        ensure!(
            self.inputs.len() + self.requirements.len() < MAX_ENTRIES,
            "backup requirements exceed their bounded entry limit"
        );
        self.requirements.push(Requirement {
            resource: resource.to_owned(),
            kind,
            description: description.into(),
        });
        Ok(())
    }

    /// Metadata only; never call canonicalize/open/read on the referenced file.
    /// Symlinked intermediate components are not traversed, even when their
    /// ultimate target lies within the intended root.
    fn inspect_path(&mut self, input: &mut BackupInput, path: &Path, directory: bool) {
        if !path.is_absolute() {
            input.inspection = "unavailable";
            self.issues.push(issue(
                &input.id,
                "path",
                "backup_path_invalid",
                "An absolute, validated path is required.",
                "Review the resource path before capture.",
            ));
            return;
        }
        let mut current = PathBuf::new();
        for component in path.components() {
            current.push(component.as_os_str());
            match fs::symlink_metadata(&current) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    input.inspection = "unverified_symlink";
                    self.issues.push(issue(
                        &input.id,
                        "path",
                        "backup_symlink",
                        "A backup input path contains a symlink.",
                        "Use real directories and review the intended file separately.",
                    ));
                    return;
                }
                Ok(_) => {}
                Err(_) => {
                    input.inspection = "unavailable";
                    self.issues.push(issue(
                        &input.id,
                        "path",
                        "backup_path_unavailable",
                        "Backup input cannot be inspected with ordinary account permissions.",
                        "Restore the expected path and appropriate read access.",
                    ));
                    return;
                }
            }
        }
        match fs::symlink_metadata(path) {
            Ok(meta)
                if if directory {
                    meta.is_dir()
                } else {
                    meta.is_file()
                } =>
            {
                let mode = meta.permissions().mode();
                if input.kind == "private_file"
                    && (meta.uid() != unsafe { libc::geteuid() } || mode & 0o077 != 0)
                {
                    input.inspection = "unsafe_permissions";
                    self.issues.push(issue(
                        &input.id,
                        "path",
                        "backup_private_permissions",
                        "A sensitive input has unexpected ownership or permissions.",
                        "Restrict the private input to the workload account.",
                    ));
                } else {
                    input.inspection = "metadata_only";
                    if meta.is_file() {
                        input.logical_bytes = Some(meta.len());
                        input.allocated_bytes = Some(meta.blocks().saturating_mul(512));
                    }
                }
            }
            _ => {
                input.inspection = "wrong_type";
                self.issues.push(issue(
                    &input.id,
                    "path",
                    "backup_path_type",
                    "Backup input is not the expected file or directory.",
                    "Restore a correctly typed path before attempting capture.",
                ));
            }
        }
    }
}

// Source-guided translation of Server Man references.go and backup_declared.go.
fn consumers(config: &Config, kind: &str, id: &str) -> Vec<String> {
    let mut resources = BTreeSet::from([format!("{kind}/{id}")]);
    for (service_id, service) in &config.services {
        let referenced = match kind {
            "storage" => {
                service
                    .containers
                    .values()
                    .any(|container| container.mounts.iter().any(|mount| mount.storage == id))
                    || service
                        .vm
                        .as_ref()
                        .is_some_and(|vm| vm.disks.iter().any(|disk| disk == id))
            }
            "secret" => service
                .containers
                .values()
                .any(|container| container.env_files.iter().any(|secret| secret == id)),
            _ => false,
        };
        if referenced {
            resources.insert(format!("service/{service_id}"));
        }
    }
    if kind == "storage" {
        for (share_id, share) in &config.shares {
            if share.storage == id {
                resources.insert(format!("share/{share_id}"));
            }
        }
    }
    resources.into_iter().collect()
}

/// Derive a capture-relative path and flag escapes/overlap with the authoritative
/// configuration or private runtime state. No capture is authorized by this.
fn annotate_coverage(inventory: &mut BackupInventory, store: &Store, root: &Path) {
    let config_path = store.path();
    let config_name = config_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    for input in &mut inventory.inputs {
        if input.strategy != "bundle_copy" {
            continue;
        }
        let Some(path) = input.path.as_deref().map(Path::new) else {
            continue;
        };
        let Ok(relative) = path.strip_prefix(root) else {
            inventory.issues.push(issue(
                &input.id,
                "path",
                "backup_path_escape",
                "A required backup input is outside the authoritative configuration root.",
                "Review and explicitly rebind this storage before capture.",
            ));
            input.inspection = "outside_configuration_root";
            continue;
        };
        let first = relative
            .components()
            .next()
            .and_then(|part| part.as_os_str().to_str())
            .unwrap_or("");
        if input.id != "configuration"
            && (first.is_empty()
                || first == config_name
                || first == store.history_name
                || first.starts_with(&format!("{config_name}."))
                || matches!(
                    first,
                    "apply-candidate.yaml" | "apply-journal.json" | "applications" | "run" | "logs"
                ))
        {
            inventory.issues.push(issue(
                &input.id, "path", "backup_derived_state_collision",
                "A backup input overlaps the authoritative configuration or derived private state.",
                "Rebind data away from configuration, history, generated definitions and runtime journals.",
            ));
            input.inspection = "state_collision";
            continue;
        }
        input.bundle_path = Some(relative.to_string_lossy().into_owned());
    }
    let mut indices: Vec<_> = inventory
        .inputs
        .iter()
        .enumerate()
        .filter(|(_, input)| {
            input.strategy == "bundle_copy"
                && input.inspection == "metadata_only"
                && input.bundle_path.is_some()
        })
        .map(|(index, _)| index)
        .collect();
    indices.sort_by(|&left, &right| {
        let a = &inventory.inputs[left];
        let b = &inventory.inputs[right];
        a.path
            .as_ref()
            .map(String::len)
            .cmp(&b.path.as_ref().map(String::len))
            .then_with(|| a.id.cmp(&b.id))
    });
    for (position, &index) in indices.iter().enumerate() {
        let covered = indices[..position].iter().find_map(|&prior| {
            let input = &inventory.inputs[index];
            let parent = &inventory.inputs[prior];
            if parent.included_by.is_some() {
                return None;
            }
            let path = Path::new(input.path.as_deref()?);
            let base = Path::new(parent.path.as_deref()?);
            (path == base || (parent.kind == "directory" && path.starts_with(base)))
                .then(|| parent.id.clone())
        });
        inventory.inputs[index].included_by = covered;
    }
}

fn inventory_inputs(
    inventory: &mut BackupInventory,
    config: &Config,
    private_root: &Path,
    storage_root: &Path,
) -> Result<()> {
    for (id, storage) in &config.storage {
        let resource = format!("storage/{id}");
        let path = if storage.external {
            PathBuf::from(&storage.path)
        } else {
            storage_root.join(&storage.path)
        };
        let kind = if storage.kind == "file" {
            "file"
        } else {
            "directory"
        };
        let mut input = BackupInput {
            id: resource.clone(),
            kind,
            strategy: if storage.external {
                "external_data"
            } else {
                "bundle_copy"
            },
            path: Some(path.to_string_lossy().into_owned()),
            bundle_path: None,
            included_by: None,
            resources: consumers(config, "storage", id),
            required: true,
            sensitive: true,
            inspection: "not_checked",
            logical_bytes: None,
            allocated_bytes: None,
        };
        if storage.external {
            input.inspection = "external_unresolved";
            inventory.issues.push(Issue {
                severity: "unknown",
                resource: resource.clone(),
                field: "path",
                code: "backup_external_data",
                message: "External data cannot be captured from this inventory.",
                remedy: "Review external storage inclusion and consistent capture strategy.",
            });
        } else {
            inventory.inspect_path(&mut input, &path, kind == "directory");
        }
        inventory.input(input)?;
        if storage.kind == "file" {
            inventory.requirement(&resource, "disk_dependencies",
                "Verify guest disk format, sparse allocation, backing files and native ownership before capture.")?;
        } else {
            inventory.requirement(&resource, "tree_scan",
                "Perform a bounded, private directory-tree and symlink/mount metadata scan before capture.")?;
        }
        if !storage.mount.is_empty() {
            inventory.requirement(
                &resource,
                "expected_mount",
                "Verify required filesystem identity, mount state and permissions before capture.",
            )?;
        }
        if let Some(source) = &storage.source {
            let key = format!("asset/{id}");
            let path = private_root.join(&source.path);
            let mut input = BackupInput {
                id: key.clone(),
                kind: "image_asset",
                strategy: "bundle_copy",
                path: Some(path.to_string_lossy().into_owned()),
                bundle_path: None,
                included_by: None,
                resources: consumers(config, "storage", id),
                required: true,
                sensitive: true,
                inspection: "not_checked",
                logical_bytes: None,
                allocated_bytes: None,
            };
            inventory.inspect_path(&mut input, &path, false);
            inventory.input(input)?;
            inventory.requirement(
                &key,
                "asset_checksum",
                "Verify the expected SHA-256 image checksum before capture.",
            )?;
        }
    }
    for (id, path) in &config.secrets {
        let resource = format!("secret/{id}");
        let full = private_root.join(path);
        let mut input = BackupInput {
            id: resource.clone(),
            kind: "private_file",
            strategy: "bundle_copy",
            path: Some(full.to_string_lossy().into_owned()),
            bundle_path: None,
            included_by: None,
            resources: consumers(config, "secret", id),
            required: true,
            sensitive: true,
            inspection: "not_checked",
            logical_bytes: None,
            allocated_bytes: None,
        };
        inventory.inspect_path(&mut input, &full, false);
        inventory.input(input)?;
    }
    for (id, service) in &config.services {
        let resource = format!("service/{id}");
        inventory.requirement(
            &resource,
            "workload_consistency",
            "Verify runtime state and quiesce running workloads before any data capture.",
        )?;
        for container_id in service.containers.keys() {
            inventory.requirement(
                &resource,
                "container_image",
                format!("Resolve the configured image for container {container_id} and review registry access before restore."),
            )?;
        }
        if service.vm.is_some() {
            inventory.input(BackupInput {
                id: format!("native/{id}"),
                kind: if service.backend == "incus" {
                    "incus_instance_export"
                } else {
                    "libvirt_guest_state"
                },
                strategy: "native_export",
                path: None,
                bundle_path: None,
                included_by: None,
                resources: vec![resource.clone()],
                required: true,
                sensitive: true,
                inspection: "native_dependencies_unchecked",
                logical_bytes: None,
                allocated_bytes: None,
            })?;
            inventory.requirement(&resource, "native_export",
                "Capture native libvirt/Incus guest state and dependent disks through its verified backend.")?;
            inventory.issues.push(Issue {
                severity: "unknown",
                resource,
                field: "native_state",
                code: "backup_native_dependencies",
                message: "Native guest export dependencies have not been verified by this metadata-only inventory.",
                remedy: "Recheck backend access and native ownership before capture.",
            });
        }
    }
    for (id, share) in &config.shares {
        let resource = format!("share/{id}");
        inventory.requirement(&resource, "share_host_setup",
            "Review client writes, export state, account identities and host permissions before capture.")?;
        if share.enabled {
            inventory.requirement(&resource, "share_quiescence",
                "Coordinate client writes before capture; unpublishing does not disconnect existing Samba sessions.")?;
        }
    }
    for id in config.bindings.keys() {
        inventory.requirement(
            &format!("binding/{id}"),
            "host_binding",
            "Review native networking and host binding prerequisites on the destination.",
        )?;
    }
    Ok(())
}

/// Never returns a completed/restorable backup. No native state or source
/// contents are touched; missing/unknown data remains an explicit blocker.
pub fn inventory(
    store: &Store,
    private_root: &Path,
    storage_root: &Path,
) -> Result<BackupInventory> {
    let snapshot = store
        .read()?
        .context("No authoritative workloads YAML exists to inventory.")?;
    let config = snapshot
        .config()
        .context("A valid private configuration is needed for backup inventory.")?;
    let mut result = BackupInventory {
        stage: "inventory",
        revision: snapshot.revision,
        checked_at: Utc::now(),
        restorable: false,
        inputs: Vec::new(),
        requirements: Vec::new(),
        exclusions: vec![
            Exclusion {
                kind: "derived_native_state",
                reason: "Managed artifacts and caches are regenerated; native config and recovery journals are not captured.",
            },
            Exclusion {
                kind: "host_configuration",
                reason: "Host packages, credentials, groups, server settings and network setup require independent review.",
            },
            Exclusion {
                kind: "container_writable_layers",
                reason: "Unmanaged container layers, engine caches and external volumes are excluded.",
            },
            Exclusion {
                kind: "configuration_history",
                reason: "Only the authoritative current revision is inventoried.",
            },
        ],
        issues: Vec::new(),
    };
    result.requirement("bundle", "private_destination",
        "A capture needs a private, owner-only destination and a checksummed manifest before publication.")?;
    result.requirement("bundle", "metadata",
        "UID/GID, xattrs, ACLs, SELinux labels, hardlinks, symlinks and sparse allocation require verified preservation.")?;
    result.input(BackupInput {
        id: "configuration".to_owned(),
        kind: "configuration",
        strategy: "bundle_copy",
        path: Some(store.path().to_string_lossy().into_owned()),
        bundle_path: None,
        included_by: None,
        resources: vec!["configuration".into()],
        required: true,
        sensitive: true,
        inspection: "metadata_only",
        logical_bytes: Some(snapshot.bytes.len() as u64),
        allocated_bytes: None,
    })?;
    inventory_inputs(&mut result, &config, private_root, storage_root)?;
    annotate_coverage(&mut result, store, private_root);
    ensure!(
        serde_json::to_vec(&result)?.len() <= super::MAX_CONFIG_BYTES,
        "backup inventory output exceeds 1 MiB"
    );
    // Detect a concurrent author edit before returning a revision claim.
    ensure!(
        store
            .read()?
            .as_ref()
            .is_some_and(|latest| latest.revision == result.revision),
        "configuration changed during backup inventory; retry"
    );
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::declarative::MISSING_REVISION;

    #[test]
    fn backup_inventory_is_nonrestorable_and_does_not_copy_private_data() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("workloads");
        let store = Store::new(directory.clone());
        store
            .save(
                b"version: 1\nsecrets:\n  key: private/key.txt\n",
                MISSING_REVISION,
            )
            .unwrap();
        let inventory = inventory(&store, &directory, root.path()).unwrap();
        assert_eq!(inventory.stage, "inventory");
        assert!(!inventory.restorable);
        assert!(
            inventory
                .inputs
                .iter()
                .any(|input| input.id == "secret/key")
        );
        assert!(
            inventory
                .issues
                .iter()
                .any(|issue| issue.code == "backup_path_unavailable")
        );
        assert!(!directory.join("private/key.txt").exists());
    }

    #[test]
    fn declared_consumers_and_overlapping_inputs_match_reference() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("workloads");
        let store = Store::new(directory.clone());
        store.save(
            b"version: 1\nstorage:\n  shared: {kind: directory, path: shared}\nsecrets:\n  nested: storage/shared/app.env\nservices:\n  app:\n    kind: container\n    engine: podman\n    backend: quadlet\n    containers:\n      web:\n        image: example/image:1\n        env_files: [nested]\n        mounts: [{storage: shared, target: /data}]\nshares:\n  files: {protocol: samba, storage: shared, enabled: true, guest: true}\n",
            MISSING_REVISION,
        ).unwrap();
        let storage = directory.join("storage");
        fs::create_dir_all(storage.join("shared")).unwrap();
        let file = storage.join("shared/app.env");
        fs::write(&file, "PRIVATE_CONTENT_SENTINEL").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        let result = inventory(&store, &directory, &storage).unwrap();
        let shared = result
            .inputs
            .iter()
            .find(|input| input.id == "storage/shared")
            .unwrap();
        assert_eq!(shared.bundle_path.as_deref(), Some("storage/shared"));
        assert!(
            shared
                .resources
                .iter()
                .any(|resource| resource == "service/app")
        );
        assert!(
            shared
                .resources
                .iter()
                .any(|resource| resource == "share/files")
        );
        let secret = result
            .inputs
            .iter()
            .find(|input| input.id == "secret/nested")
            .unwrap();
        assert_eq!(secret.included_by.as_deref(), Some("storage/shared"));
        assert!(
            secret
                .resources
                .iter()
                .any(|resource| resource == "service/app")
        );
        assert!(
            result
                .requirements
                .iter()
                .any(|item| item.kind == "share_quiescence")
        );
        assert!(
            !serde_json::to_string(&result)
                .unwrap()
                .contains("PRIVATE_CONTENT_SENTINEL")
        );
        assert_eq!(
            fs::read_to_string(file).unwrap(),
            "PRIVATE_CONTENT_SENTINEL"
        );
    }

    #[test]
    fn malformed_desired_configuration_cannot_be_inventoried_as_restorable() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path().join("workloads"));
        assert!(inventory(&store, root.path(), root.path()).is_err());
    }
}
