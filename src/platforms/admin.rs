//! Linux-only, one-shot privilege boundary. Never available in ASUSWRT builds.
//! The workload server reads nonsecret grants; the helper independently checks root policy.
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
const POLICY_VERSION: u32 = 1;
const MAX_REQUEST: usize = reference::MAX_REQUEST;
const MAX_REPLY: usize = 16384;
const MAX_WIRE_REPLY: usize = 4 * 1024 * 1024;

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
    #[serde(default)]
    share_writes: bool,
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
    pub share_writes: bool,
}

impl Probe {
    fn ready(units: Vec<String>, package_writes: bool, samba_password: bool, share_writes: bool) -> Self {
        Self {
            status: "ready",
            code: "ready".into(),
            reason: "The authorized privileged helper is available.".into(),
            system_units: units,
            package_writes,
            samba_password,
            share_writes,
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
            share_writes: false,
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

    pub fn permits_shares(&self) -> bool {
        self.status == "ready" && self.share_writes
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
        Ok(metadata) if metadata.mode() & 0o037 == 0 && metadata.len() <= MAX_REPLY as u64 => {}
        _ => {
            return Probe::unavailable(
                "helper_policy_unsafe",
                "The helper policy must be root-owned, bounded and not writable by non-root; workload-group read access is permitted.",
            );
        }
    }
    if !Path::new(SUDO_PATH).is_file() {
        return Probe::unavailable(
            "sudo_missing",
            "Noninteractive sudo is not installed at the supported path.",
        );
    }
    Probe::ready(vec![], false, false, false)
}

/// All calls use the reference envelope, with no private wire operation.
async fn call(operation: &str, arguments: serde_json::Value, deadline: Duration) -> Result<reference::Response> {
    let data = serde_json::to_vec(&reference::Request { operation: operation.into(), arguments })?;
    ensure!(data.len() <= MAX_REQUEST, "helper request is too large");
    reference::decode(&data)?;
    let mut child = Command::new(SUDO_PATH)
        .args(["-n", "-u", "root", "--", HELPER_PATH])
        .env_clear().env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null())
        .kill_on_drop(true).spawn().context("noninteractive sudo is unavailable")?;
    let stdout = child.stdout.take().context("privileged helper has no stdout")?;
    let mut output = Vec::with_capacity(1024);
    timeout(deadline, async {
        if let Some(mut stdin) = child.stdin.take() { stdin.write_all(&data).await?; }
        stdout.take((MAX_WIRE_REPLY + 1) as u64).read_to_end(&mut output).await?;
        ensure!(output.len() <= MAX_WIRE_REPLY, "privileged helper reply is too large");
        ensure!(child.wait().await?.success(), "privileged helper is unauthorized, busy or unavailable");
        Ok::<(), anyhow::Error>(())
    }).await.context("privileged helper timed out")??;
    let response: reference::Response = serde_json::from_slice(&output)
        .context("unrecognized privileged helper response")?;
    ensure!(response.result.is_some() != !response.error.is_empty(), "privileged helper protocol mismatch");
    Ok(response)
}

fn checked(response: reference::Response) -> Result<reference::Response> {
    ensure!(response.error.is_empty(), "{} ({})",response.error,response.code);
    Ok(response)
}

fn safe_system_unit(unit: &str) -> bool {
    crate::platforms::systemd::valid_unit(unit)
        && unit != "maestro.service"
        && unit != "router-hub.service"
        && !crate::core::declarative::managed_unit(unit)
}

pub async fn probe() -> Probe {
    let uid = unsafe { libc::geteuid() };
    if uid == 0 {
        return Probe::unavailable("main_running_as_root","Linux Maestro must run as the non-root workload owner.");
    }
    let installed = helper_installation();
    if installed.status != "ready" { return installed; }
    let policy = match load_policy(uid) {
        Ok(policy) => policy,
        Err(_) => return Probe::unavailable("helper_policy_unreadable","The workload account cannot read its root-owned helper grants; review the policy owner, group and mode."),
    };
    // Inert authorization discovery, outside the helper wire protocol.
    let mut command = Command::new(SUDO_PATH);
    command.args(["-n","-l","-u","root","--",HELPER_PATH])
        .env_clear().env("PATH","/usr/sbin:/usr/bin:/sbin:/bin")
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true);
    if !timeout(Duration::from_secs(6),command.status()).await.is_ok_and(|result| result.is_ok_and(|status| status.success())) {
        return Probe::unavailable("helper_unauthorized","Review noninteractive sudo authorization for the exact helper command.");
    }
    Probe::ready(policy.system_units,policy.package_writes,policy.samba_password,policy.share_writes)
}

