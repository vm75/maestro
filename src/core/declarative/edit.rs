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
            if input.operation == "remove" {
                ensure!(config.services.remove(&input.id).is_some(),"resource does not exist");
            } else {
                let mut value: Service = serde_json::from_str(definition)
                    .context("invalid typed definition or unknown JSON fields")?;
                for container in value.containers.values_mut() {
                    for mount in &mut container.mounts {
                        resolve_mount_storage(&mut config, &input.id, mount)?;
                    }
                }
                prepare_identity(
                    store,
                    actual,
                    &input.id,
                    config.services.get(&input.id),
                    &mut value,
                )?;
                config.services.insert(input.id.clone(), value);
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

// The typed editor accepts paths, but persisted definitions reference storage.
fn clean_reference_path(path: &str) -> Result<String> {
    ensure!(!path.is_empty() && !path.chars().any(char::is_control),
        "enter a clean host path or select a storage reference");
    let absolute = path.starts_with('/');
    let mut components = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => anyhow::bail!("parent traversal is not allowed in storage paths"),
            part => components.push(part),
        }
    }
    let cleaned = if absolute {
        format!("/{}", components.join("/"))
    } else if components.is_empty() {
        ".".into()
    } else {
        components.join("/")
    };
    ensure!(super::path_valid(&cleaned, absolute), "invalid storage path");
    Ok(cleaned)
}

fn resolve_storage(config: &mut Config, base_id: &str, path: &str, kind: &str) -> Result<String> {
    if let Some((storage_id, storage)) = config.storage.iter().find(|(_, s)| s.path == path) {
        ensure!(
            (kind == "file") == (storage.kind == "file"),
            "storage path is already referenced with a different data type"
        );
        return Ok(storage_id.clone());
    }
    ensure!(super::id_valid(base_id), "invalid generated storage identifier");
    let storage_id = if !config.storage.contains_key(base_id) {
        base_id.to_owned()
    } else {
        (1..=256)
            .map(|n| {
                let suffix = if n == 1 { "-storage".into() } else { format!("-storage-{n}") };
                format!("{}{}", &base_id[..base_id.len().min(63 - suffix.len())], suffix)
            })
            .find(|id| !config.storage.contains_key(id))
            .context("no available storage identifier")?
    };
    config.storage.insert(storage_id.clone(), Storage {
        path: path.into(),
        external: path.starts_with('/'),
        kind: kind.into(),
        ..Storage::default()
    });
    Ok(storage_id)
}

fn resolve_share_storage(config: &mut Config, id: &str, share: &mut Share) -> Result<()> {
    if !share.storage.is_empty() {
        // Accept a full path echoed by a Server Man client; otherwise retain
        // Maestro's existing explicit-reference relative subpath semantics.
        if config.storage.get(&share.storage).is_some_and(|s| s.path == share.path) {
            share.path.clear();
        }
        return Ok(());
    }
    let path = clean_reference_path(&share.path)?;
    share.storage = resolve_storage(config, id, &path, "directory")?;
    share.path.clear();
    Ok(())
}

fn resolve_mount_storage(config: &mut Config, id: &str, mount: &mut super::Mount) -> Result<()> {
    if !mount.storage.is_empty() {
        if config.storage.get(&mount.storage).is_some_and(|s| s.path == mount.path) {
            mount.path.clear();
        }
        return Ok(());
    }
    let path = clean_reference_path(&mount.path)?;
    let kind = match mount.kind.as_str() {
        "file" => "file",
        "directory" => "directory",
        "" | "bind" => {
            if let Some(storage) = config.storage.values().find(|s| s.path == path) {
                if storage.kind == "file" { "file" } else { "directory" }
            } else if path.starts_with('/') && std::fs::metadata(&path).is_ok_and(|m| m.is_file()) {
                "file"
            } else {
                "directory"
            }
        }
        _ => anyhow::bail!("invalid container mount data type"),
    };
    let name = std::path::Path::new(&path).file_name().and_then(|s| s.to_str()).unwrap_or("storage");
    let suffix: String = name.bytes().map(|b| {
        let b = b.to_ascii_lowercase();
        if b.is_ascii_alphanumeric() || b"-_".contains(&b) { b as char } else { '-' }
    }).collect();
    let base = format!("{id}-{suffix}");
    let base = base[..base.len().min(63)].trim_end_matches('-');
    mount.storage = resolve_storage(config, base, &path, kind)?;
    mount.path.clear();
    mount.kind = kind.into();
    Ok(())
}

