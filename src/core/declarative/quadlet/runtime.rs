use crate::{
    command::CommandRunner,
    core::declarative::{Config, store},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    time::Duration,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ContainerMemberStatus {
    pub name: String,
    pub state: String,
}

pub(super) struct RuntimeGroup {
    pub state: &'static str,
    pub matches: bool,
    pub members: Vec<ContainerMemberStatus>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Names {
    One(String),
    Many(Vec<String>),
}
impl Names {
    fn single(self) -> Result<String> {
        match self {
            Self::One(name) => Ok(name),
            Self::Many(mut names) => {
                ensure!(names.len() == 1, "ambiguous native container names");
                Ok(names.remove(0))
            }
        }
    }
}
#[derive(Deserialize)]
struct Row {
    #[serde(rename = "Names", alias = "names")]
    names: Names,
    #[serde(rename = "Labels", alias = "labels")]
    labels: BTreeMap<String, String>,
    #[serde(rename = "State", alias = "state")]
    state: String,
}

fn expected(config: &Config, id: &str) -> Result<BTreeMap<String, String>> {
    ensure!(
        crate::core::declarative::id_valid(id),
        "invalid managed service ID"
    );
    let service = config
        .services
        .get(id)
        .ok_or_else(|| anyhow::anyhow!("unknown managed service"))?;
    ensure!(
        !service.containers.is_empty() && service.containers.len() <= 256,
        "container inventory exceeds its limit"
    );
    let mut names = BTreeSet::new();
    service
        .containers
        .iter()
        .map(|(member, container)| {
            let name = if container.container_name.is_empty() {
                super::unit_name(id, member)
            } else {
                container.container_name.clone()
            };
            ensure!(
                name.as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                    && name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
                    && names.insert(name.clone()),
                "invalid or duplicate native container name"
            );
            Ok((member.clone(), name))
        })
        .collect()
}

#[cfg(test)]
fn parse(
    bytes: &[u8],
    id: &str,
    fingerprint: &str,
    expected: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, (String, bool)>> {
    parse_with_boot_equivalent(bytes, id, fingerprint, None, expected)
}

fn parse_with_boot_equivalent(
    bytes: &[u8],
    id: &str,
    fingerprint: &str,
    boot_equivalent: Option<&str>,
    expected: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, (String, bool)>> {
    ensure!(
        bytes.len() <= 256 * 1024,
        "container inventory exceeds its limit"
    );
    let rows: Vec<Row> =
        serde_json::from_slice(bytes).map_err(|_| anyhow::anyhow!("invalid Podman inventory"))?;
    ensure!(
        rows.len() <= expected.len(),
        "unexpected containers share the managed identity"
    );
    let mut members = BTreeMap::new();
    for row in rows {
        ensure!(
            row.labels
                .get("io.maestro.service")
                .is_some_and(|value| value == id),
            "native service identity does not match"
        );
        let member = row
            .labels
            .get("io.maestro.container")
            .ok_or_else(|| anyhow::anyhow!("native member identity is missing"))?;
        let name = row.names.single()?;
        ensure!(
            expected.get(member) == Some(&name) && !members.contains_key(member),
            "unknown or duplicate native member identity"
        );
        let native_fingerprint = row
            .labels
            .get("io.maestro.fingerprint")
            .filter(|value| store::valid_revision(value))
            .ok_or_else(|| anyhow::anyhow!("native fingerprint is missing or invalid"))?;
        let state = match row.state.as_str() {
            "running" | "created" | "configured" | "exited" | "stopped" | "paused"
            | "restarting" | "dead" => row.state,
            _ => "unknown".into(),
        };
        let current = native_fingerprint == fingerprint || boot_equivalent == Some(native_fingerprint.as_str());
        members.insert(member.clone(), (state, current));
    }
    Ok(members)
}

pub(super) async fn inspect(
    runner: &CommandRunner,
    program: &Path,
    config: &Config,
    id: &str,
    fingerprint: &str,
) -> Result<RuntimeGroup> {
    ensure!(
        program.is_absolute() && program.is_file() && store::valid_revision(fingerprint),
        "Podman metadata is unavailable"
    );
    let expected = expected(config, id)?;
    let filter = format!("label=io.maestro.service={id}");
    let output = runner
        .capture_status(
            program,
            &[
                "--remote=false",
                "ps",
                "--all",
                "--filter",
                &filter,
                "--format",
                "json",
            ],
            Duration::from_secs(5),
            256 * 1024,
        )
        .await
        .context("Podman metadata is unavailable")?;
    ensure!(
        output.status.success() && !output.truncated,
        "Podman metadata is unavailable or exceeds its limit"
    );
    // The existing ownership fingerprint includes the boot flag. Accept only
    // the exact other boot-policy value for the same complete runtime definition.
    // This also recognizes already-running containers from earlier builds,
    // without weakening exact generated-file and loaded-unit ownership checks.
    let boot_equivalent = if super::super::plan::service_fingerprint(config, id)? == fingerprint {
        let mut other = config.clone();
        let service = other.services.get_mut(id).expect("verified service ID");
        service.enabled = !service.enabled;
        Some(super::super::plan::service_fingerprint(&other, id)?)
    } else {
        None
    };
    let rows = parse_with_boot_equivalent(
        &output.stdout, id, fingerprint, boot_equivalent.as_deref(), &expected,
    )?;
    let mut members = Vec::new();
    let mut running = 0;
    let mut matches = true;
    let mut unknown = false;
    for (member, name) in &expected {
        let state = if let Some((state, current)) = rows.get(member) {
            if state == "running" {
                running += 1;
                matches &= current;
            }
            unknown |= matches!(state.as_str(), "unknown" | "paused" | "restarting" | "dead");
            state.clone()
        } else {
            // A name collision carrying foreign/missing labels must not be
            // mistaken for an absent configured member before systemd start.
            let found = runner
                .capture_status(
                    program,
                    &["--remote=false", "container", "exists", "--", name],
                    Duration::from_secs(5),
                    4096,
                )
                .await
                .context("cannot establish native container absence")?;
            ensure!(
                !found.truncated && found.status.code() == Some(1),
                "native container name is occupied or cannot be inspected"
            );
            "not_running".into()
        };
        members.push(ContainerMemberStatus {
            name: member.clone(),
            state,
        });
    }
    let state = if unknown {
        "unknown"
    } else if running == expected.len() {
        "running"
    } else if running > 0 {
        "partially_running"
    } else {
        "stopped"
    };
    Ok(RuntimeGroup {
        state,
        matches: matches && !unknown,
        members,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    const YAML: &[u8] = b"version: 1\nservices:\n  demo: {kind: container, containers: {web: {image: alpine}, worker: {image: alpine}}}\n";
    fn row(member: &str, state: &str, fingerprint: &str) -> serde_json::Value {
        serde_json::json!({"Names":[super::super::unit_name("demo", member)], "State":state,
            "Labels":{"io.maestro.service":"demo","io.maestro.container":member,"io.maestro.fingerprint":fingerprint}})
    }
    fn stub(root: &Path, rows: &[u8], exists: i32) -> std::path::PathBuf {
        std::fs::write(root.join("rows.json"), rows).unwrap();
        std::fs::write(root.join("exists-code"), exists.to_string()).unwrap();
        let path = root.join("podman");
        // Use an immutable executable from the checkout. Rewriting a script
        // just before spawn can race with inherited write descriptors in other
        // concurrently spawning tests (ETXTBSY on Linux).
        if !path.exists() {
            let executable = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("test-fixtures/phase2/podman-inventory-stub.sh");
            std::os::unix::fs::symlink(executable, &path).unwrap();
        }
        path
    }
    #[tokio::test]
    async fn group_distinguishes_partial_stale_and_stopped_members() {
        let root = tempfile::tempdir().unwrap();
        let config = crate::core::declarative::parse(YAML).unwrap();
        let fingerprint = "a".repeat(64);
        let body = serde_json::to_vec(&vec![row("web", "running", &"b".repeat(64))]).unwrap();
        let program = stub(root.path(), &body, 1);
        let result = inspect(
            &CommandRunner::new(false),
            &program,
            &config,
            "demo",
            &fingerprint,
        )
        .await
        .unwrap();
        assert_eq!(result.state, "partially_running");
        assert!(!result.matches);
        assert_eq!(result.members[0].name, "web");
        assert_eq!(result.members[1].state, "not_running");
        let body = serde_json::to_vec(&vec![
            row("web", "running", &fingerprint),
            row("worker", "running", &fingerprint),
        ])
        .unwrap();
        let program = stub(root.path(), &body, 1);
        let result = inspect(
            &CommandRunner::new(false),
            &program,
            &config,
            "demo",
            &fingerprint,
        )
        .await
        .unwrap();
        assert_eq!(result.state, "running");
        assert!(result.matches);
        let body = serde_json::to_vec(&vec![row("web", "exited", &"b".repeat(64))]).unwrap();
        let program = stub(root.path(), &body, 1);
        let result = inspect(
            &CommandRunner::new(false),
            &program,
            &config,
            "demo",
            &fingerprint,
        )
        .await
        .unwrap();
        assert_eq!(result.state, "stopped");
        assert!(result.matches);
    }
    #[tokio::test]
    async fn boot_only_changes_do_not_make_running_runtime_stale() {
        let root = tempfile::tempdir().unwrap();
        let old = crate::core::declarative::parse(YAML).unwrap();
        let previous = super::super::super::plan::service_fingerprint(&old, "demo").unwrap();
        let mut desired = old.clone();
        desired.services.get_mut("demo").unwrap().enabled = true;
        let current = super::super::super::plan::service_fingerprint(&desired, "demo").unwrap();
        assert_ne!(current, previous);
        let body = serde_json::to_vec(&vec![
            row("web", "running", &previous),
            row("worker", "running", &previous),
        ]).unwrap();
        let program = stub(root.path(), &body, 1);
        let result = inspect(&CommandRunner::new(false), &program, &desired, "demo", &current).await.unwrap();
        assert_eq!(result.state, "running");
        assert!(result.matches);
        desired.services.get_mut("demo").unwrap().containers.get_mut("web").unwrap().image = "busybox".into();
        let changed = super::super::super::plan::service_fingerprint(&desired, "demo").unwrap();
        let result = inspect(&CommandRunner::new(false), &program, &desired, "demo", &changed).await.unwrap();
        assert!(!result.matches);
    }

    #[tokio::test]
    async fn foreign_name_collisions_and_engine_failures_cannot_be_adopted() {
        let root = tempfile::tempdir().unwrap();
        let config = crate::core::declarative::parse(YAML).unwrap();
        for exists in [0, 125] {
            let program = stub(root.path(), b"[]", exists);
            assert!(
                inspect(
                    &CommandRunner::new(false),
                    &program,
                    &config,
                    "demo",
                    &"a".repeat(64)
                )
                .await
                .is_err()
            );
        }
    }
    #[test]
    fn malformed_foreign_duplicate_and_unknown_members_are_rejected() {
        let config = crate::core::declarative::parse(YAML).unwrap();
        let expected = expected(&config, "demo").unwrap();
        let fingerprint = "a".repeat(64);
        let good = row("web", "running", &fingerprint);
        let mut foreign = good.clone();
        foreign["Labels"]["io.maestro.service"] = "another-service".into();
        let mut bad_name = good.clone();
        bad_name["Names"] = serde_json::json!(["foreign-native-name"]);
        for rows in [
            vec![good.clone(), good.clone()],
            vec![foreign],
            vec![bad_name],
            vec![row("unknown", "running", &fingerprint)],
            vec![row("web", "running", "invalid")],
        ] {
            assert!(
                parse(
                    &serde_json::to_vec(&rows).unwrap(),
                    "demo",
                    &fingerprint,
                    &expected
                )
                .is_err()
            );
        }
        assert!(parse(b"{\"broken\":true}", "demo", &fingerprint, &expected).is_err());
    }
    #[tokio::test]
    async fn oversized_native_inventory_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let config = crate::core::declarative::parse(YAML).unwrap();
        let mut body = b"[]".to_vec();
        body.resize(256 * 1024 + 1, b' ');
        let program = stub(root.path(), &body, 1);
        assert!(
            inspect(
                &CommandRunner::new(false),
                &program,
                &config,
                "demo",
                &"a".repeat(64)
            )
            .await
            .is_err()
        );
    }
    #[tokio::test]
    #[ignore = "requires the isolated offline Podman lab"]
    async fn native_rootless_inventory_reports_owned_stale_member() {
        let program = std::env::var_os("MAESTRO_PODMAN_PROGRAM")
            .expect("run make test-podman to select private native engine roots");
        let config = crate::core::declarative::parse(b"version: 1\nservices:\n  offline: {kind: container, containers: {probe: {image: 'localhost/maestro-phase2-offline:lab', container_name: maestro-phase2-offline}}}\n").unwrap();
        let fingerprint =
            crate::core::declarative::plan::service_fingerprint(&config, "offline").unwrap();
        let result = inspect(
            &CommandRunner::new(false),
            Path::new(&program),
            &config,
            "offline",
            &fingerprint,
        )
        .await
        .unwrap();
        assert_eq!(result.state, "running");
        assert!(!result.matches);
        assert_eq!(result.members.len(), 1);
        assert_eq!(result.members[0].name, "probe");
        assert_eq!(result.members[0].state, "running");
    }
    #[tokio::test]
    #[ignore = "requires the isolated offline Podman lab"]
    async fn native_rootless_inventory_rejects_foreign_name() {
        let program = std::env::var_os("MAESTRO_PODMAN_PROGRAM")
            .expect("run make test-podman to select private native engine roots");
        let config = crate::core::declarative::parse(b"version: 1\nservices:\n  offline: {kind: container, containers: {probe: {image: 'localhost/maestro-phase2-offline:lab', container_name: maestro-phase2-offline}}}\n").unwrap();
        let fingerprint =
            crate::core::declarative::plan::service_fingerprint(&config, "offline").unwrap();
        let error = inspect(
            &CommandRunner::new(false),
            Path::new(&program),
            &config,
            "offline",
            &fingerprint,
        )
        .await
        .err()
        .unwrap();
        assert!(error.to_string().contains("name is occupied"));
    }
}