pub async fn system_service(unit: &str, action: &str) -> Result<String> {
    ensure!(safe_system_unit(unit), "invalid or reserved system service");
    ensure!(matches!(action,"start"|"stop"|"restart"|"reload"|"enable"|"disable"),"unsupported service action");
    let capability = probe().await;
    ensure!(capability.permits_unit(unit),"{} ({})",capability.reason,capability.code);
    let mut arguments = serde_json::json!({"scope":"system","unit":unit});
    if !matches!(action,"start"|"enable") { arguments["confirm"] = serde_json::json!(true); }
    Ok(checked(call(&format!("services_{action}"),arguments,Duration::from_secs(25*60+40)).await?)?.output)
}

pub async fn package_operation(operation: &str, names: Vec<String>, target: Option<String>) -> Result<String> {
    let (operation, arguments) = match operation {
        "package_refresh" if names.is_empty() && target.is_none() => ("updates_refresh",serde_json::json!({})),
        "package_upgrade" if names.is_empty() && target.is_none() => ("updates_install",serde_json::json!({"confirm":true})),
        "package_install" if target.is_none() => {
            let names = names.join(" ");
            admin_packages::package_names(&names)?;
            ("packages_install",serde_json::json!({"packages":names,"confirm":true}))
        },
        "dependency_install" if names.is_empty() => {
            let target = target.context("missing dependency group")?;
            ensure!(matches!(target.as_str(),"podman"|"docker"|"samba"|"nfs"|"virtualization"|"incus"),"dependency group must be allowlisted");
            ("dependencies_install",serde_json::json!({"target":target,"confirm":true}))
        },
        _ => anyhow::bail!("invalid package operation arguments"),
    };
    let capability = probe().await;
    ensure!(capability.permits_packages(),"{} ({})",capability.reason,capability.code);
    Ok(checked(call(operation,arguments,Duration::from_secs(25*60+40)).await?)?.output)
}

/// Secret-bearing operation never logs the request or retains a job result.
pub async fn samba_password(password: String) -> Result<()> {
    admin_shares::validate_password(&password)?;
    ensure!(probe().await.permits_samba_password(),"Samba password changes require an authorized optional helper and root-owned policy");
    checked(call("shares_samba_password",serde_json::json!({"password":password,"confirm":true}),Duration::from_secs(45)).await?)?;
    Ok(())
}

pub(crate) fn workload_account() -> Result<String> {
    admin_shares::caller_name(unsafe { libc::geteuid() })
}

pub(crate) async fn share_operation(operation: &str, arguments: serde_json::Value) -> Result<(serde_json::Value,String)> {
    ensure!(matches!(operation,"shares_list"|"shares_definitions_inspect"|"shares_definitions_validate"|"shares_definitions_apply"|"shares_setup"),
        "unsupported share operation");
    let capability = probe().await;
    ensure!(capability.status == "ready","{} ({})",capability.reason,capability.code);
    if matches!(operation,"shares_definitions_apply"|"shares_setup") {
        ensure!(capability.permits_shares(),"share writes are disabled by root-owned policy");
    }
    if operation == "shares_setup" { ensure!(capability.permits_packages(),"package writes are disabled by root-owned policy"); }
    let response = checked(call(operation,arguments,Duration::from_secs(25*60+40)).await?)?;
    Ok((response.result.context("missing helper result")?,response.output))
}

