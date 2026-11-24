//! Strict typed editing of the *existing* authoritative Linux workload YAML.
//! This never creates a parallel resource store or executes native commands.
//! Publication is delegated to Store::apply_review and its recovery journal.
use anyhow::{Context, Result, ensure};
use serde::Deserialize;

use super::{Binding, Config, MISSING_REVISION, Service, Share, Storage, Store, parse};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditInput {
    pub kind: String,
    pub id: String,
    pub operation: String,
    #[serde(default)]
    pub definition: Option<String>,
    pub revision: String,
}

pub fn edit(store: &Store, input: &EditInput) -> Result<Vec<u8>> {
    ensure!(super::id_valid(&input.id), "invalid resource identifier");
    ensure!(
        matches!(input.operation.as_str(), "save" | "remove"),
        "invalid resource operation"
    );
    ensure!(
        matches!(
            input.kind.as_str(),
            "service" | "storage" | "share" | "binding" | "secret"
        ),
        "unsupported resource kind"
    );
    let current = store.read()?;
    let actual = current
        .as_ref()
        .map_or(MISSING_REVISION, |snapshot| snapshot.revision.as_str());
    ensure!(
        input.revision == actual,
        "configuration revision changed; review again"
    );

    // Preserve the recoverability of a malformed existing document. A typed
    // form cannot silently repair or overwrite invalid raw author content.
    let mut config = current.as_ref().map_or_else(
        || {
            Ok::<Config, anyhow::Error>(Config {
                version: 1,
                ..Config::default()
            })
        },
        |snapshot| snapshot.config(),
    )?;

    let definition = input.definition.as_deref().unwrap_or_default();
    ensure!(
        definition.len() <= 256 * 1024,
        "typed definition exceeds 256 KiB"
    );
    if input.operation == "remove" {
        ensure!(
            input.definition.is_none(),
            "remove cannot include a replacement definition"
        );
    } else {
        ensure!(
            !definition.is_empty() && definition.trim() != "null",
            "definition is required"
        );
    }

    macro_rules! edit_map {
        ($map:expr, $ty:ty) => {{
            let target = &mut $map;
            if input.operation == "remove" {
                ensure!(
                    target.remove(&input.id).is_some(),
                    "resource does not exist"
                );
            } else {
                // Structs deny unknown JSON fields. No untyped fragment or shell
                // value can become a native command through this editor.
                let value: $ty = serde_json::from_str(definition)
                    .context("invalid typed definition or unknown JSON fields")?;
                target.insert(input.id.clone(), value);
            }
        }};
    }

    match input.kind.as_str() {
        "service" => {
            // Server Man's PrepareIdentities preserves the UUID and MAC
            // when the editor omits them. Do not generate random identities
            // independently during plan and save: both must use identical
            // reviewed candidate bytes. New guests currently need explicit
            // identities until the reference candidate workflow is ported.
            if input.operation == "remove" {
                ensure!(config.services.remove(&input.id).is_some(),"resource does not exist");
            } else {
                let mut value: Service = serde_json::from_str(definition)
                    .context("invalid typed definition or unknown JSON fields")?;
                if let Some(old) = config.services.get(&input.id) {
                    if let (Some(prior),Some(next)) = (&old.vm,&mut value.vm) {
                        ensure!(old.backend == value.backend,"existing VM backend cannot be changed");
                        ensure!(next.uuid.is_empty() || prior.uuid.is_empty() || next.uuid == prior.uuid,"existing VM identity cannot be changed");
                        ensure!(next.mac.is_empty() || prior.mac.is_empty() || next.mac == prior.mac,"existing VM identity cannot be changed");
                        if next.uuid.is_empty() { next.uuid = prior.uuid.clone(); }
                        if next.mac.is_empty() { next.mac = prior.mac.clone(); }
                    }
                }
                config.services.insert(input.id.clone(),value);
            }
        },
        "storage" => edit_map!(config.storage, Storage),
        "share" => {
            if input.operation == "remove" {
                ensure!(
                    config.shares.remove(&input.id).is_some(),
                    "resource does not exist"
                );
            } else {
                let mut value: Share = serde_json::from_str(definition)
                    .context("invalid typed definition or unknown JSON fields")?;
                resolve_share_storage(&mut config, &input.id, &mut value)?;
                config.shares.insert(input.id.clone(), value);
            }
        }
        "binding" => edit_map!(config.bindings, Binding),
        "secret" => edit_map!(config.secrets, String),
        _ => unreachable!(),
    }
    config.validate()?;
    let yaml =
        serde_yaml::to_string(&config).context("could not encode typed workload definition")?;
    ensure!(
        yaml.len() <= super::MAX_CONFIG_BYTES,
        "resulting YAML exceeds the limit"
    );
    let parsed = parse(yaml.as_bytes())?;
    ensure!(parsed.version == 1, "resulting version is unsupported");
    Ok(yaml.into_bytes())
}

