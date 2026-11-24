//! Linux-only, one-shot privilege boundary. Never available in ASUSWRT builds.
//! The workload server does not read the root policy or accept a helper path from clients.
use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::Path,
    process::Stdio,
    time::Duration,
};

use super::{admin_packages, admin_shares};
#[path = "admin_reference.rs"]
mod reference;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
    time::timeout,
};

pub const HELPER_PATH: &str = "/usr/local/libexec/maestro-admin";
const POLICY_PATH: &str = "/etc/maestro/admin-policy.json";
const SUDO_PATH: &str = "/usr/bin/sudo";
const PROTOCOL: u32 = 1;
const MAX_REQUEST: usize = reference::MAX_REQUEST;
const MAX_REPLY: usize = 16384;
const MAX_WIRE_REPLY: usize = 4 * 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    version: u32,
    operation: String,
    #[serde(default)]
    unit: Option<String>,
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    confirm: Option<bool>,
    #[serde(default)]
    packages: Vec<String>,
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    password: Option<String>,
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
    #[serde(default)]
    package_writes: bool,
    #[serde(default)]
    samba_password: bool,
    #[serde(default)]
    output: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    version: u32,
    workload_uid: u32,
    system_units: Vec<String>,
    #[serde(default)]
    package_writes: bool,
    #[serde(default)]
    samba_password: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Probe {
    pub status: &'static str,
    pub code: String,
    pub reason: String,
    pub system_units: Vec<String>,
    pub package_writes: bool,
    pub samba_password: bool,
}

impl Probe {
    fn ready(units: Vec<String>, package_writes: bool, samba_password: bool) -> Self {
        Self {
            status: "ready",
            code: "ready".into(),
            reason: "The authorized privileged helper is available.".into(),
            system_units: units,
            package_writes,
            samba_password,
        }
    }

    fn unavailable(code: &str, reason: &str) -> Self {
        Self {
            status: "unavailable",
            code: code.into(),
            reason: reason.into(),
            system_units: vec![],
            package_writes: false,
            samba_password: false,
        }
    }

    pub fn permits_unit(&self, unit: &str) -> bool {
        self.status == "ready" && self.system_units.iter().any(|allowed| allowed == unit)
    }

    pub fn permits_packages(&self) -> bool {
        self.status == "ready" && self.package_writes
    }

    pub fn permits_samba_password(&self) -> bool {
        self.status == "ready" && self.samba_password
    }
}

// Reject symlinks, untrusted ancestors and any group/other-writable component.
// The fixed paths are intentionally not configurable from workload YAML.
fn trusted_component(path: &Path, directory: bool) -> Result<fs::Metadata> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("missing trusted path {}", path.display()))?;
    ensure!(metadata.uid() == 0, "privileged path is not root-owned");
    ensure!(
        metadata.mode() & 0o022 == 0,
        "privileged path is writable by non-root"
    );
    ensure!(
        if directory {
            metadata.is_dir()
        } else {
            metadata.is_file()
        },
        "invalid privileged path type"
    );
    Ok(metadata)
}

