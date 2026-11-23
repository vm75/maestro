use std::{ffi::OsString, path::Path, process::Stdio, time::Duration};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use anyhow::{Context, Result, bail};
use tokio::{process::Command, time::timeout};
use tracing::info;

use crate::models::CommandResult;

#[derive(Clone)]
pub struct CommandRunner {
    test_mode: bool,
}

#[cfg(feature = "linux")]
pub struct CapturedOutput {
    pub stdout: Vec<u8>,
    pub truncated: bool,
    pub status: std::process::ExitStatus,
    pub had_stderr: bool,
}

#[cfg(feature = "linux")]
impl CapturedOutput {
    fn require_success(self) -> Result<Self> {
        anyhow::ensure!(self.status.success(), "command exited unsuccessfully");
        Ok(self)
    }
}

#[cfg(unix)]
struct ProcessGroupGuard(Option<u32>);

#[cfg(unix)]
impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            // SAFETY: the child created this process group; negative PID targets
            // its descendants as well, including on future cancellation.
            if unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGKILL) } != 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    tracing::warn!(%pid, %error, "unable to kill cancelled command process group");
                }
            }
        }
    }
}

impl CommandRunner {
    pub fn new(test_mode: bool) -> Self {
        Self { test_mode }
    }

    #[cfg(feature = "linux")]
    pub fn is_test_mode(&self) -> bool {
        self.test_mode
    }

    pub async fn run<I, S>(
        &self,
        program: &Path,
        args: I,
        timeout_duration: Duration,
    ) -> Result<CommandResult>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
        if self.test_mode {
            let rendered = args
                .iter()
                .map(|arg| arg.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" ");
            info!(program = %program.display(), args = %rendered, "simulated command");
            return Ok(CommandResult {
                success: true,
                code: Some(0),
                stdout: format!("SIMULATED: {} {}", program.display(), rendered),
                stderr: String::new(),
                simulated: true,
            });
        }

        if !program.exists() {
            bail!("command not found: {}", program.display());
        }

        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let output = timeout(timeout_duration, command.output())
            .await
            .with_context(|| format!("command timed out: {}", program.display()))??;

