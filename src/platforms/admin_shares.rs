//! Linux-only root helper Samba password operation.
//! Target account derives from the verified sudo caller UID, not request data.
use std::{
    ffi::CStr,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use tokio::{io::AsyncWriteExt, process::Command, time::timeout};

use super::admin_packages::trusted_executable;

const PASSWORD_TIMEOUT: Duration = Duration::from_secs(30);

pub fn validate_password(password: &str) -> Result<()> {
    ensure!(
        (1..=256).contains(&password.len())
            && !password.trim().is_empty()
            && password.chars().all(|ch| ch != '\0' && ch != '\r' && ch != '\n'),
        "Samba password must be 1–256 nonblank characters without line breaks or NUL"
    );
    Ok(())
}

fn caller_name(uid: u32) -> Result<String> {
    ensure!(uid > 0, "Samba password requires non-root workload caller");
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    let mut buffer = vec![0_i8; 16384];
    let code = unsafe {
        libc::getpwuid_r(
            uid as libc::uid_t, &mut pwd,
            buffer.as_mut_ptr(), buffer.len(), &mut found,
        )
    };
    ensure!(code == 0 && !found.is_null(), "Trusted workload user cannot be resolved");
    let name = unsafe { CStr::from_ptr(pwd.pw_name) }.to_str()
        .context("Invalid trusted workload account name")?.to_owned();
    ensure!(
        !name.is_empty() && name.len() <= 32
            && name.bytes().next().is_some_and(|c| c.is_ascii_lowercase() || c == b'_')
            && name.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_-".contains(&c))
            && !name.contains(' '),
        "Unsupported workload username"
    );
    Ok(name)
}

fn fixed_binary() -> Result<PathBuf> {
    for path in ["/usr/bin/smbpasswd", "/usr/sbin/smbpasswd"] {
        if Path::new(path).exists() {
            return trusted_executable(Path::new(path));
        }
    }
    anyhow::bail!("Samba password tool is not installed")
}

pub async fn set_password(caller_uid: u32, password: &str) -> Result<()> {
    validate_password(password)?;
    let username = caller_name(caller_uid)?;
    let program = fixed_binary()?;
    let mut child = Command::new(program)
        .args(["-s", "-a", username.as_str()])
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env("HOME", "/root")
        .current_dir("/")
        .stdin(Stdio::piped())
        // No native output can echo or leak the password into UI/MCP/audit.
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("Could not launch bounded Samba password operation")?;
    let result = timeout(PASSWORD_TIMEOUT, async {
        let mut input = child.stdin.take().context("Missing native password stdin")?;
        input.write_all(password.as_bytes()).await?;
        input.write_all(b"\n").await?;
        input.write_all(password.as_bytes()).await?;
        input.write_all(b"\n").await?;
        drop(input);
        let status = child.wait().await?;
        ensure!(status.success(), "Samba password tool rejected the update");
        Ok::<(), anyhow::Error>(())
    }).await.context("Samba password operation exceeded its deadline")??;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reject_samba_password_shell_or_linebreak_injection() {
        for invalid in ["", " ", "\n", "bad\nnext", "bad\rnext", "bad\0next"] {
            assert!(validate_password(invalid).is_err());
        }
        assert!(validate_password(&"x".repeat(257)).is_err());
        assert!(validate_password("Safe value ; /tmp/quoted").is_ok());
    }

    #[test]
    fn nonroot_caller_identity_is_mandatory() {
        assert!(caller_name(0).is_err());
    }
}