fn helper_installation() -> Probe {
    if !Path::new(HELPER_PATH).exists() {
        return Probe::unavailable(
            "helper_missing",
            "Install the optional maestro-admin helper to enable protected host writes.",
        );
    }
    for path in ["/", "/usr", "/usr/local", "/usr/local/libexec"] {
        if trusted_component(Path::new(path), true).is_err() {
            return Probe::unavailable(
                "helper_unsafe",
                "The helper has an unsafe parent directory or ownership.",
            );
        }
    }
    match trusted_component(Path::new(HELPER_PATH), false) {
        Ok(metadata) if metadata.mode() & 0o111 != 0 && metadata.mode() & 0o6000 == 0 => {}
        _ => {
            return Probe::unavailable(
                "helper_unsafe",
                "The helper must be an executable, root-owned regular file without untrusted write access.",
            );
        }
    }
    if !Path::new(POLICY_PATH).exists() {
        return Probe::unavailable(
            "helper_policy_missing",
            "Install an explicit root-owned private helper policy for the workload account.",
        );
    }
    for path in ["/etc", "/etc/maestro"] {
        if trusted_component(Path::new(path), true).is_err() {
            return Probe::unavailable(
                "helper_policy_unsafe",
                "The privileged policy has an untrusted parent directory.",
            );
        }
    }
    match trusted_component(Path::new(POLICY_PATH), false) {
        Ok(metadata) if metadata.mode() & 0o077 == 0 && metadata.len() <= MAX_REPLY as u64 => {}
        _ => {
            return Probe::unavailable(
                "helper_policy_unsafe",
                "The helper policy must be a root-owned mode 0600 regular file with bounded size.",
            );
        }
    }
    if !Path::new(SUDO_PATH).is_file() {
        return Probe::unavailable(
            "sudo_missing",
            "Noninteractive sudo is not installed at the supported path.",
        );
    }
    Probe::ready(vec![], false, false)
}

async fn call(request: &Request, deadline: Duration) -> Result<Reply> {
    validate_request(request)?;
    // Only the inert Maestro capability probe retains its private extension.
    // All management requests use the exact reference operation/arguments envelope.
    let data = if request.operation == "probe" {
        serde_json::to_vec(request)?
    } else {
        let (operation, arguments) = match request.operation.as_str() {
            "system_service" => {
                let action = request.action.as_deref().context("missing action")?;
                let mut arguments = serde_json::json!({"scope":"system","unit":request.unit});
                if let Some(confirm) = request.confirm { arguments["confirm"] = serde_json::json!(confirm); }
                (format!("services_{action}"), arguments)
            }
            "package_refresh" => ("updates_refresh".into(), serde_json::json!({})),
            "package_upgrade" => ("updates_install".into(), serde_json::json!({"confirm":true})),
            "package_install" => ("packages_install".into(), serde_json::json!({"packages":request.packages.join(" "),"confirm":true})),
            "dependency_install" => ("dependencies_install".into(), serde_json::json!({"target":request.target,"confirm":true})),
            "samba_password" => ("shares_samba_password".into(), serde_json::json!({"password":request.password,"confirm":true})),
            _ => anyhow::bail!("unknown privileged operation"),
        };
        serde_json::to_vec(&reference::Request { operation, arguments })?
    };
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
    let stdout = child
        .stdout
        .take()
        .context("privileged helper has no stdout")?;
    let limit = if request.operation == "probe" { MAX_REPLY } else { MAX_WIRE_REPLY };
    let mut output = Vec::with_capacity(1024);
    timeout(deadline, async {
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(&data).await?;
        }
        // Reading only MAX_REPLY+1 avoids an unbounded allocation even if the
        // installed executable is corrupt or has an incompatible protocol.
        stdout
            .take((limit + 1) as u64)
            .read_to_end(&mut output)
            .await?;
        ensure!(
            output.len() <= limit,
            "privileged helper reply is too large"
        );
        let status = child.wait().await?;
        ensure!(
            status.success(),
            "privileged helper is unauthorized or unavailable"
        );
        Ok::<(), anyhow::Error>(())
    })
    .await
    .context("privileged helper timed out")??;
    if request.operation != "probe" {
        let response: reference::Response = serde_json::from_slice(&output)
            .context("unrecognized privileged helper response")?;
        ensure!(response.result.is_some() || !response.error.is_empty(), "privileged helper protocol mismatch");
        return Ok(Reply {
            version: PROTOCOL, ok: response.error.is_empty(),
            code: if response.error.is_empty() { "ok".into() } else { response.code },
            message: if response.error.is_empty() { "Privileged operation completed.".into() } else { response.error },
            system_units: vec![], package_writes: false, samba_password: false,
            output: response.output,
        });
    }
    let reply: Reply =
        serde_json::from_slice(&output).context("unrecognized privileged helper response")?;
    ensure!(
        reply.version == PROTOCOL,
        "privileged helper protocol mismatch"
    );
    ensure!(
        reply.code.len() <= 64
            && reply.message.len() <= 512
            && reply.system_units.len() <= 128
            && reply.output.len() <= 8192,
        "invalid privileged helper reply"
    );
    Ok(reply)
}

