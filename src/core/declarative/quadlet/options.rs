//! Typed Podman options shared with the Compose model, without arbitrary argv.
use std::fmt::Write;

use anyhow::{Result, ensure};

use super::{Container, word};

pub(super) fn render(body: &mut String, container: &Container) -> Result<()> {
    let mut no_new_privileges = true;
    for option in &container.security_opt {
        match option.as_str() {
            "no-new-privileges" | "no-new-privileges:true" => no_new_privileges = true,
            "no-new-privileges:false" => no_new_privileges = false,
            _ => {}
        }
    }
    writeln!(body, "NoNewPrivileges={no_new_privileges}")?;
    for (flag, value) in [
        ("user", &container.user),
        ("userns", &container.userns_mode),
        ("pull", &container.pull_policy),
        ("log-driver", &container.log_driver),
    ] {
        if !value.is_empty() {
            argument(body, flag, value)?;
        }
    }
    for (flag, values) in [
        ("uidmap", &container.uid_maps),
        ("gidmap", &container.gid_maps),
        ("group-add", &container.group_add),
        ("device", &container.devices),
        ("cap-add", &container.cap_add),
        ("cap-drop", &container.cap_drop),
        ("tmpfs", &container.tmpfs),
    ] {
        for value in values {
            argument(body, flag, value)?;
        }
    }
    for option in &container.security_opt {
        if !option.starts_with("no-new-privileges") {
            argument(body, "security-opt", option)?;
        }
    }
    if container.privileged {
        writeln!(body, "PodmanArgs=--privileged")?;
    }
    for (key, value) in &container.log_options {
        argument(body, "log-opt", &format!("{key}={value}"))?;
    }
    if !container.stop_grace_period.is_empty() {
        let seconds = container
            .stop_grace_period
            .strip_suffix('s')
            .and_then(|value| value.parse::<u32>().ok());
        ensure!(
            seconds.is_some_and(|value| value > 0),
            "Quadlet stop grace period must be whole seconds"
        );
        writeln!(body, "StopTimeout={}", seconds.unwrap())?;
    }
    if let Some(health) = &container.healthcheck {
        // Podman accepts the Compose CMD/CMD-SHELL JSON array verbatim.
        // Escape dollars for systemd, leaving container-side expansion intact.
        let command = serde_json::to_string(&health.test)?;
        // HealthCmd is a scalar, unlike the argument list in PodmanArgs.
        writeln!(
            body,
            "HealthCmd={}",
            command.replace('%', "%%").replace('$', "$$")
        )?;
        for (key, value) in [
            ("HealthInterval", &health.interval),
            ("HealthTimeout", &health.timeout),
            ("HealthStartPeriod", &health.start_period),
        ] {
            if !value.is_empty() {
                writeln!(body, "{key}={value}")?;
            }
        }
        if health.retries > 0 {
            writeln!(body, "HealthRetries={}", health.retries)?;
        }
    }
    Ok(())
}

fn argument(body: &mut String, flag: &str, value: &str) -> Result<()> {
    writeln!(
        body,
        "PodmanArgs={}",
        word(&format!("--{flag}={}", value.replace('$', "$$")))?
    )?;
    Ok(())
}
