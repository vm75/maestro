use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Result, ensure};

use crate::{
    command::CommandRunner,
    core::declarative::{ApplyMode, ApplyProvider, Change, Config, ObservedResource},
};

use super::{QuadletArtifacts, publication::Transition};

pub struct QuadletProvider {
    artifacts: QuadletArtifacts,
    runner: CommandRunner,
    systemctl: PathBuf,
    podman: PathBuf,
}

pub struct QuadletStatus {
    pub state: &'static str,
    pub runtime_matches: bool,
    pub restart_required: bool,
    pub members: Vec<super::ContainerMemberStatus>,
}

impl QuadletProvider {
    pub fn new(
        artifacts: QuadletArtifacts,
        runner: CommandRunner,
        systemctl: PathBuf,
        podman: PathBuf,
    ) -> Result<Self> {
        ensure!(
            systemctl.is_absolute() && systemctl.is_file(),
            "configured systemctl is unavailable"
        );
        ensure!(
            podman.is_absolute() && podman.is_file(),
            "configured Podman is unavailable"
        );
        ensure!(
            unsafe { libc::geteuid() } != 0,
            "Quadlet requires a rootless user manager"
        );
        Ok(Self {
            artifacts,
            runner,
            systemctl,
            podman,
        })
    }

    async fn inspect(&self, unit: &str, expected_source: &Path, fingerprint: &str) -> Result<()> {
        ensure!(
            self.inspect_owned(unit, expected_source, fingerprint)
                .await?
                == "inactive",
            "Quadlet unit must be inactive"
        );
        Ok(())
    }

    async fn inspect_owned(
        &self,
        unit: &str,
        expected_source: &Path,
        fingerprint: &str,
    ) -> Result<String> {
        let output = self
            .runner
            .capture_status(
                &self.systemctl,
                &[
                    "--user",
                    "--no-pager",
                    "--no-ask-password",
                    "show",
                    "--property=LoadState,ActiveState,SourcePath,ExecStart",
                    "--",
                    unit,
                ],
                Duration::from_secs(12),
                32 * 1024,
            )
            .await?;
        let text = std::str::from_utf8(&output.stdout)?;
        let properties = unit_state(text);
        ensure!(
            output.status.success() && !output.had_stderr,
            "cannot inspect rootless systemd unit"
        );
        let (load, active, source) = properties;
        ensure!(
            load == Some("loaded")
                && source.is_some_and(|source| Path::new(source) == expected_source)
                && loaded_fingerprint(text, fingerprint),
            "Quadlet unit must be loaded with the reviewed source and fingerprint"
        );
        active
            .map(str::to_owned)
            .ok_or_else(|| anyhow::anyhow!("missing Quadlet active state"))
    }

    pub async fn action(&self, config: &Config, id: &str, action: &str) -> Result<String> {
        ensure!(
            matches!(action, "start" | "stop" | "restart"),
            "unsupported Quadlet action"
        );
        ensure!(config.services.contains_key(id), "unknown workload service");
        let mut affected = std::collections::BTreeSet::new();
        let mut pending = vec![id];
        while let Some(service) = pending.pop() {
            if affected.insert(service) {
                pending.extend(
                    config.services[service]
                        .depends_on
                        .iter()
                        .map(String::as_str),
                );
            }
        }
        let mut units = Vec::new();
        for service in affected {
            let verified = self.owned_units(config, service).await?;
            let runtime = self.runtime_status(config, service, &verified).await?;
            ensure!(
                runtime.state != "unknown",
                "native runtime and managed units cannot be reconciled"
            );
            ensure!(
                action != "start" || runtime.runtime_matches,
                "running containers use older settings; use Restart to reconcile them"
            );
            if service == id {
                units.extend(verified.into_iter().map(|(unit, _)| unit));
            }
        }
        ensure!(!units.is_empty(), "no generated Quadlet container units");
        let mut args = vec!["--user", "--no-pager", "--no-ask-password", action, "--"];
        args.extend(units.iter().map(String::as_str));
        let (output, _) = self
            .runner
            .capture_tail(&self.systemctl, &args, Duration::from_secs(60), 128 * 1024)
            .await?;
        Ok(String::from_utf8_lossy(&output).into_owned())
    }

