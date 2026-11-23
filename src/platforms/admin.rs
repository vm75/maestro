//! Linux-only, one-shot privilege boundary. Never available in ASUSWRT builds.
//! The workload server does not read the root policy or accept a helper path from clients.
use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Write},
    os::{fd::AsRawFd, unix::fs::{MetadataExt, OpenOptionsExt}},
    path::Path,
    process::Stdio,
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use tokio::{io::AsyncWriteExt, process::Command, time::timeout};

pub const HELPER_PATH: &str = "/usr/local/libexec/maestro-admin";
const POLICY_PATH: &str = "/etc/maestro/admin-policy.json";
const SUDO_PATH: &str = "/usr/bin/sudo";
const SYSTEMCTL_PATH: &str = "/usr/bin/systemctl";
const PROTOCOL: u32 = 1;
const MAX_REQUEST: usize = 4096;
const MAX_REPLY: usize = 16384;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    version: u32,
    operation: String,
    #[serde(default)]
    unit: Option<String>,
    #[serde(default)]
    action: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    version: u32,
    ok: bool,
    code: String,
    message: String,
    #[serde(default)]
    system_units: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    version: u32,
    workload_uid: u32,
    system_units: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Probe {
    pub status: &'static str,
    pub code: String,
    pub reason: String,
    pub system_units: Vec<String>,
}

impl Probe {
    fn ready(units: Vec<String>) -> Self {
        Self {
            status: "ready",
            code: "ready".into(),
            reason: "The authorized privileged helper is available.".into(),
            system_units: units,
        }
    }

    fn unavailable(code: &str, reason: &str) -> Self {
        Self {
            status: "unavailable",
            code: code.into(),
            reason: reason.into(),
            system_units: vec![],
        }
    }

    pub fn permits_unit(&self, unit: &str) -> bool {
        self.status == "ready" && self.system_units.iter().any(|allowed| allowed == unit)
    }
}

// Reject symlinks, untrusted ancestors and any group/other-writable component.
// The fixed paths are intentionally not configurable from workload YAML/TOML.
fn trusted_component(path: &Path, directory: bool) -> Result<fs::Metadata> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("missing trusted path {}", path.display()))?;
    ensure!(metadata.uid() == 0, "privileged path is not root-owned");
    ensure!(metadata.mode() & 0o022 == 0, "privileged path is writable by non-root");
    ensure!(
        if directory { metadata.is_dir() } else { metadata.is_file() },
        "invalid privileged path type"
    );
    Ok(metadata)
}

fn helper_installation() -> Probe {
    if !Path::new(HELPER_PATH).exists() {
        return Probe::unavailable("helper_missing", "Install the optional maestro-admin helper to enable protected host writes.");
    }
    for path in ["/", "/usr", "/usr/local", "/usr/local/libexec"] {
        if trusted_component(Path::new(path), true).is_err() {
            return Probe::unavailable("helper_unsafe", "The helper has an unsafe parent directory or ownership.");
        }
    }
    match trusted_component(Path::new(HELPER_PATH), false) {
        Ok(metadata) if metadata.mode() & 0o111 != 0 => {}
        _ => return Probe::unavailable("helper_unsafe", "The helper must be an executable, root-owned regular file without untrusted write access."),
    }
    if !Path::new(SUDO_PATH).is_file() {
        return Probe::unavailable("sudo_missing", "Noninteractive sudo is not installed at the supported path.");
    }
    Probe::ready(vec![])
}

async fn call(request: &Request, deadline: Duration) -> Result<Reply> {
    validate_request(request)?;
    let data = serde_json::to_vec(request)?;
    ensure!(data.len() <= MAX_REQUEST, "helper request is too large");
    let mut child = Command::new(SUDO_PATH)
        .args(["-n", "--", HELPER_PATH])
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("noninteractive sudo is unavailable")?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(&data).await?;
    }
    let result = timeout(deadline, child.wait_with_output())
        .await
        .context("privileged helper timed out")??;
    ensure!(result.status.success(), "privileged helper is unauthorized or unavailable");
    ensure!(result.stdout.len() <= MAX_REPLY, "privileged helper reply is too large");
    let reply: Reply = serde_json::from_slice(&result.stdout)
        .context("unrecognized privileged helper response")?;
    ensure!(reply.version == PROTOCOL, "privileged helper protocol mismatch");
    ensure!(
        reply.code.len() <= 64 && reply.message.len() <= 512 && reply.system_units.len() <= 128,
        "invalid privileged helper reply"
    );
    Ok(reply)
}