fn load_policy(caller_uid: u32) -> Result<Policy> {
    for path in ["/", "/etc", "/etc/maestro"] {
        trusted_component(Path::new(path), true)?;
    }
    let metadata = trusted_component(Path::new(POLICY_PATH), false)?;
    ensure!(
        metadata.mode() & 0o037 == 0 && metadata.len() <= MAX_REPLY as u64,
        "privileged policy must be bounded and not writable by non-root"
    );
    let file = fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK).open(POLICY_PATH)?;
    let opened = file.metadata()?;
    ensure!(opened.is_file() && opened.uid() == 0 && opened.mode() & 0o037 == 0 && opened.len() <= MAX_REPLY as u64,
        "unsafe privileged policy");
    let mut content = Vec::new();
    file.take((MAX_REPLY+1) as u64).read_to_end(&mut content)?;
    ensure!(content.len() <= MAX_REPLY, "privileged policy is too large");
    let policy: Policy = serde_json::from_slice(&content)?;
    ensure!(
        policy.version == POLICY_VERSION,
        "privileged policy version mismatch"
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
        "another privileged operation is running"
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
    // Reference helper serializes requests before decoding the bounded input.
    let _lock = lock_root_operation()?;
    let mut input = Vec::new();
    unsafe { libc::alarm(25 * 60); }
    std::io::stdin().take((MAX_REQUEST + 1) as u64).read_to_end(&mut input)?;
    unsafe { libc::alarm(0); }
    let request = reference::decode(&input)?;
    let (cancel, receiver) = tokio::sync::watch::channel(false);
    // A share apply may have published its candidate before activation fails.
    // Keep the lock and await its bounded rollback on interruption or timeout.
    let share_apply = request.operation == "shares_definitions_apply";
    let response = if share_apply {
        use tokio::signal::unix::{signal, SignalKind};
        let mut terminate = signal(SignalKind::terminate())?;
        let mut interrupt = signal(SignalKind::interrupt())?;
        let operation = reference::execute(request, &policy, caller_uid, receiver);
        tokio::pin!(operation);
        tokio::select! {
            response = &mut operation => response,
            _ = async {
                tokio::select! {
                    _ = terminate.recv() => {},
                    _ = interrupt.recv() => {},
                    _ = tokio::time::sleep(Duration::from_secs(25 * 60)) => {},
                }
            } => {
                cancel.send_replace(true);
                operation.await
            }
        }
    } else {
        timeout(Duration::from_secs(25 * 60), reference::execute(request, &policy, caller_uid, receiver))
            .await.context("context deadline exceeded")?
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
    fn only_reference_operations_are_accepted() {
        for input in [
            r#"{"version":1,"operation":"probe"}"#,
            r#"{"operation":"probe","arguments":{}}"#,
            r#"{"operation":"system_service","arguments":{}}"#,
            r#"{"operation":"shell","arguments":{}}"#,
            r#"{"operation":"services_start","arguments":{"scope":"user","unit":"sshd.service"}}"#,
        ] {
            assert!(reference::decode(input.as_bytes()).is_err(),"{input}");
        }
        let request = reference::decode(br#"{"operation":"services_restart","arguments":{"unit":"sshd.service","scope":"system","confirm":true}}"#).unwrap();
        assert_eq!(request.operation,"services_restart");
    }

    #[test]
    fn capability_policy_stays_fail_closed() {
        assert!(!safe_system_unit("maestro.service"));
        assert!(!safe_system_unit("maestro-example-app-0123456789abcdef.service"));
        let missing = Probe::unavailable("helper_missing","missing");
        assert!(!missing.permits_unit("sshd.service"));
        assert!(!missing.permits_packages());
        assert!(!missing.permits_samba_password());
        assert!(!missing.permits_shares());
        let authorized = Probe::ready(vec!["sshd.service".into()],true,false,true);
        assert!(authorized.permits_unit("sshd.service"));
        assert!(!authorized.permits_unit("nginx.service"));
        assert!(authorized.permits_packages());
        assert!(!authorized.permits_samba_password());
        assert!(authorized.permits_shares());
    }
}
