use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{Config, MISSING_REVISION, RevisionConflict, Store, parse, revision};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Create,
    Update,
    Remove,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    pub resource: String,
    pub backend: String,
    pub operation: Operation,
    pub before: Option<String>,
    pub after: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub base_revision: String,
    pub desired_revision: String,
    pub changes: Vec<Change>,
    pub fingerprint: String,
}

impl Store {
    /// Review a proposed revision without writing YAML or touching native providers.
    pub fn preview(&self, bytes: &[u8], expected: &str) -> Result<Plan> {
        let current = self.read()?;
        let desired = parse(bytes)?;
        Self::plan(current, bytes, expected, &desired).map(|(_, plan)| plan)
    }

    pub(super) fn plan(
        current: Option<super::Snapshot>,
        bytes: &[u8],
        expected: &str,
        desired: &Config,
    ) -> Result<(Config, Plan)> {
        let base = current
            .as_ref()
            .map_or(MISSING_REVISION, |snapshot| &snapshot.revision);
        if base != expected {
            return Err(RevisionConflict.into());
        }
        let previous = match current {
            Some(snapshot) => snapshot
                .config()
                .context("existing configuration must be repaired before native planning")?,
            None => Config {
                version: 1,
                ..Default::default()
            },
        };
        super::store::validate_existing_guests(&previous, desired)?;
        let desired_revision = revision(bytes);
        let changes = changes(&previous, desired)?;
        let encoded = serde_json::to_vec(&(
            "maestro-workload-plan-v1",
            expected,
            &desired_revision,
            &changes,
        ))?;
        Ok((
            previous,
            Plan {
                base_revision: expected.into(),
                desired_revision,
                changes,
                fingerprint: revision(&encoded),
            },
        ))
    }

    /// Review returning to a verified historical revision.
    pub fn preview_revert(&self, historical_revision: &str, expected: &str) -> Result<Plan> {
        let historical = self.read_history(historical_revision)?;
        self.preview(&historical.bytes, expected)
    }

    /// Rebuild a reviewed plan against current state before a coordinator executes it.
    pub fn verify_review(&self, bytes: &[u8], expected: &str, fingerprint: &str) -> Result<Plan> {
        let plan = self.preview(bytes, expected)?;
        ensure!(
            plan.fingerprint == fingerprint,
            "reviewed plan no longer matches the candidate"
        );
        Ok(plan)
    }
}

struct Resource {
    key: String,
    backend: String,
    fingerprint: String,
}

fn changes(previous: &Config, desired: &Config) -> Result<Vec<Change>> {
    for (id, before) in &previous.storage {
        if let Some(after) = desired.storage.get(id) {
            let managed = super::storage::managed_directory(before);
            ensure!(
                managed == super::storage::managed_directory(after)
                    && (!managed || before.path == after.path),
                "managed storage must retain its path and provisioning mode"
            );
        }
    }
    let old = resources(previous)?;
    let next = resources(desired)?;
    let old_by_key: BTreeMap<_, _> = old
        .iter()
        .map(|resource| (&resource.key, resource))
        .collect();
    let next_by_key: BTreeMap<_, _> = next
        .iter()
        .map(|resource| (&resource.key, resource))
        .collect();

    let mut result = Vec::new();
    let mut storage_removals = Vec::new();
    for resource in old
        .iter()
        .rev()
        .filter(|resource| !next_by_key.contains_key(&resource.key))
    {
        let changes = if resource.key.starts_with("storage/") {
            &mut storage_removals
        } else {
            &mut result
        };
        changes.push(Change {
            resource: resource.key.clone(),
            backend: resource.backend.clone(),
            operation: Operation::Remove,
            before: Some(resource.fingerprint.clone()),
            after: None,
        });
    }
    for resource in next {
        let before = old_by_key.get(&resource.key);
        if let Some(before) = before {
            ensure!(
                before.backend == resource.backend,
                "changing an existing resource backend requires a new definition"
            );
            if before.fingerprint == resource.fingerprint {
                continue;
            }
        }
        result.push(Change {
            resource: resource.key,
            backend: resource.backend,
            operation: if before.is_some() {
                Operation::Update
            } else {
                Operation::Create
            },
            before: before.map(|before| before.fingerprint.clone()),
            after: Some(resource.fingerprint),
        });
    }
    result.extend(storage_removals);
    Ok(result)
}

pub(super) fn service_fingerprint(config: &Config, id: &str) -> Result<String> {
    let key = format!("service/{id}");
    resources(config)?
        .into_iter()
        .find(|resource| resource.key == key)
        .map(|resource| resource.fingerprint)
        .ok_or_else(|| anyhow::anyhow!("unknown workload service"))
}