    async fn owned_units(&self, config: &Config, id: &str) -> Result<Vec<(String, String)>> {
        let fingerprint = crate::core::declarative::plan::service_fingerprint(config, id)?;
        let change = Change {
            resource: format!("service/{id}"),
            backend: "podman/quadlet".into(),
            operation: crate::core::declarative::Operation::Create,
            before: None,
            after: Some(fingerprint.clone()),
        };
        let empty = Config::default();
        ensure!(
            self.artifacts.observe(&change, &empty, config)? == ObservedResource::After,
            "Quadlet files do not match the saved workload revision"
        );
        let transition = Transition::new(&change, &empty, config, &self.artifacts.storage_base)?;
        let mut units = Vec::new();
        for file in transition.after.keys() {
            if let Some(unit) = generated_service(file) {
                let active = self
                    .inspect_owned(&unit, &self.artifacts.directory.join(file), &fingerprint)
                    .await?;
                if file.ends_with(".container") {
                    units.push((unit, active));
                }
            }
        }
        ensure!(!units.is_empty(), "no generated Quadlet container units");
        Ok(units)
    }

    pub async fn status(&self, config: &Config, id: &str) -> Result<&'static str> {
        Ok(self.status_details(config, id).await?.state)
    }

    pub async fn status_details(&self, config: &Config, id: &str) -> Result<QuadletStatus> {
        let units = self.owned_units(config, id).await?;
        self.runtime_status(config, id, &units).await
    }

    async fn runtime_status(
        &self,
        config: &Config,
        id: &str,
        units: &[(String, String)],
    ) -> Result<QuadletStatus> {
        let fingerprint = crate::core::declarative::plan::service_fingerprint(config, id)?;
        let runtime =
            super::runtime::inspect(&self.runner, &self.podman, config, id, &fingerprint).await?;
        let native = group_state(units.iter().map(|(_, state)| state.as_str()));
        let consistent = unit_members_match_runtime(id, units, &runtime.members);
        let state = if !matches!(native, "running" | "stopped" | "mixed") || !consistent {
            "unknown"
        } else {
            runtime.state
        };
        Ok(QuadletStatus {
            state,
            runtime_matches: runtime.matches && state != "unknown",
            restart_required: !runtime.matches && matches!(state, "running" | "partially_running"),
            members: runtime.members,
        })
    }

    pub async fn logs(
        &self,
        config: &Config,
        id: &str,
        journalctl: &Path,
        tail: usize,
    ) -> Result<String> {
        Ok(self.managed_logs(config, id, journalctl, tail).await?.display_text())
    }

    pub async fn managed_logs(
        &self,
        config: &Config,
        id: &str,
        journalctl: &Path,
        tail: usize,
    ) -> Result<crate::core::declarative::composite::ManagedLogs> {
        ensure!(
            (1..=2000).contains(&tail),
            "journal line limit must be between 1 and 2000"
        );
        let units = self.owned_units(config, id).await?;
        let lines = tail.to_string();
        let mut args = vec![
            "--user",
            "--no-pager",
            "--quiet",
            "--output=short-iso",
            "--lines",
            &lines,
        ];
        for (unit, _) in &units {
            args.extend(["--user-unit", unit]);
        }
        let (bytes, truncated) = self
            .runner
            .capture_tail(journalctl, &args, Duration::from_secs(10), 128 * 1024)
            .await?;
        Ok(crate::core::declarative::composite::ManagedLogs {
            text: String::from_utf8_lossy(&bytes).into_owned(),
            truncated,
            source: "systemd-journal",
        })
    }

    async fn check_mode(
        &self,
        change: &Change,
        previous: &Config,
        desired: &Config,
        mode: ApplyMode,
    ) -> Result<()> {
        ensure!(
            self.artifacts.observe(change, previous, desired)? == ObservedResource::Before,
            "Quadlet files do not match the reviewed prior state"
        );
        self.runner
            .capture_bounded(
                &self.systemctl,
                &[
                    "--user",
                    "--no-pager",
                    "--no-ask-password",
                    "show",
                    "--property=Version",
                ],
                Duration::from_secs(12),
                32 * 1024,
            )
            .await?;
        let transition = Transition::new(change, previous, desired, &self.artifacts.storage_base)?;
        for file in transition.before.keys().chain(
            transition
                .after
                .keys()
                .filter(|name| !transition.before.contains_key(*name)),
        ) {
            let Some(unit) = generated_service(file) else {
                continue;
            };
            let source = self.artifacts.directory.join(file);
            let exists_before = transition.before.contains_key(file);
            if exists_before {
                let unit_mode = if transition.after.contains_key(file) {
                    mode
                } else {
                    ApplyMode::Now
                };
                self.inspect_mode(
                    &unit,
                    &source,
                    change.before.as_deref().unwrap_or_default(),
                    unit_mode,
                )
                .await?;
            } else {
                self.ensure_absent(&unit).await?;
            }
        }
        Ok(())
    }

    async fn inspect_mode(
        &self,
        unit: &str,
        source: &Path,
        fingerprint: &str,
        mode: ApplyMode,
    ) -> Result<()> {
        if mode == ApplyMode::Now {
            return self.inspect(unit, source, fingerprint).await;
        }
        let state = self.inspect_owned(unit, source, fingerprint).await?;
        ensure!(
            matches!(state.as_str(), "active" | "inactive"),
            "Quadlet unit is transitioning; retry after it settles"
        );
        Ok(())
    }

    async fn ensure_absent(&self, unit: &str) -> Result<()> {
        let output = self
            .runner
            .capture_status(
                &self.systemctl,
                &[
                    "--user",
                    "--no-pager",
                    "--no-ask-password",
                    "show",
                    "--property=LoadState,ActiveState,SourcePath",
                    "--",
                    unit,
                ],
                Duration::from_secs(12),
                32 * 1024,
            )
            .await?;
        ensure!(
            !output.had_stderr
                && unit_state(std::str::from_utf8(&output.stdout)?)
                    == (Some("not-found"), Some("inactive"), Some("")),
            "a unit with the generated Quadlet name already exists"
        );
        Ok(())
    }
}