        Ok(CommandResult {
            success: output.status.success(),
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).trim().to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            simulated: false,
        })
    }

    /// Capture only bounded output from a command that must complete within the deadline.
    #[cfg(feature = "linux")]
    pub async fn capture_bounded(
        &self,
        program: &Path,
        args: &[&str],
        deadline: Duration,
        limit: usize,
    ) -> Result<Vec<u8>> {
        self.capture(program, args, deadline, limit, false, &[])
            .await
            .and_then(CapturedOutput::require_success)
            .map(|output| output.stdout)
    }

    /// Retain the most recent bytes while continuing to drain the child pipe.
    #[cfg(feature = "linux")]
    pub async fn capture_tail(
        &self,
        program: &Path,
        args: &[&str],
        deadline: Duration,
        limit: usize,
    ) -> Result<(Vec<u8>, bool)> {
        self.capture(program, args, deadline, limit, true, &[])
            .await
            .and_then(CapturedOutput::require_success)
            .map(|output| (output.stdout, output.truncated))
    }

    #[cfg(feature = "linux")]
    pub async fn capture_status(
        &self,
        program: &Path,
        args: &[&str],
        deadline: Duration,
        limit: usize,
    ) -> Result<CapturedOutput> {
        self.capture(program, args, deadline, limit, false, &[])
            .await
    }

    #[cfg(feature = "linux")]
    pub async fn capture_package_job(
        &self,
        program: &Path,
        args: &[&str],
        environment: &[(&str, &str)],
    ) -> Result<CapturedOutput> {
        self.capture(
            program,
            args,
            Duration::from_secs(20 * 60),
            128 * 1024,
            true,
            environment,
        )
        .await
    }

    #[cfg(feature = "linux")]
    async fn capture(
        &self,
        program: &Path,
        args: &[&str],
        deadline: Duration,
        limit: usize,
        tail: bool,
        environment: &[(&str, &str)],
    ) -> Result<CapturedOutput> {
        use std::collections::VecDeque;
        use tokio::io::AsyncReadExt;

        if self.test_mode {
            bail!("command capture is unavailable in simulation");
        }
        let mut command = Command::new(program);
        command
            .args(args)
            .env("LC_ALL", "C")
            .envs(environment.iter().copied())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        if !environment.is_empty() {
            command.process_group(0);
        }
        let mut child = command
            .spawn()
            .with_context(|| format!("failed to start {}", program.display()))?;
        #[cfg(unix)]
        let mut group_guard = ProcessGroupGuard(if environment.is_empty() {
            None
        } else {
            child.id()
        });
        let mut stdout = child.stdout.take().context("missing command stdout")?;
        let mut stderr = child.stderr.take().context("missing command stderr")?;
        let result = timeout(deadline, async {
            let read_stderr = async {
                let mut first = [0];
                let has_error = stderr.read(&mut first).await? != 0;
                tokio::io::copy(&mut stderr, &mut tokio::io::sink()).await?;
                Ok::<_, anyhow::Error>(has_error)
            };
            let read_stdout = async {
                let mut bytes = VecDeque::new();
                let mut chunk = [0; 8192];
                let mut truncated = false;
                loop {
                    let count = stdout.read(&mut chunk).await?;
                    if count == 0 {
                        break;
                    }
                    let overflow = (bytes.len() + count).saturating_sub(limit);
                    if overflow > 0 {
                        if !tail {
                            bail!("command output exceeds {limit} bytes");
                        }
                        let discard = overflow.min(bytes.len());
                        bytes.drain(..discard);
                        bytes.extend(&chunk[overflow - discard..count]);
                        truncated = true;
                    } else {
                        bytes.extend(&chunk[..count]);
                    }
                }
                Ok::<_, anyhow::Error>((bytes.into(), truncated))
            };
            let ((stdout, truncated), had_stderr) = tokio::try_join!(read_stdout, read_stderr)?;
            Ok(CapturedOutput {
                stdout,
                truncated,
                had_stderr,
                status: child.wait().await?,
            })
        })
        .await
        .with_context(|| format!("command timed out: {}", program.display()))?;
        #[cfg(unix)]
        if result.is_ok() {
            group_guard.0 = None;
        }
        result
    }

    /// Run a command and all its hook subprocesses in a dedicated Unix process
    /// group. On timeout, kill the group rather than only the parent shell.
    pub async fn run_process_group<I, S>(
        &self,
        program: &Path,
        args: I,
        timeout_duration: Duration,
    ) -> Result<CommandResult>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
        if self.test_mode {
            return self.run(program, args, timeout_duration).await;
        }
        if !program.exists() {
            bail!("command not found: {}", program.display());
        }

        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.as_std_mut().process_group(0);

        let child = command
            .spawn()
            .with_context(|| format!("failed to start {}", program.display()))?;
        #[cfg(unix)]
        let mut group = ProcessGroupGuard(child.id());
        let output = match timeout(timeout_duration, child.wait_with_output()).await {
            Ok(result) => result?,
            Err(_) => {
                bail!("command timed out: {}", program.display());
            }
        };
        #[cfg(unix)]
        {
            group.0 = None;
        }

        Ok(CommandResult {
            success: output.status.success(),
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).trim().to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            simulated: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_command_runner_simulation() {
        let runner = CommandRunner::new(true);
        let res = runner
            .run(
                Path::new("/bin/nonexistent"),
                vec!["arg1", "arg2"],
                Duration::from_secs(1),
            )
            .await
            .unwrap();

        assert!(res.simulated);
        assert!(res.success);
        assert_eq!(res.code, Some(0));
        assert!(res.stdout.contains("SIMULATED: /bin/nonexistent arg1 arg2"));
    }

    #[tokio::test]
    async fn test_command_runner_non_existent() {
        let runner = CommandRunner::new(false);
        let res = runner
            .run(
                Path::new("/nonexistent/path/cmd"),
                vec!["arg"],
                Duration::from_secs(1),
            )
            .await;

        assert!(res.is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_process_group_timeout_kills_hook_descendants() {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("child-survived");
        let command = format!("sleep 1; touch '{}'", marker.display());
        let result = CommandRunner::new(false)
            .run_process_group(
                Path::new("/bin/sh"),
                vec!["-c".to_string(), command],
                Duration::from_millis(100),
            )
            .await;
        assert!(result.unwrap_err().to_string().contains("timed out"));
        tokio::time::sleep(Duration::from_millis(1200)).await;
        assert!(
            !marker.exists(),
            "a timed-out command left a subprocess running"
        );
    }

    #[tokio::test]
    async fn test_command_runner_real_echo() {
        let runner = CommandRunner::new(false);
        let res = runner
            .run(
                Path::new("/bin/echo"),
                vec!["hello"],
                Duration::from_secs(1),
            )
            .await
            .unwrap();

        assert!(!res.simulated);
        assert!(res.success);
        assert_eq!(res.stdout, "hello");
    }
}