// Deterministically prepare a fresh identity for this exact reviewed revision.
// Unlike a random generation on each request, this gives the same candidate
// bytes on plan and save, and remains persisted after the initial save.
fn prepare_identity(store: &Store, revision: &str, id: &str, previous: Option<&Service>,
    service: &mut Service) -> Result<bool> {
    use sha2::{Digest, Sha256};
    let Some(vm) = service.vm.as_mut() else { return Ok(false) };
    let mut changed = false;
    if let Some(old) = previous {
        if let Some(prior) = &old.vm {
            ensure!(old.backend == service.backend, "existing VM backend cannot be changed");
            ensure!(vm.uuid.is_empty() || prior.uuid.is_empty() || vm.uuid == prior.uuid,
                "existing VM identity cannot be changed");
            ensure!(vm.mac.is_empty() || prior.mac.is_empty() || vm.mac == prior.mac,
                "existing VM identity cannot be changed");
            if vm.uuid.is_empty() && !prior.uuid.is_empty() { vm.uuid = prior.uuid.clone(); changed = true; }
            if vm.mac.is_empty() && !prior.mac.is_empty() { vm.mac = prior.mac.clone(); changed = true; }
        }
    }
    if vm.uuid.is_empty() || vm.mac.is_empty() {
        // Namespace generated identities by host, configuration and revision.
        // Host identity is never written to the candidate or returned by the API.
        let machine = std::fs::read_to_string("/etc/machine-id").unwrap_or_default();
        let seed = serde_json::to_vec(&("maestro-vm-identity-v1", machine.trim(),
            store.path(), revision, id))?;
        let digest = Sha256::digest(&seed);
        if vm.uuid.is_empty() {
            let mut bytes = [0u8; 16];
            bytes.copy_from_slice(&digest[..16]);
            bytes[6] = (bytes[6] & 0x0f) | 0x80; // RFC 9562 custom/version 8 UUID
            bytes[8] = (bytes[8] & 0x3f) | 0x80;
            vm.uuid = uuid::Uuid::from_bytes(bytes).hyphenated().to_string();
            changed = true;
        }
        if vm.mac.is_empty() {
            let mut bytes = [0u8; 6];
            bytes.copy_from_slice(&digest[16..22]);
            bytes[0] = (bytes[0] | 2) & 0xfe; // locally administered unicast
            vm.mac = bytes.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":");
            changed = true;
        }
    }
    Ok(changed)
}

/// Raw YAML follows the same identity preparation as typed forms. Preserve
/// exact authored bytes if no VM needs identity preparation.
pub fn prepare_yaml_identities(store: &Store, bytes: &[u8], expected: &str) -> Result<Vec<u8>> {
    let mut desired = parse(bytes)?;
    let snapshot = store.read()?;
    let actual = snapshot.as_ref().map_or(MISSING_REVISION, |s| s.revision.as_str());
    ensure!(expected == actual, "configuration revision changed; review again");
    let previous = snapshot.as_ref().map(|s| s.config()).transpose()?.unwrap_or_default();
    let mut changed = false;
    for (id, service) in &mut desired.services {
        changed |= prepare_identity(store, expected, id, previous.services.get(id), service)?;
    }
    if !changed { return Ok(bytes.to_vec()); }
    desired.validate()?;
    let encoded = serde_yaml::to_string(&desired)?.into_bytes();
    parse(&encoded)?;
    Ok(encoded)
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
    fn container_paths_reuse_typed_references_and_keep_review_deterministic() {
        let (_temp, store) = fixture();
        let initial = store.read().unwrap().unwrap().revision;
        store.save(b"version: 1\nstorage:\n  existing: {path: /mnt/media, external: true, mount: /dev/sdb1}\n", &initial).unwrap();
        let request = EditInput {
            kind: "service".into(), id: "app".into(), operation: "save".into(),
            definition: Some(serde_json::json!({
                "kind":"container","engine":"podman","backend":"quadlet",
                "containers":{"web":{"image":"alpine","mounts":[
                    {"path":"/mnt/media/","target":"/media"},
                    {"path":"/etc/localtime","kind":"file","target":"/etc/localtime","read_only":true},
                    {"path":"config","target":"/config"}]}}
            }).to_string()), revision: store.read().unwrap().unwrap().revision,
        };
        let bytes = edit(&store, &request).unwrap();
        assert_eq!(bytes, edit(&store, &request).unwrap());
        let config = parse(&bytes).unwrap();
        let mounts = &config.services["app"].containers["web"].mounts;
        assert_eq!(mounts[0].storage, "existing");
        assert_eq!(config.storage["existing"].mount, "/dev/sdb1");
        assert_eq!(config.storage[&mounts[1].storage].kind, "file");
        assert!(!config.storage[&mounts[2].storage].external);
        assert!(mounts.iter().all(|m| m.path.is_empty()));
        assert_eq!(config.storage.len(), 3);
        assert!(store.read().unwrap().unwrap().config().unwrap().services.is_empty());
    }

    #[test]
    fn vm_identity_is_generated_and_preserved_across_reviews_and_edits() {
        let (_temp, store) = fixture();
        let initial = store.read().unwrap().unwrap().revision;
        store.save(b"version: 1\nstorage:\n  disk: {path: disk.raw, kind: file, format: raw}\nbindings:\n  net: {kind: network, name: default}\n", &initial).unwrap();
        let request = EditInput {
            kind: "service".into(), id: "guest".into(), operation: "save".into(),
            definition: Some(serde_json::json!({"kind":"vm","backend":"libvirt",
                "vm":{"cpus":2,"memory_mib":2048,"network":"net","disks":["disk"]}}).to_string()),
            revision: store.read().unwrap().unwrap().revision,
        };
        let bytes = edit(&store, &request).unwrap();
        assert_eq!(bytes, edit(&store, &request).unwrap());
        let config = parse(&bytes).unwrap();
        let vm = config.services["guest"].vm.as_ref().unwrap();
        assert!(uuid::Uuid::parse_str(&vm.uuid).is_ok());
        assert!(super::super::valid_mac(&vm.mac));
        let saved = store.save(&bytes, &request.revision).unwrap();
        let mut request = request;
        request.revision = saved.revision;
        let edited = parse(&edit(&store, &request).unwrap()).unwrap();
        assert_eq!(edited.services["guest"].vm.as_ref().unwrap().uuid, vm.uuid);
        assert_eq!(edited.services["guest"].vm.as_ref().unwrap().mac, vm.mac);
        assert_eq!(prepare_yaml_identities(&store, &bytes, &request.revision).unwrap(), bytes);
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