fn validate_request(request: &Request) -> Result<()> {
    ensure!(request.version == PROTOCOL, "unsupported helper protocol");
    match request.operation.as_str() {
        "probe" => ensure!(
            request.unit.is_none() && request.action.is_none(),
            "probe accepts no arguments"
        ),
        "system_service" => {
            let unit = request.unit.as_deref().context("missing unit")?;
            let action = request.action.as_deref().context("missing action")?;
            ensure!(safe_system_unit(unit), "invalid or reserved system service");
            ensure!(
                matches!(action, "start" | "stop" | "restart" | "reload" | "enable" | "disable"),
                "unsupported service action"
            );
        }
        _ => anyhow::bail!("unknown privileged operation"),
    }
    Ok(())
}

fn safe_system_unit(unit: &str) -> bool {
    crate::platforms::systemd::valid_unit(unit)
        && unit != "maestro.service"
        && unit != "router-hub.service"
        && !crate::core::declarative::managed_unit(unit)
}

pub async fn probe() -> Probe {
    if unsafe { libc::geteuid() } == 0 {
        return Probe::unavailable("main_running_as_root", "Linux Maestro must run as the non-root workload owner.");
    }
    let installed = helper_installation();
    if installed.status != "ready" {
        return installed;
    }
    let request = Request {
        version: PROTOCOL,
        operation: "probe".into(),
        unit: None,
        action: None,
    };
    match call(&request, Duration::from_secs(6)).await {
        Ok(reply) if reply.ok => Probe::ready(reply.system_units),
        Ok(reply) => Probe::unavailable(&reply.code, &reply.message),
        Err(error) if error.to_string().contains("protocol mismatch") => {
            Probe::unavailable("helper_protocol_mismatch", "The installed helper must be upgraded to the matching Maestro protocol.")
        }
        Err(_) => Probe::unavailable(
            "helper_unauthorized",
            "The helper could not be authorized with sudo -n; review the exact sudoers rule, helper version and policy.",
        ),
    }
}

pub async fn system_service(unit: &str, action: &str) -> Result<String> {
    let request = Request {
        version: PROTOCOL,
        operation: "system_service".into(),
        unit: Some(unit.into()),
        action: Some(action.into()),
    };
    validate_request(&request)?;
    let capability = probe().await;
    ensure!(capability.permits_unit(unit), "{} ({})", capability.reason, capability.code);
    // Revalidation is deliberately repeated inside the root helper.
    let reply = call(&request, Duration::from_secs(130)).await?;
    ensure!(reply.ok, "{} ({})", reply.message, reply.code);
    Ok(reply.message)
}

fn load_policy(caller_uid: u32) -> Result<Policy> {
    for path in ["/", "/etc", "/etc/maestro"] {
        trusted_component(Path::new(path), true)?;
    }
    let metadata = trusted_component(Path::new(POLICY_PATH), false)?;
    ensure!(metadata.mode() & 0o077 == 0 && metadata.len() <= MAX_REPLY as u64, "privileged policy must be private and bounded");
    let content = fs::read(POLICY_PATH)?;
    ensure!(content.len() <= MAX_REPLY, "privileged policy is too large");
    let policy: Policy = serde_json::from_slice(&content)?;
    ensure!(policy.version == PROTOCOL, "privileged policy protocol mismatch");
    ensure!(caller_uid != 0 && policy.workload_uid == caller_uid, "caller is not the configured workload account");
    ensure!(policy.system_units.len() <= 128, "too many allowed system units");
    let mut units = BTreeSet::new();
    for unit in &policy.system_units {
        ensure!(safe_system_unit(unit) && units.insert(unit), "invalid or duplicate policy unit");
    }
    Ok(policy)
}

fn lock_root_operation() -> Result<fs::File> {
    fs::create_dir_all("/run/maestro-admin")?;
    let directory = trusted_component(Path::new("/run/maestro-admin"), true)?;
    ensure!(directory.mode() & 0o077 == 0, "unsafe privileged runtime directory");
    let lock = fs::OpenOptions::new()
        .read(true).write(true).create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open("/run/maestro-admin/lock")?;
    let metadata = lock.metadata()?;
    ensure!(metadata.is_file() && metadata.uid() == 0 && metadata.mode() & 0o077 == 0, "unsafe privileged lock");
    ensure!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0, "another privileged operation is active");
    Ok(lock)
}

fn root_command() -> Command {
    let mut cmd = Command::new(SYSTEMCTL_PATH);
    cmd.env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .current_dir("/")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    cmd
}