impl crate::core::declarative::RecoveryProvider for QuadletProvider {
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
        let observed = self.artifacts.observe(change, previous, desired)?;
        let transition = Transition::new(change, previous, desired, &self.artifacts.storage_base)?;
        let verified = match observed {
            ObservedResource::Before => {
                self.verify_loaded_mode(
                    &transition.before,
                    &transition.after,
                    change.before.as_deref(),
                    mode,
                )
                .await
            }
            ObservedResource::After => {
                self.verify_loaded_mode(
                    &transition.after,
                    &transition.before,
                    change.after.as_deref(),
                    mode,
                )
                .await
            }
            ObservedResource::Unknown => return Ok(ObservedResource::Unknown),
        };
        Ok(if verified.is_ok() {
            observed
        } else {
            ObservedResource::Unknown
        })
    }
}

impl ApplyProvider for QuadletProvider {
    async fn preflight(&self, change: &Change, previous: &Config, desired: &Config) -> Result<()> {
        self.preflight_mode(change, previous, desired, ApplyMode::Now)
            .await
    }
    async fn preflight_mode(
        &self,
        change: &Change,
        previous: &Config,
        desired: &Config,
        mode: ApplyMode,
    ) -> Result<()> {
        self.check_mode(change, previous, desired, mode).await
    }

    async fn apply(&self, change: &Change, previous: &Config, desired: &Config) -> Result<()> {
        self.apply_mode(change, previous, desired, ApplyMode::Now)
            .await
    }
    async fn apply_mode(
        &self,
        change: &Change,
        previous: &Config,
        desired: &Config,
        mode: ApplyMode,
    ) -> Result<()> {
        self.check_mode(change, previous, desired, mode).await?;
        let transition = Transition::new(change, previous, desired, &self.artifacts.storage_base)?;
        self.artifacts.publish(change, previous, desired)?;
        self.runner
            .capture_bounded(
                &self.systemctl,
                &["--user", "--no-pager", "--no-ask-password", "daemon-reload"],
                Duration::from_secs(30),
                32 * 1024,
            )
            .await?;
        self.verify_loaded_mode(
            &transition.after,
            &transition.before,
            change.after.as_deref(),
            mode,
        )
        .await
    }
}

impl QuadletProvider {
    async fn verify_loaded_mode(
        &self,
        expected: &std::collections::BTreeMap<String, String>,
        other: &std::collections::BTreeMap<String, String>,
        fingerprint: Option<&str>,
        mode: ApplyMode,
    ) -> Result<()> {
        for name in expected.keys() {
            if let Some(unit) = generated_service(name) {
                let fingerprint = fingerprint
                    .ok_or_else(|| anyhow::anyhow!("missing expected Quadlet fingerprint"))?;
                self.inspect_mode(
                    &unit,
                    &self.artifacts.directory.join(name),
                    fingerprint,
                    mode,
                )
                .await?;
            }
        }
        for name in other.keys().filter(|name| !expected.contains_key(*name)) {
            if let Some(unit) = generated_service(name) {
                self.ensure_absent(&unit).await?;
            }
        }
        Ok(())
    }
}

