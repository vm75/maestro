use std::{path::PathBuf, time::Duration};

use anyhow::{Result, anyhow, ensure};

use crate::{
    command::CommandRunner,
    core::declarative::{
        ApplyProvider, Change, Config, ObservedResource, Operation, RecoveryProvider, Service,
    },
};

pub struct IncusProvider {
    pub binary: PathBuf,
    pub runner: CommandRunner,
}

impl IncusProvider {
    pub fn new(binary: PathBuf, runner: CommandRunner) -> Self {
        Self { binary, runner }
    }

    pub fn matches(&self, service: &Service) -> bool {
        service.backend == "incus" || service.engine == "incus"
    }

    pub async fn action(&self, service_id: &str, action: &str) -> Result<String> {
        let cmd = match action {
            "start" => "start",
            "stop" => "stop",
            "restart" => "restart",
            other => return Err(anyhow!("unsupported incus action: {other}")),
        };
        if self.runner.is_test_mode() {
            return Ok(format!("SIMULATED: Incus {cmd} on {service_id}"));
        }
        let output = self
            .runner
            .capture_bounded(
                &self.binary,
                &[cmd, service_id],
                Duration::from_secs(30),
                64 * 1024,
            )
            .await?;
        Ok(String::from_utf8_lossy(&output).trim().to_string())
    }

    pub async fn logs(&self, service_id: &str) -> Result<String> {
        if self.runner.is_test_mode() {
            return Ok(format!("SIMULATED: Incus logs for {service_id}"));
        }
        let output = self
            .runner
            .capture_bounded(
                &self.binary,
                &["info", "--show-log", service_id],
                Duration::from_secs(15),
                128 * 1024,
            )
            .await?;
        Ok(String::from_utf8_lossy(&output).to_string())
    }

    pub async fn status(&self, service_id: &str) -> Result<String> {
        let output = self
            .runner
            .capture_bounded(
                &self.binary,
                &[
                    "list",
                    "--format",
                    "csv",
                    "-c",
                    "n,s",
                    &format!("^{service_id}$"),
                ],
                Duration::from_secs(10),
                16 * 1024,
            )
            .await?;
        let text = String::from_utf8_lossy(&output);
        let status = text
            .lines()
            .find(|line| line.starts_with(service_id))
            .and_then(|line| line.split(',').nth(1))
            .unwrap_or("UNKNOWN")
            .to_lowercase();
        Ok(status)
    }
}

impl ApplyProvider for IncusProvider {
    async fn preflight(&self, change: &Change, _previous: &Config, desired: &Config) -> Result<()> {
        let Some(service_id) = change.resource.strip_prefix("service/") else {
            return Ok(());
        };
        if let Some(service) = desired.services.get(service_id) {
            if !self.matches(service) {
                return Ok(());
            }
        }
        ensure!(super::id_valid(service_id), "invalid incus service ID");
        Ok(())
    }

    async fn apply(&self, change: &Change, _previous: &Config, desired: &Config) -> Result<()> {
        let Some(service_id) = change.resource.strip_prefix("service/") else {
            return Ok(());
        };
        if self.runner.is_test_mode() {
            return Ok(());
        }
        match change.operation {
            Operation::Create => {
                let service = desired
                    .services
                    .get(service_id)
                    .ok_or_else(|| anyhow!("service not found"))?;
                if !self.matches(service) {
                    return Ok(());
                }
                let image = if let Some(vm) = &service.vm {
                    &vm.image
                } else if let Some((_, c)) = service.containers.iter().next() {
                    &c.image
                } else {
                    return Err(anyhow!("incus service must define a VM or container"));
                };
                let is_vm = service.kind == "vm" || service.vm.is_some();
                let mut args = vec!["init", image.as_str(), service_id];
                if is_vm {
                    args.push("--vm");
                }
                self.runner
                    .capture_bounded(&self.binary, &args, Duration::from_secs(60), 64 * 1024)
                    .await?;
                if service.enabled {
                    let _ = self.action(service_id, "start").await;
                }
            }
            Operation::Update => {
                let service = desired
                    .services
                    .get(service_id)
                    .ok_or_else(|| anyhow!("service not found"))?;
                if !self.matches(service) {
                    return Ok(());
                }
                if service.enabled {
                    let _ = self.action(service_id, "start").await;
                } else {
                    let _ = self.action(service_id, "stop").await;
                }
            }
            Operation::Remove => {
                let _ = self.action(service_id, "stop").await;
                let _ = self
                    .runner
                    .capture_bounded(
                        &self.binary,
                        &["delete", "-f", service_id],
                        Duration::from_secs(30),
                        64 * 1024,
                    )
                    .await;
            }
        }
        Ok(())
    }
}

impl RecoveryProvider for IncusProvider {
    async fn observe(
        &self,
        change: &Change,
        _previous: &Config,
        _desired: &Config,
    ) -> Result<ObservedResource> {
        let Some(service_id) = change.resource.strip_prefix("service/") else {
            return Ok(ObservedResource::Unknown);
        };
        if self.runner.is_test_mode() {
            return Ok(if change.operation == Operation::Create {
                ObservedResource::Before
            } else {
                ObservedResource::After
            });
        }
        let status = self.status(service_id).await;
        match status {
            Ok(ref s) if s != "unknown" && !s.is_empty() => {
                if change.operation == Operation::Create {
                    Ok(ObservedResource::After)
                } else {
                    Ok(ObservedResource::Before)
                }
            }
            _ => {
                if change.operation == Operation::Create {
                    Ok(ObservedResource::Before)
                } else {
                    Ok(ObservedResource::After)
                }
            }
        }
    }
}

#[cfg(test)]
mod simulation_tests {
    use super::*;

    #[tokio::test]
    async fn lifecycle_and_logs_do_not_require_native_capture_in_simulation() {
        let provider = IncusProvider::new("/nonexistent/incus".into(), CommandRunner::new(true));
        assert!(
            provider
                .action("demo", "restart")
                .await
                .unwrap()
                .starts_with("SIMULATED:")
        );
        assert!(
            provider
                .logs("demo")
                .await
                .unwrap()
                .starts_with("SIMULATED:")
        );
        assert!(provider.action("demo", "invalid").await.is_err());
    }
}