async fn root_service_action(policy: &Policy, unit: &str, action: &str) -> Result<()> {
    ensure!(policy.system_units.iter().any(|allowed| allowed == unit), "system service is outside root-owned policy");
    trusted_component(Path::new(SYSTEMCTL_PATH), false)?;
    let mut show = root_command();
    let identity = timeout(
        Duration::from_secs(12),
        show.args(["--system", "--no-pager", "--no-ask-password", "show", "--property=Id", "--value", "--", unit]).output(),
    )
    .await
    .context("systemd identity check timed out")??;
    ensure!(identity.status.success() && identity.stdout.len() <= 4096, "systemd identity unavailable");
    let canonical = std::str::from_utf8(&identity.stdout)?.trim();
    ensure!(canonical == unit && safe_system_unit(canonical), "systemd unit alias is outside policy");
    let mut command = root_command();
    let status = timeout(
        Duration::from_secs(120),
        command.args(["--system", "--no-pager", "--no-ask-password", action, "--", canonical]).status(),
    )
    .await
    .context("systemd operation timed out")??;
    ensure!(status.success(), "systemd operation failed; inspect the host journal");
    Ok(())
}

/// Invoked by the separate root-owned binary. No listener, argv commands or user-selected paths.
pub async fn helper_main() -> Result<()> {
    ensure!(unsafe { libc::geteuid() } == 0, "helper requires root");
    ensure!(std::env::args_os().len() == 1, "helper accepts no command arguments");
    let caller_uid: u32 = std::env::var("SUDO_UID")
        .context("helper requires sudo caller identity")?
        .parse()
        .context("invalid sudo caller identity")?;
    let policy = load_policy(caller_uid)?;
    unsafe { libc::umask(0o077); libc::alarm(10); }
    std::env::set_current_dir("/")?;
    let mut input = Vec::new();
    std::io::stdin().take((MAX_REQUEST + 1) as u64).read_to_end(&mut input)?;
    unsafe { libc::alarm(0); }
    ensure!(input.len() <= MAX_REQUEST, "helper request is too large");

    let response = match serde_json::from_slice::<Request>(&input) {
        Ok(request) if validate_request(&request).is_ok() => {
            if request.operation == "probe" {
                Reply {
                    version: PROTOCOL,
                    ok: true,
                    code: "ready".into(),
                    message: "Privileged helper authorized.".into(),
                    system_units: policy.system_units,
                }
            } else {
                let _lock = lock_root_operation()?;
                let result = root_service_action(
                    &policy,
                    request.unit.as_deref().unwrap_or_default(),
                    request.action.as_deref().unwrap_or_default(),
                ).await;
                Reply {
                    version: PROTOCOL,
                    ok: result.is_ok(),
                    code: if result.is_ok() { "ok" } else { "operation_denied" }.into(),
                    message: if result.is_ok() {
                        "System service action completed.".into()
                    } else {
                        // Never echo potentially private native command output or input.
                        "System service action failed, was denied, or requires host inspection.".into()
                    },
                    system_units: vec![],
                }
            }
        }
        _ => Reply {
            version: PROTOCOL,
            ok: false,
            code: "invalid_request".into(),
            message: "Unsupported or malformed privileged request.".into(),
            system_units: vec![],
        },
    };
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, &response)?;
    stdout.write_all(b"\n")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reject_unknown_and_unbounded_privileged_actions() {
        for request in [
            r#"{"version":1,"operation":"shell","unit":"/bin/sh"}"#,
            r#"{"version":1,"operation":"system_service","unit":"../maestro.service","action":"stop"}"#,
            r#"{"version":1,"operation":"system_service","unit":"maestro.service","action":"stop"}"#,
            r#"{"version":1,"operation":"system_service","unit":"sshd.service","action":"execute"}"#,
            r#"{"version":1,"operation":"probe","unit":"sshd.service"}"#,
            r#"{"version":2,"operation":"probe"}"#,
        ] {
            let request: Request = serde_json::from_str(request).unwrap();
            assert!(validate_request(&request).is_err(), "{request:?}");
        }
        assert!(serde_json::from_str::<Request>(r#"{"version":1,"operation":"probe","extra":true}"#).is_err());
        assert!(serde_json::from_str::<Request>(r#"{"version":1,"operation":"probe"} {}"#).is_err());
    }

    #[test]
    fn accepts_only_exact_owned_service_actions() {
        let request = Request {
            version: 1,
            operation: "system_service".into(),
            unit: Some("sshd.service".into()),
            action: Some("restart".into()),
        };
        assert!(validate_request(&request).is_ok());
        assert!(!safe_system_unit("maestro.service"));
        assert!(!safe_system_unit("maestro-example-app-0123456789abcdef.service"));
        assert!(!Probe::unavailable("helper_missing", "missing").permits_unit("sshd.service"));
    }
}