fn unit_members_match_runtime(
    id: &str,
    units: &[(String, String)],
    members: &[super::ContainerMemberStatus],
) -> bool {
    members.iter().all(|member| {
        let unit = format!("{}.service", super::unit_name(id, &member.name));
        let active = units
            .iter()
            .find(|(name, _)| name == &unit)
            .map(|(_, active)| active.as_str());
        matches!(
            (active, member.state.as_str()),
            (Some("active"), "running")
                | (
                    Some("inactive"),
                    "created" | "configured" | "exited" | "stopped" | "not_running"
                )
        )
    })
}

fn group_state<'a>(states: impl Iterator<Item = &'a str>) -> &'static str {
    let mut result = None;
    for state in states {
        let state = match state {
            "active" => "running",
            "inactive" => "stopped",
            "failed" => "failed",
            "activating" | "deactivating" | "reloading" => "transitioning",
            _ => "unknown",
        };
        if result.is_some_and(|previous| previous != state) {
            return "mixed";
        }
        result = Some(state);
    }
    result.unwrap_or("unknown")
}

fn loaded_fingerprint(properties: &str, fingerprint: &str) -> bool {
    if !crate::core::declarative::store::valid_revision(fingerprint) {
        return false;
    }
    let label = format!("io.maestro.fingerprint={fingerprint}");
    properties
        .lines()
        .filter_map(|line| line.strip_prefix("ExecStart="))
        .any(|line| {
            let mut words = line.split_whitespace().peekable();
            while let Some(word) = words.next() {
                if word.strip_prefix("--label=") == Some(label.as_str())
                    || (word == "--label" && words.peek() == Some(&label.as_str()))
                {
                    return true;
                }
            }
            false
        })
}

fn generated_service(file: &str) -> Option<String> {
    file.strip_suffix(".container")
        .map(|stem| format!("{stem}.service"))
        .or_else(|| {
            file.strip_suffix(".network")
                .map(|stem| format!("{stem}-network.service"))
        })
}