fn validate_request(request: &Request) -> Result<()> {
    ensure!(request.version == PROTOCOL, "unsupported helper protocol");
    match request.operation.as_str() {
        "probe" => ensure!(
            request.unit.is_none()
                && request.action.is_none()
                && request.confirm.is_none()
                && request.packages.is_empty()
                && request.target.is_none()
                && request.password.is_none(),
            "probe accepts no arguments"
        ),
        "system_service" => {
            let unit = request.unit.as_deref().context("missing unit")?;
            let action = request.action.as_deref().context("missing action")?;
            ensure!(
                request.packages.is_empty()
                    && request.target.is_none()
                    && request.password.is_none(),
                "system service accepts no package or password arguments"
            );
            ensure!(safe_system_unit(unit), "invalid or reserved system service");
            ensure!(
                matches!(
                    action,
                    "start" | "stop" | "restart" | "reload" | "enable" | "disable"
                ),
                "unsupported service action"
            );
            ensure!(
                matches!(action, "start" | "enable") || request.confirm == Some(true),
                "destructive system service action requires confirmation"
            );
        }
        "package_refresh" | "package_upgrade" | "package_install" | "dependency_install" => {
            ensure!(
                request.unit.is_none() && request.action.is_none() && request.password.is_none(),
                "package action has service or password arguments"
            );
            match request.operation.as_str() {
                "package_refresh" => ensure!(
                    request.packages.is_empty()
                        && request.target.is_none()
                        && request.confirm.is_none(),
                    "metadata refresh accepts no arguments"
                ),
                "package_upgrade" => ensure!(
                    request.packages.is_empty()
                        && request.target.is_none()
                        && request.confirm == Some(true),
                    "package upgrade requires confirmation"
                ),
                "package_install" => {
                    ensure!(
                        request.target.is_none() && request.confirm == Some(true),
                        "package installation requires confirmation"
                    );
                    admin_packages::package_names(&request.packages.join(" "))?;
                }
                "dependency_install" => ensure!(
                    request.packages.is_empty()
                        && request.confirm == Some(true)
                        && matches!(
                            request.target.as_deref(),
                            Some(
                                "podman" | "docker" | "samba" | "nfs" | "virtualization" | "incus"
                            )
                        ),
                    "dependency group must be allowlisted and confirmed"
                ),
                _ => unreachable!(),
            }
        }
        "samba_password" => {
            ensure!(
                request.unit.is_none()
                    && request.action.is_none()
                    && request.packages.is_empty()
                    && request.target.is_none()
                    && request.confirm == Some(true),
                "Samba password requires fixed caller identity and confirmation"
            );
            admin_shares::validate_password(
                request
                    .password
                    .as_deref()
                    .context("missing Samba password")?,
            )?;
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
        return Probe::unavailable(
            "main_running_as_root",
            "Linux Maestro must run as the non-root workload owner.",
        );
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
        confirm: None,
        packages: vec![],
        target: None,
        password: None,
    };
    match call(&request, Duration::from_secs(6)).await {
        Ok(reply) if reply.ok => Probe::ready(
            reply.system_units,
            reply.package_writes,
            reply.samba_password,
        ),
        Ok(reply) => Probe::unavailable(&reply.code, &reply.message),
        Err(error) if error.to_string().contains("protocol mismatch") => Probe::unavailable(
            "helper_protocol_mismatch",
            "The installed helper must be upgraded to the matching Maestro protocol.",
        ),
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
        confirm: (!matches!(action, "start" | "enable")).then_some(true),
        packages: vec![],
        target: None,
        password: None,
    };
    validate_request(&request)?;
    let capability = probe().await;
    ensure!(
        capability.permits_unit(unit),
        "{} ({})",
        capability.reason,
        capability.code
    );
    // Revalidation is deliberately repeated inside the root helper.
    let reply = call(&request, Duration::from_secs(25 * 60)).await?;
    ensure!(reply.ok, "{} ({})", reply.message, reply.code);
    Ok(reply.output)
}

/// The main server never invokes a package manager directly. Only a validated
/// helper can perform protected writes, and its root-owned policy is reloaded
/// before every request. Package-manager selection is based on trusted OS data.
pub async fn package_operation(
    operation: &str,
    names: Vec<String>,
    target: Option<String>,
) -> Result<String> {
    let request = Request {
        version: PROTOCOL,
        operation: operation.to_owned(),
        unit: None,
        action: None,
        confirm: (operation != "package_refresh").then_some(true),
        packages: names,
        target,
        password: None,
    };
    validate_request(&request)?;
    let capability = probe().await;
    ensure!(
        capability.permits_packages(),
        "{} ({})",
        capability.reason,
        capability.code
    );
    let reply = call(&request, Duration::from_secs(25 * 60)).await?;
    ensure!(reply.ok, "{} ({})", reply.message, reply.code);
    Ok(reply.output)
}

/// Secret-bearing operation is never logged, placed in argv or retained in a
/// management job history. Only the sudo-resolved workload identity is targeted.
pub async fn samba_password(password: String) -> Result<()> {
    let request = Request {
        version: PROTOCOL,
        operation: "samba_password".into(),
        unit: None,
        action: None,
        confirm: Some(true),
        packages: vec![],
        target: None,
        password: Some(password),
    };
    validate_request(&request)?;
    ensure!(
        probe().await.permits_samba_password(),
        "Samba password changes require an authorized optional helper and root-owned policy"
    );
    let reply = call(&request, Duration::from_secs(45)).await?;
    ensure!(reply.ok, "{} ({})", reply.message, reply.code);
    Ok(())
}

fn load_policy(caller_uid: u32) -> Result<Policy> {
    for path in ["/", "/etc", "/etc/maestro"] {
        trusted_component(Path::new(path), true)?;
    }
    let metadata = trusted_component(Path::new(POLICY_PATH), false)?;
    ensure!(
        metadata.mode() & 0o077 == 0 && metadata.len() <= MAX_REPLY as u64,
        "privileged policy must be private and bounded"
    );
    let content = fs::read(POLICY_PATH)?;
    ensure!(content.len() <= MAX_REPLY, "privileged policy is too large");
    let policy: Policy = serde_json::from_slice(&content)?;
    ensure!(
        policy.version == PROTOCOL,
        "privileged policy protocol mismatch"
    );
    ensure!(
        caller_uid != 0 && policy.workload_uid == caller_uid,
        "caller is not the configured workload account"
    );
    ensure!(
        policy.system_units.len() <= 128,
        "too many allowed system units"
    );
    let mut units = BTreeSet::new();
    for unit in &policy.system_units {
        ensure!(
            safe_system_unit(unit) && units.insert(unit),
            "invalid or duplicate policy unit"
        );
    }
    Ok(policy)
}

fn lock_root_operation() -> Result<fs::File> {
    trusted_component(Path::new("/run"), true)?;
    fs::create_dir_all("/run/maestro-admin")?;
    let directory = trusted_component(Path::new("/run/maestro-admin"), true)?;
    ensure!(
        directory.mode() & 0o077 == 0,
        "unsafe privileged runtime directory"
    );
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open("/run/maestro-admin/lock")?;
    let metadata = lock.metadata()?;
    ensure!(
        metadata.is_file() && metadata.uid() == 0 && metadata.mode() & 0o077 == 0,
        "unsafe privileged lock"
    );
    ensure!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
        "another privileged operation is active"
    );
    Ok(lock)
}

