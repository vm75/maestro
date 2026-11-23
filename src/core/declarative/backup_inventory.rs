//! Read-only, bounded inventory for partial (NON-RESTORABLE) Linux capture.
 //! This is not a backup/capture/verification implementation. It never opens
 //! private file contents, follows links or changes native workloads.
use std::{
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
    pub description: &'static str,
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
    fn input(&mut self, input: BackupInput) -> Result<()> {
        ensure!(self.inputs.len() + self.requirements.len() < MAX_ENTRIES,
            "backup input inventory exceeds its bounded entry limit");
        self.inputs.push(input);
        Ok(())
    }

    fn requirement(&mut self, resource: &str, kind: &'static str, description: &'static str) -> Result<()> {
        ensure!(self.inputs.len() + self.requirements.len() < MAX_ENTRIES,
            "backup requirements exceed their bounded entry limit");
        self.requirements.push(Requirement {
            resource: resource.to_owned(), kind, description,
        });
        Ok(())
    }

    /// Metadata only; never call canonicalize/open/read on the referenced file.
    /// Symlinked intermediate components are not traversed, even when their
    /// ultimate target lies within the intended root.
    fn inspect_path(&mut self, input: &mut BackupInput, path: &Path, directory: bool) {
        if !path.is_absolute() {
            input.inspection = "unavailable";
            self.issues.push(issue(&input.id, "path", "backup_path_invalid",
                "An absolute, validated path is required.",
                "Review the resource path before capture."));
            return;
        }
        let mut current = PathBuf::new();
        for component in path.components() {
            current.push(component.as_os_str());
            match fs::symlink_metadata(&current) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    input.inspection = "unverified_symlink";
                    self.issues.push(issue(&input.id, "path", "backup_symlink",
                        "A backup input path contains a symlink.",
                        "Use real directories and review the intended file separately."));
                    return;
                }
                Ok(_) => {}
                Err(_) => {
                    input.inspection = "unavailable";
                    self.issues.push(issue(&input.id, "path", "backup_path_unavailable",
                        "Backup input cannot be inspected with ordinary account permissions.",
                        "Restore the expected path and appropriate read access."));
                    return;
                }
            }
        }
        match fs::symlink_metadata(path) {
            Ok(meta) if if directory { meta.is_dir() } else { meta.is_file() } => {
                let mode = meta.permissions().mode();
                if input.sensitive && (meta.uid() != unsafe { libc::geteuid() } || mode & 0o077 != 0) {
                    input.inspection = "unsafe_permissions";
                    self.issues.push(issue(&input.id, "path", "backup_private_permissions",
                        "A sensitive input has unexpected ownership or permissions.",
                        "Restrict the private input to the workload account."));
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
                self.issues.push(issue(&input.id, "path", "backup_path_type",
                    "Backup input is not the expected file or directory.",
                    "Restore a correctly typed path before attempting capture."));
            }
        }
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
        let kind = if storage.kind == "file" { "disk" } else { "directory" };
        let mut input = BackupInput {
            id: resource.clone(),
            kind,
            strategy: if storage.external { "external_data" } else { "bundle_copy" },
            path: Some(path.to_string_lossy().into_owned()),
            resources: vec![resource.clone()],
            required: true,
            sensitive: false,
            inspection: "not_checked",
            logical_bytes: None,
            allocated_bytes: None,
        };
        if storage.external {
            input.inspection = "external_unresolved";
            inventory.issues.push(Issue {
                severity: "unknown", resource: resource.clone(),
                field: "path", code: "backup_external_data",
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
            inventory.requirement(&resource, "expected_mount",
                "Verify required filesystem identity, mount state and permissions before capture.")?;
        }
        if let Some(source) = &storage.source {
            let key = format!("image/{id}");
            let path = private_root.join(&source.path);
            let mut input = BackupInput {
                id: key.clone(),
                kind: "image_asset",
                strategy: "bundle_copy",
                path: Some(path.to_string_lossy().into_owned()),
                resources: vec![resource.clone()],
                required: true,
                sensitive: false,
                inspection: "not_checked",
                logical_bytes: None,
                allocated_bytes: None,
            };
            inventory.inspect_path(&mut input, &path, false);
            inventory.input(input)?;
            inventory.requirement(&key, "asset_checksum",
                "Verify the expected SHA-256 image checksum before capture.")?;
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
            resources: vec![resource],
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
        inventory.requirement(&resource, "workload_consistency",
            "Verify runtime state and quiesce running workloads before any data capture.")?;
        if service.vm.is_some() {
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
    for id in config.shares.keys() {
        inventory.requirement(&format!("share/{id}"), "share_host_setup",
            "Review client writes, export state, account identities and host permissions before capture.")?;
    }
    for id in config.bindings.keys() {
        inventory.requirement(&format!("binding/{id}"), "host_binding",
            "Review native networking and host binding prerequisites on the destination.")?;
    }
    Ok(())
}

/// Never returns a completed/restorable backup. No native state or source
/// contents are touched; missing/unknown data remains an explicit blocker.
pub fn inventory(store: &Store, private_root: &Path, storage_root: &Path) -> Result<BackupInventory> {
    let snapshot = store.read()?.context("No authoritative workloads YAML exists to inventory.")?;
    let config = snapshot.config().context("A valid private configuration is needed for backup inventory.")?;
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
        path: Some(private_root.join("workloads.yaml").to_string_lossy().into_owned()),
        resources: vec![],
        required: true,
        sensitive: true,
        inspection: "metadata_only",
        logical_bytes: Some(snapshot.bytes.len() as u64),
        allocated_bytes: None,
    })?;
    inventory_inputs(&mut result, &config, private_root, storage_root)?;
    // Detect a concurrent author edit before returning a revision claim.
    ensure!(store.read()?.as_ref().is_some_and(|latest| latest.revision == result.revision),
        "configuration changed during backup inventory; retry");
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
        fs::create_dir(&directory).unwrap();
        let store = Store::new(directory.clone());
        store.save(b"version: 1\nsecrets:\n  key: private/key.txt\n", MISSING_REVISION).unwrap();
        let inventory = inventory(&store, &directory, root.path()).unwrap();
        assert_eq!(inventory.stage, "inventory");
        assert!(!inventory.restorable);
        assert!(inventory.inputs.iter().any(|input| input.id == "secret/key"));
        assert!(inventory.issues.iter().any(|issue| issue.code == "backup_path_unavailable"));
        assert!(!directory.join("private/key.txt").exists());
    }

    #[test]
    fn malformed_desired_configuration_cannot_be_inventoried_as_restorable() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path().join("workloads"));
        assert!(inventory(&store, root.path(), root.path()).is_err());
    }
}