// Server Man's typed share editor accepts a direct directory path and
// translates it to a storage reference in the same reviewed candidate.
// Keep the Rust model's existing storage-relative share.path semantics when
// a reference is explicitly supplied; a manual path is never a subpath.
fn resolve_share_storage(config: &mut Config, id: &str, share: &mut Share) -> Result<()> {
    if !share.storage.is_empty() {
        return Ok(());
    }
    let path = std::path::Path::new(&share.path);
    let external = path.is_absolute();
    ensure!(
        super::path_valid(&share.path, external),
        "enter a clean directory path or select a storage reference"
    );
    if let Some((storage_id, storage)) = config
        .storage
        .iter()
        .find(|(_, storage)| storage.path == share.path)
    {
        ensure!(storage.kind != "file", "share requires directory storage");
        share.storage = storage_id.clone();
    } else {
        // Names must be deterministic for preview/save fingerprints, valid
        // even for maximum-length share IDs, and must not overwrite storage.
        let storage_id = if !config.storage.contains_key(id) {
            id.to_owned()
        } else {
            (1..=256)
                .map(|n| {
                    let suffix = if n == 1 {
                        "-storage".to_owned()
                    } else {
                        format!("-storage-{n}")
                    };
                    format!("{}{}", &id[..id.len().min(63 - suffix.len())], suffix)
                })
                .find(|candidate| !config.storage.contains_key(candidate))
                .context("no available storage identifier")?
        };
        config.storage.insert(
            storage_id.clone(),
            Storage {
                path: share.path.clone(),
                external,
                kind: "directory".into(),
                ..Storage::default()
            },
        );
        share.storage = storage_id;
    }
    // The full directory is now the storage's path. Leaving it here would
    // append the path a second time (or bypass the referenced base).
    share.path.clear();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::declarative::revision;

    fn fixture() -> (tempfile::TempDir, Store) {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::new(temp.path().join("workloads"));
        store.save(b"version: 1\n", MISSING_REVISION).unwrap();
        (temp, store)
    }

    #[test]
    fn typed_storage_edit_uses_authoritative_yaml_and_revision() {
        let (_temp, store) = fixture();
        let original = store.read().unwrap().unwrap();
        let request = EditInput {
            kind: "storage".into(),
            id: "data".into(),
            operation: "save".into(),
            definition: Some(r#"{"kind":"directory","path":"data"}"#.into()),
            revision: original.revision.clone(),
        };
        let candidate = edit(&store, &request).unwrap();
        let config = parse(&candidate).unwrap();
        assert_eq!(config.storage["data"].path, "data");
        assert_eq!(store.read().unwrap().unwrap().revision, original.revision);
        assert_ne!(revision(&candidate), original.revision);
        let mut stale = request;
        stale.revision = MISSING_REVISION.into();
        assert!(edit(&store, &stale).is_err());
    }

    fn share_request(store: &Store, id: &str, definition: serde_json::Value) -> EditInput {
        EditInput {
            kind: "share".into(),
            id: id.into(),
            operation: "save".into(),
            definition: Some(definition.to_string()),
            revision: store.read().unwrap().unwrap().revision,
        }
    }

    #[test]
    fn direct_share_path_creates_reference_without_creating_data() {
        let (temp, store) = fixture();
        let missing = temp.path().join("missing");
        let request = share_request(
            &store,
            "media",
            serde_json::json!({"protocol":"samba","path":missing.to_str().unwrap(),"enabled":true}),
        );
        let candidate = edit(&store, &request).unwrap();
        assert_eq!(candidate, edit(&store, &request).unwrap());
        let config = parse(&candidate).unwrap();
        assert_eq!(config.shares["media"].storage, "media");
        assert!(config.shares["media"].path.is_empty());
        assert_eq!(config.storage["media"].path, missing.to_str().unwrap());
        assert!(config.storage["media"].external);
        assert_eq!(config.storage["media"].kind, "directory");
        assert!(!missing.exists());
        assert_eq!(store.read().unwrap().unwrap().revision, request.revision);
    }

    #[test]
    fn direct_share_reuses_storage_and_keeps_existing_reference_options() {
        let (_temp, store) = fixture();
        let initial = store.read().unwrap().unwrap().revision;
        store.save(
            b"version: 1\nstorage:\n  existing: {kind: directory, path: /mnt/media, external: true, mount: /dev/sdb1}\n",
            &initial,
        ).unwrap();
        let request = share_request(
            &store,
            "media",
            serde_json::json!({"protocol":"samba","path":"/mnt/media"}),
        );
        let config = parse(&edit(&store, &request).unwrap()).unwrap();
        assert_eq!(config.storage.len(), 1);
        assert_eq!(config.shares["media"].storage, "existing");
        assert_eq!(config.storage["existing"].mount, "/dev/sdb1");
        let selected = share_request(
            &store,
            "media",
            serde_json::json!({"protocol":"samba","storage":"existing","path":"subdir"}),
        );
        let config = parse(&edit(&store, &selected).unwrap()).unwrap();
        assert_eq!(config.shares["media"].path, "subdir");
        assert_eq!(config.storage.len(), 1);
    }

    #[test]
    fn direct_share_collision_and_retarget_preserve_old_storage() {
        let (_temp, store) = fixture();
        let initial = store.read().unwrap().unwrap().revision;
        store.save(
            b"version: 1\nstorage:\n  media: {path: old}\n  media-storage: {path: other}\nshares:\n  media: {protocol: samba, storage: media}\n",
            &initial,
        ).unwrap();
        let request = share_request(
            &store,
            "media",
            serde_json::json!({"protocol":"samba","path":"new"}),
        );
        let config = parse(&edit(&store, &request).unwrap()).unwrap();
        assert_eq!(config.shares["media"].storage, "media-storage-2");
        assert_eq!(config.storage["media"].path, "old");
        assert_eq!(config.storage["media-storage"].path, "other");
        assert_eq!(config.storage["media-storage-2"].path, "new");
        assert!(!config.storage["media-storage-2"].external);
    }

    #[test]
    fn direct_share_rejects_invalid_paths_files_and_unknown_references() {
        let (_temp, store) = fixture();
        let initial = store.read().unwrap().unwrap().revision;
        store.save(
            b"version: 1\nstorage:\n  disk: {kind: file, path: disk.qcow2}\n",
            &initial,
        ).unwrap();
        for definition in [
            serde_json::json!({"protocol":"samba","path":""}),
            serde_json::json!({"protocol":"samba","path":"../escape"}),
            serde_json::json!({"protocol":"samba","path":"disk.qcow2"}),
            serde_json::json!({"protocol":"samba","storage":"missing"}),
            serde_json::json!({"protocol":"samba","storage":"disk"}),
        ] {
            assert!(edit(&store, &share_request(&store, "media", definition)).is_err());
        }
    }

    #[test]
    fn rejects_unknown_fields_and_unsafe_removals() {
        let (_temp, store) = fixture();
        let revision = store.read().unwrap().unwrap().revision;
        for definition in [
            r#"{"kind":"directory","path":"data","command":"sh"}"#,
            "null",
            r#"{"kind":"directory","path":"../escape"}"#,
        ] {
            let request = EditInput {
                kind: "storage".into(),
                id: "data".into(),
                operation: "save".into(),
                definition: Some(definition.into()),
                revision: revision.clone(),
            };
            assert!(edit(&store, &request).is_err());
        }
        assert!(
            edit(
                &store,
                &EditInput {
                    kind: "storage".into(),
                    id: "missing".into(),
                    operation: "remove".into(),
                    definition: None,
                    revision,
                }
            )
            .is_err()
        );
    }
}