/// Invoked by the separate root-owned binary. No listener, argv commands or user-selected paths.
pub async fn helper_main() -> Result<()> {
    ensure!(unsafe { libc::geteuid() } == 0 && std::env::args_os().len() == 1,
        "helper requires root and no arguments");
    let caller_uid: u32 = std::env::var("SUDO_UID")
        .context("helper requires sudo caller identity")?
        .parse().context("invalid helper caller UID")?;
    let policy = load_policy(caller_uid)?;
    unsafe { libc::umask(0o077); }
    std::env::set_current_dir("/")?;
    // As in the reference helper, serialize native operations and bound a
    // stalled request. The probe is inert and does not take the native lock.
    let mut input = Vec::new();
    unsafe { libc::alarm(25 * 60); }
    std::io::stdin().take((MAX_REQUEST + 1) as u64).read_to_end(&mut input)?;
    unsafe { libc::alarm(0); }
    ensure!(input.len() <= MAX_REQUEST, "request exceeds 1 MiB");

    if let Ok(request) = serde_json::from_slice::<Request>(&input) {
        if request.operation == "probe" && validate_request(&request).is_ok() {
            let response = Reply {
                version: PROTOCOL, ok: true, code: "ready".into(),
                message: "Privileged helper authorized.".into(),
                system_units: policy.system_units, package_writes: policy.package_writes,
                samba_password: policy.samba_password, output: String::new(),
            };
            let mut stdout = std::io::stdout().lock();
            serde_json::to_writer(&mut stdout, &response)?;
            stdout.write_all(b"\n")?;
            return Ok(());
        }
    }
    let request = reference::decode(&input)?;
    let _lock = lock_root_operation()?;
    let response = timeout(Duration::from_secs(25 * 60), reference::execute(request, &policy, caller_uid))
        .await.context("context deadline exceeded")?;
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
            r#"{"version":1,"operation":"system_service","unit":"sshd.service","action":"stop"}"#,
            r#"{"version":1,"operation":"probe","unit":"sshd.service"}"#,
            r#"{"version":1,"operation":"package_install","packages":["../etc/passwd"],"confirm":true}"#,
            r#"{"version":1,"operation":"package_install","packages":["--overwrite"],"confirm":true}"#,
            r#"{"version":1,"operation":"package_upgrade"}"#,
            r#"{"version":1,"operation":"dependency_install","target":"shell","confirm":true}"#,
            r#"{"version":1,"operation":"samba_password","password":"bad\nnewline","confirm":true}"#,
            r#"{"version":1,"operation":"samba_password","password":"valid","unit":"root.service","confirm":true}"#,
            r#"{"version":1,"operation":"samba_password","password":"valid","target":"root","confirm":true}"#,
            r#"{"version":2,"operation":"probe"}"#,
        ] {
            let request: Request = serde_json::from_str(request).unwrap();
            assert!(validate_request(&request).is_err(), "{request:?}");
        }
        assert!(
            serde_json::from_str::<Request>(r#"{"version":1,"operation":"probe","extra":true}"#)
                .is_err()
        );
        assert!(
            serde_json::from_str::<Request>(r#"{"version":1,"operation":"probe"} {}"#).is_err()
        );
    }

    #[test]
    fn accepts_only_exact_owned_service_actions() {
        let request = Request {
            version: 1,
            operation: "system_service".into(),
            unit: Some("sshd.service".into()),
            action: Some("restart".into()),
            confirm: Some(true),
            packages: vec![],
            target: None,
            password: None,
        };
        assert!(validate_request(&request).is_ok());
        assert!(!safe_system_unit("maestro.service"));
        assert!(!safe_system_unit(
            "maestro-example-app-0123456789abcdef.service"
        ));
        assert!(!Probe::unavailable("helper_missing", "missing").permits_unit("sshd.service"));
        assert!(!Probe::unavailable("helper_missing", "missing").permits_packages());
        assert!(!Probe::unavailable("helper_missing", "missing").permits_samba_password());
    }
}