fn resources(config: &Config) -> Result<Vec<Resource>> {
    let mut result = Vec::new();
    let mut managed_storage: Vec<_> = config
        .storage
        .iter()
        .filter(|(_, storage)| {
            super::storage::managed_directory(storage) || storage.source.is_some()
        })
        .collect();
    managed_storage.sort_unstable_by(|(_, left), (_, right)| left.path.cmp(&right.path));
    for (id, storage) in managed_storage {
        let key = format!("storage/{id}");
        let encoded = serde_json::to_vec(&json!({"key": key, "definition": storage}))?;
        result.push(Resource {
            key,
            backend: if storage.source.is_some() {
                "storage/disk"
            } else {
                "storage"
            }
            .into(),
            fingerprint: revision(&encoded),
        });
    }
    for id in config.service_order()? {
        let service = &config.services[id];
        let mut storage = BTreeMap::new();
        let mut secrets = BTreeMap::new();
        let mut bindings = BTreeMap::new();
        for container in service.containers.values() {
            for mount in &container.mounts {
                storage.insert(&mount.storage, &config.storage[&mount.storage]);
            }
            for secret in &container.env_files {
                secrets.insert(secret, &config.secrets[secret]);
            }
        }
        if let Some(vm) = &service.vm {
            for disk in &vm.disks {
                storage.insert(disk, &config.storage[disk]);
            }
            for binding in [&vm.network, &vm.pool, &vm.project] {
                if let Some(value) = config.bindings.get(binding) {
                    bindings.insert(binding, value);
                }
            }
        }
        let backend = if service.engine.is_empty() {
            service.backend.clone()
        } else {
            format!("{}/{}", service.engine, service.backend)
        };
        let key = format!("service/{id}");
        let encoded = serde_json::to_vec(&json!({
            "key": key, "backend": backend, "definition": service,
            "storage": storage, "privateFiles": secrets, "bindings": bindings,
        }))?;
        result.push(Resource {
            key,
            backend,
            fingerprint: revision(&encoded),
        });
    }
    for (id, share) in &config.shares {
        let key = format!("share/{id}");
        let encoded = serde_json::to_vec(&json!({
            "key": key, "definition": share, "storage": config.storage[&share.storage],
        }))?;
        result.push(Resource {
            key,
            backend: share.protocol.clone(),
            fingerprint: revision(&encoded),
        });
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    const WORKLOADS: &str = r#"
version: 1
storage:
  cache: {path: volumes/cache}
services:
  cache:
    kind: container
    containers:
      app: {image: redis:7}
  web:
    kind: container
    depends_on: [cache]
    containers:
      app:
        image: nginx:alpine
        mounts: [{storage: cache, target: /cache}]
"#;

    #[test]
    fn review_orders_dependents_and_detects_referenced_changes() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let store = Store::new(temp.path().to_path_buf());
        let creation = store
            .preview(WORKLOADS.as_bytes(), MISSING_REVISION)
            .unwrap();
        assert_eq!(
            creation
                .changes
                .iter()
                .map(|change| change.resource.as_str())
                .collect::<Vec<_>>(),
            ["storage/cache", "service/cache", "service/web"]
        );
        assert!(store.read().unwrap().is_none());
        let current = store.save(WORKLOADS.as_bytes(), MISSING_REVISION).unwrap();

        let relocated = WORKLOADS.replace("volumes/cache", "volumes/other");
        assert!(
            store
                .preview(relocated.as_bytes(), &current.revision)
                .is_err()
        );
        let changed = WORKLOADS.replace(
            "storage: cache, target:",
            "storage: cache, path: subdir, target:",
        );
        let update = store
            .preview(changed.as_bytes(), &current.revision)
            .unwrap();
        assert_eq!(update.changes.len(), 1);
        assert_eq!(update.changes[0].resource, "service/web");
        assert_eq!(update.changes[0].operation, Operation::Update);
        assert!(
            store
                .verify_review(changed.as_bytes(), &current.revision, &update.fingerprint)
                .is_ok()
        );
        let different_bytes = format!("# comment\n{changed}");
        assert!(
            store
                .verify_review(
                    different_bytes.as_bytes(),
                    &current.revision,
                    &update.fingerprint
                )
                .is_err()
        );

        let removal = store.preview(b"version: 1\n", &current.revision).unwrap();
        assert_eq!(
            removal
                .changes
                .iter()
                .map(|change| change.resource.as_str())
                .collect::<Vec<_>>(),
            ["service/web", "service/cache", "storage/cache"]
        );
        assert!(
            store
                .verify_review(changed.as_bytes(), MISSING_REVISION, &update.fingerprint)
                .err()
                .unwrap()
                .is::<RevisionConflict>()
        );
    }

    #[test]
    fn revert_review_uses_the_exact_archived_yaml() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let store = Store::new(temp.path().to_path_buf());
        let old = store.save(WORKLOADS.as_bytes(), MISSING_REVISION).unwrap();
        let current = store.save(b"version: 1\n", &old.revision).unwrap();
        let plan = store
            .preview_revert(&old.revision, &current.revision)
            .unwrap();
        assert_eq!(plan.desired_revision, old.revision);
        assert_eq!(plan.changes.len(), 3);
        assert!(
            plan.changes
                .iter()
                .all(|change| change.operation == Operation::Create)
        );
        assert_eq!(store.read().unwrap().unwrap().revision, current.revision);
    }
}