fn unit_state(text: &str) -> (Option<&str>, Option<&str>, Option<&str>) {
    let mut load = None;
    let mut active = None;
    let mut source = None;
    for (key, value) in text.lines().filter_map(|line| line.split_once('=')) {
        match key {
            "LoadState" => load = Some(value),
            "ActiveState" => active = Some(value),
            "SourcePath" => source = Some(value),
            _ => {}
        }
    }
    (load, active, source)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loaded_unit_requires_the_exact_fingerprint_label() {
        let expected = "a".repeat(64);
        let stale = "b".repeat(64);
        assert!(loaded_fingerprint(
            &format!(
                "ExecStart={{ argv[]=/usr/bin/podman --label io.maestro.fingerprint={expected} }}"
            ),
            &expected
        ));
        assert!(loaded_fingerprint(
            &format!(
                "ExecStart={{ argv[]=/usr/bin/podman --label=io.maestro.fingerprint={expected} }}"
            ),
            &expected
        ));
        assert!(!loaded_fingerprint(
            &format!(
                "ExecStart={{ argv[]=/usr/bin/podman --label=io.maestro.fingerprint={stale} }}"
            ),
            &expected
        ));
        assert!(!loaded_fingerprint(
            &format!("Environment=--label=io.maestro.fingerprint={expected}"),
            &expected
        ));
    }

    #[test]
    fn group_state_preserves_failed_and_mixed_activity() {
        assert_eq!(group_state(["active", "active"].into_iter()), "running");
        assert_eq!(group_state(["inactive", "failed"].into_iter()), "mixed");
        assert_eq!(group_state(["failed"].into_iter()), "failed");
        assert_eq!(group_state(["activating"].into_iter()), "transitioning");
    }

    #[test]
    fn runtime_members_must_match_their_own_unit_activity() {
        let units = vec![
            (
                format!("{}.service", super::super::unit_name("demo", "web")),
                "active".into(),
            ),
            (
                format!("{}.service", super::super::unit_name("demo", "worker")),
                "inactive".into(),
            ),
        ];
        let mut members = vec![
            super::super::ContainerMemberStatus {
                name: "web".into(),
                state: "running".into(),
            },
            super::super::ContainerMemberStatus {
                name: "worker".into(),
                state: "not_running".into(),
            },
        ];
        assert!(unit_members_match_runtime("demo", &units, &members));
        // The aggregate still has one running member, but the wrong unit owns it.
        members[0].state = "not_running".into();
        members[1].state = "running".into();
        assert!(!unit_members_match_runtime("demo", &units, &members));
    }

    #[tokio::test]
    async fn next_start_updates_active_definitions_without_lifecycle_commands() {
        use crate::core::declarative::{MISSING_REVISION, RecoveryProvider, Store, parse};
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let directory = root.path().join("units");
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        let old_yaml = b"version: 1\nservices:\n  demo: {kind: container, containers: {app: {image: alpine}}}\n";
        let next_yaml = b"version: 1\nservices:\n  demo: {kind: container, containers: {app: {image: busybox}}}\n";
        let old = parse(old_yaml).unwrap();
        let next = parse(next_yaml).unwrap();
        let fingerprint =
            crate::core::declarative::plan::service_fingerprint(&old, "demo").unwrap();
        let files = super::super::render_quadlet(&old, "demo", root.path(), &fingerprint).unwrap();
        for (name, contents) in &files {
            let path = directory.join(name);
            std::fs::write(&path, contents).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let script = root.path().join("systemctl");
        let calls = root.path().join("calls");
        let mut body = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$4\" >> '{}'\nif [ \"$4\" = daemon-reload ]; then exit 0; fi\n[ \"$4\" = show ] || exit 19\nif [ \"$5\" = --property=Version ]; then printf 'Version=fixture\\n'; exit 0; fi\ncase \"$7\" in\n",
            calls.display()
        );
        for name in files.keys() {
            let unit = generated_service(name).unwrap();
            let source = directory.join(name);
            body.push_str(&format!("{unit}) file='{}' ;;\n", source.display()));
        }
        body.push_str("*) exit 23 ;;\nesac\nlabel=''\nwhile IFS= read -r line; do case \"$line\" in Label=io.maestro.fingerprint=*) label=${line#Label=} ;; esac; done < \"$file\"\nprintf '%s\\n' 'LoadState=loaded' 'ActiveState=active' \"SourcePath=$file\" \"ExecStart=--label=$label\"\n");
        std::fs::write(&script, body).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut provider = QuadletProvider {
            artifacts: QuadletArtifacts::new(directory.clone(), root.path().to_path_buf()).unwrap(),
            runner: CommandRunner::new(false),
            systemctl: script,
            podman: "/bin/false".into(),
        };
        let store = Store::new(root.path().join("state"));
        let original = store.save(old_yaml, MISSING_REVISION).unwrap();
        let plan = store.preview(next_yaml, &original.revision).unwrap();
        assert!(
            store
                .apply_review(next_yaml, &original.revision, &plan.fingerprint, &provider)
                .await
                .is_err()
        );
        assert_eq!(store.read().unwrap().unwrap().bytes, old_yaml);
        let saved = store
            .apply_review_mode(
                next_yaml,
                &original.revision,
                &plan.fingerprint,
                ApplyMode::NextStart,
                &provider,
            )
            .await
            .unwrap();
        assert_eq!(saved.bytes, next_yaml);
        assert_eq!(
            provider
                .observe_mode(&plan.changes[0], &old, &next, ApplyMode::NextStart)
                .await
                .unwrap(),
            ObservedResource::After
        );
        assert_eq!(
            provider
                .observe(&plan.changes[0], &old, &next)
                .await
                .unwrap(),
            ObservedResource::Unknown
        );
        let podman = root.path().join("podman");
        let mut inventory = serde_json::json!([{
            "Names":[super::super::unit_name("demo", "app")],"State":"running",
            "Labels":{"io.maestro.service":"demo","io.maestro.container":"app","io.maestro.fingerprint":fingerprint}
        }]);
        let inventory_path = root.path().join("inventory.json");
        std::fs::write(&inventory_path, serde_json::to_vec(&inventory).unwrap()).unwrap();
        std::fs::write(
            &podman,
            "#!/bin/sh\n[ \"$2\" = ps ] || exit 1\ncat -- \"${0%/*}/inventory.json\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&podman, std::fs::Permissions::from_mode(0o700)).unwrap();
        provider.podman = podman;
        let status = provider.status_details(&next, "demo").await.unwrap();
        assert_eq!(status.state, "running");
        assert!(status.restart_required);
        assert!(!status.runtime_matches);
        assert_eq!(status.members[0].name, "app");
        assert!(provider.action(&next, "demo", "start").await.is_err());
        inventory[0]["Labels"]["io.maestro.service"] = "foreign".into();
        std::fs::write(&inventory_path, serde_json::to_vec(&inventory).unwrap()).unwrap();
        assert!(provider.status_details(&next, "demo").await.is_err());
        assert!(provider.action(&next, "demo", "restart").await.is_err());
        let removal = store.preview(b"version: 1\n", &saved.revision).unwrap();
        assert!(
            store
                .apply_review_mode(
                    b"version: 1\n",
                    &saved.revision,
                    &removal.fingerprint,
                    ApplyMode::NextStart,
                    &provider
                )
                .await
                .is_err()
        );
        assert_eq!(store.read().unwrap().unwrap().bytes, next_yaml);
        let calls = std::fs::read_to_string(calls).unwrap();
        assert!(calls.lines().any(|verb| verb == "daemon-reload"));
        assert!(
            calls
                .lines()
                .all(|verb| matches!(verb, "show" | "daemon-reload")),
            "{calls}"
        );
    }

    #[tokio::test]
    async fn lifecycle_requires_loaded_ownership_before_invoking_systemctl() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("units");
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        let config = crate::core::declarative::parse(
            b"version: 1\nservices:\n  demo: {kind: container, containers: {app: {image: alpine}}}\n",
        ).unwrap();
        let fingerprint =
            crate::core::declarative::plan::service_fingerprint(&config, "demo").unwrap();
        let files =
            super::super::render_quadlet(&config, "demo", temp.path(), &fingerprint).unwrap();
        for (name, content) in &files {
            let path = directory.join(name);
            std::fs::write(&path, content).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let script = temp.path().join("systemctl");
        let calls = temp.path().join("calls");
        let mut content = String::from("#!/bin/sh\nif [ \"$4\" = show ]; then\ncase \"$7\" in\n");
        for name in files.keys() {
            let unit = generated_service(name).unwrap();
            content.push_str(&format!("{unit}) printf '%s\\n' 'LoadState=loaded' 'ActiveState=inactive' 'SourcePath={}' 'ExecStart=--label=io.maestro.fingerprint={fingerprint}' ;;\n", directory.join(name).display()));
        }
        content.push_str(&format!(
            "*) exit 1 ;;\nesac\nelse\nprintf '%s\\n' \"$4\" > '{}'\nfi\n",
            calls.display()
        ));
        std::fs::write(&script, &content).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let podman = temp.path().join("podman");
        std::fs::write(
            &podman,
            "#!/bin/sh\nif [ \"$2\" = ps ]; then printf '[]\\n'; else exit 1; fi\n",
        )
        .unwrap();
        std::fs::set_permissions(&podman, std::fs::Permissions::from_mode(0o700)).unwrap();
        let provider = QuadletProvider {
            artifacts: QuadletArtifacts::new(directory, temp.path().to_path_buf()).unwrap(),
            runner: CommandRunner::new(false),
            systemctl: script.clone(),
            podman,
        };
        assert_eq!(provider.status(&config, "demo").await.unwrap(), "stopped");
        let journal = temp.path().join("journalctl");
        std::fs::write(&journal, "#!/bin/sh\n[ \"$1\" = --user ] && [ \"$6\" = 10 ] || exit 1\nprintf '%s\\n' 'recent log'\n").unwrap();
        std::fs::set_permissions(&journal, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            provider.logs(&config, "demo", &journal, 10).await.unwrap(),
            "recent log\n"
        );
        provider.action(&config, "demo", "start").await.unwrap();
        assert_eq!(std::fs::read_to_string(&calls).unwrap(), "start\n");
        std::fs::remove_file(&calls).unwrap();
        std::fs::write(&script, content.replace(&fingerprint, &"0".repeat(64))).unwrap();
        assert!(provider.action(&config, "demo", "stop").await.is_err());
        assert!(!calls.exists());
    }
}
