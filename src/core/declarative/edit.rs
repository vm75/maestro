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
        "service" => edit_map!(config.services, Service),
        "storage" => edit_map!(config.storage, Storage),
        "share" => edit_map!(config.shares, Share),
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
