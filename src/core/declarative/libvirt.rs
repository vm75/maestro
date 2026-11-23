use std::{path::PathBuf, time::Duration};

use anyhow::{Result, anyhow, ensure};

use crate::{
    command::CommandRunner,
    core::declarative::{
        ApplyProvider, Change, Config, ObservedResource, Operation, RecoveryProvider, Service,
    },
};

pub fn render_libvirt_domain(service: &Service, id: &str, fingerprint: &str) -> Result<String> {
    ensure!(super::id_valid(id), "invalid libvirt domain ID");
    let vm = service
        .vm
        .as_ref()
        .ok_or_else(|| anyhow!("service has no VM definition"))?;
    let memory_kib = if vm.memory_mib > 0 {
        vm.memory_mib * 1024
    } else {
        1024 * 1024
    };
    let cpus = if vm.cpus > 0 { vm.cpus } else { 1 };
    let uuid_tag = if !vm.uuid.is_empty() {
        format!("<uuid>{}</uuid>", vm.uuid)
    } else {
        String::new()
    };
    let mac_tag = if !vm.mac.is_empty() {
        format!("<mac address='{}'/>", vm.mac)
    } else {
        String::new()
    };
    Ok(format!(
        "<domain type='kvm'>\n  <name>{id}</name>\n  {uuid_tag}\n  <metadata>\n    <maestro:ownership xmlns:maestro='http://maestro.router/schema' service='{id}' fingerprint='{fingerprint}'/>\n  </metadata>\n  <memory unit='KiB'>{memory_kib}</memory>\n  <vcpu placement='static'>{cpus}</vcpu>\n  <os>\n    <type arch='x86_64' machine='q35'>hvm</type>\n    <boot dev='hd'/>\n  </os>\n  <devices>\n    <interface type='network'>\n      {mac_tag}\n      <source network='default'/>\n      <model type='virtio'/>\n    </interface>\n    <graphics type='vnc' port='-1' autoport='yes' listen='127.0.0.1'/>\n  </devices>\n</domain>\n"
    ))
}

pub struct LibvirtProvider {
    pub virsh: PathBuf,
    pub runner: CommandRunner,
}

impl LibvirtProvider {
    pub fn new(virsh: PathBuf, runner: CommandRunner) -> Self {
        Self { virsh, runner }
    }

    pub fn matches(&self, service: &Service) -> bool {
        service.backend == "libvirt"
    }

    pub async fn action(&self, service_id: &str, action: &str) -> Result<String> {
        let cmd = match action {
            "start" => "start",
            "stop" | "shutdown" => "shutdown",
            "restart" | "reboot" => "reboot",
            other => return Err(anyhow!("unsupported libvirt action: {other}")),
        };
        let output = self
            .runner
            .capture_bounded(
                &self.virsh,
                &[cmd, service_id],
                Duration::from_secs(30),
                64 * 1024,
            )
            .await?;
        Ok(String::from_utf8_lossy(&output).trim().to_string())
    }

    pub async fn status(&self, service_id: &str) -> Result<String> {
        let output = self
            .runner
            .capture_bounded(
                &self.virsh,
                &["dominfo", service_id],
                Duration::from_secs(10),
                16 * 1024,
            )
            .await?;
        let text = String::from_utf8_lossy(&output);
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("State:") {
                return Ok(rest.trim().to_lowercase());
            }
        }
        Ok("unknown".into())
    }
}

impl ApplyProvider for LibvirtProvider {
    async fn preflight(&self, change: &Change, _previous: &Config, desired: &Config) -> Result<()> {
        let Some(service_id) = change.resource.strip_prefix("service/") else {
            return Ok(());
        };
        if let Some(service) = desired.services.get(service_id) {
            if !self.matches(service) {
                return Ok(());
            }
        }
        ensure!(super::id_valid(service_id), "invalid libvirt service ID");
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
            Operation::Create | Operation::Update => {
                let service = desired
                    .services
                    .get(service_id)
                    .ok_or_else(|| anyhow!("service not found"))?;
                if !self.matches(service) {
                    return Ok(());
                }
                let xml = render_libvirt_domain(
                    service,
                    service_id,
                    change.after.as_deref().unwrap_or(""),
                )?;
                let temp_path =
                    std::env::temp_dir().join(format!("maestro-libvirt-{service_id}.xml"));
                std::fs::write(&temp_path, xml)?;
                let temp_str = temp_path
                    .to_str()
                    .ok_or_else(|| anyhow!("non-UTF8 temp path"))?;
                let define_res = self
                    .runner
                    .capture_bounded(
                        &self.virsh,
                        &["define", temp_str],
                        Duration::from_secs(30),
                        64 * 1024,
                    )
                    .await;
                let _ = std::fs::remove_file(&temp_path);
                define_res?;
                if service.enabled {
                    let _ = self
                        .runner
                        .capture_bounded(
                            &self.virsh,
                            &["autostart", service_id],
                            Duration::from_secs(15),
                            16 * 1024,
                        )
                        .await;
                    let _ = self.action(service_id, "start").await;
                } else {
                    let _ = self
                        .runner
                        .capture_bounded(
                            &self.virsh,
                            &["autostart", "--disable", service_id],
                            Duration::from_secs(15),
                            16 * 1024,
                        )
                        .await;
                }
            }
            Operation::Remove => {
                let _ = self
                    .runner
                    .capture_bounded(
                        &self.virsh,
                        &["destroy", service_id],
                        Duration::from_secs(20),
                        16 * 1024,
                    )
                    .await;
                let _ = self
                    .runner
                    .capture_bounded(
                        &self.virsh,
                        &["undefine", service_id],
                        Duration::from_secs(20),
                        16 * 1024,
                    )
                    .await;
            }
        }
        Ok(())
    }
}

impl RecoveryProvider for LibvirtProvider {
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
mod tests {
    use super::*;
    use crate::core::declarative::{Service, VM};

    #[test]
    fn test_render_libvirt_domain() {
        let service = Service {
            name: "Debian VM".to_string(),
            kind: "vm".to_string(),
            backend: "libvirt".to_string(),
            vm: Some(VM {
                memory_mib: 2048,
                cpus: 2,
                uuid: "c3d699a2-921d-4009-8b63-1d0144fbb7dc".to_string(),
                mac: "52:54:00:12:34:56".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let xml = render_libvirt_domain(&service, "debvm", "fp001").unwrap();
        assert!(xml.contains("<name>debvm</name>"));
        assert!(xml.contains("<memory unit='KiB'>2097152</memory>"));
        assert!(xml.contains("<vcpu placement='static'>2</vcpu>"));
        assert!(xml.contains("<uuid>c3d699a2-921d-4009-8b63-1d0144fbb7dc</uuid>"));
        assert!(xml.contains("<mac address='52:54:00:12:34:56'/>"));
        assert!(xml.contains("service='debvm' fingerprint='fp001'"));
    }
}
