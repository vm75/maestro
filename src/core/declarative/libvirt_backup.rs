//! Basic stopped-domain XML export and standalone declared disk verification.
use super::*;
use crate::core::declarative::{
    backup_capture::{self as capture, NativeExport, NativeObject},
    storage,
};
use std::{fs::File, os::unix::fs::MetadataExt, time::Instant};

impl LibvirtProvider {
    pub(crate) async fn backup_dependency(&self, c: &Config, id: &str) -> Result<NativeObject> {
        let (want, obs) = self.verified(c, id).await?;
        ensure!(
            matches!(obs.state.as_str(), "running" | "shut off"),
            "Libvirt guest state is not stable for capture"
        );
        // Metadata-only discovery uses ordinary inactive XML; private XML and
        // physical disk inspection occur only during confirmed capture.
        let raw = self
            .command(
                &["dumpxml", "--domain", &want.uuid, "--inactive"],
                5,
                super::super::MAX_CONFIG_BYTES,
                false,
            )
            .await?;
        xml::validate_backup_xml(&raw, &want)?;
        Ok(NativeObject {
            backend: "libvirt".into(),
            name: xml::domain_name(id),
            project: String::new(),
            pool: String::new(),
            content_type: String::new(),
        })
    }
    pub(crate) async fn validate_backup(
        &self,
        c: &Config,
        id: &str,
        object: &NativeObject,
        stopped: bool,
    ) -> Result<Vec<u8>> {
        let (want, obs) = self.verified(c, id).await?;
        ensure!(
            *object == self.backup_dependency(c, id).await?,
            "Libvirt export identity changed"
        );
        ensure!(
            !stopped || obs.state == "shut off",
            "Libvirt guest must be verified shut off for capture"
        );
        let uuid = &want.uuid;
        let private = self
            .command(
                &["dumpxml", "--domain", uuid, "--inactive", "--security-info"],
                5,
                super::super::MAX_CONFIG_BYTES,
                false,
            )
            .await
            .map_err(|_| anyhow!("private Libvirt definition is unavailable"))?;
        xml::validate_backup_xml(&private, &want)?;
        let saved = self
            .command(
                &["list", "--all", "--uuid", "--with-managed-save"],
                5,
                256 * 1024,
                false,
            )
            .await?;
        for value in std::str::from_utf8(&saved)?.split_whitespace() {
            ensure!(
                uuid::Uuid::parse_str(value).is_ok() && !value.eq_ignore_ascii_case(uuid),
                "Libvirt managed-save state is unresolved"
            );
        }
        for operation in ["snapshot-list", "checkpoint-list"] {
            let history = self
                .command(
                    &[operation, "--domain", uuid, "--name"],
                    5,
                    256 * 1024,
                    false,
                )
                .await?;
            ensure!(
                std::str::from_utf8(&history)?.trim().is_empty(),
                "Libvirt snapshots/checkpoints need explicit capture support"
            );
        }
        let program = storage::qemu_img()
            .ok_or_else(|| anyhow!("qemu-img is required for Libvirt capture"))?;
        self.runner
            .capture_bounded(&program, &["--version"], Duration::from_secs(5), 64 * 1024)
            .await?;
        for disk_id in &c.services[id].vm.as_ref().unwrap().disks {
            let disk = c
                .storage
                .get(disk_id)
                .ok_or_else(|| anyhow!("missing declared VM disk"))?;
            ensure!(
                !disk.external
                    && disk.kind == "file"
                    && matches!(disk.format.as_str(), "raw" | "qcow2"),
                "Libvirt disks need owned raw/qcow2 storage"
            );
            let path = self.storage_base.join(&disk.path);
            let file = capture::open_absolute(&path, false)?;
            let before = file.metadata()?;
            ensure!(
                before.is_file() && before.len() > 0,
                "Libvirt disk is not a stable nonempty regular file"
            );
            if obs.state == "shut off" {
                storage::check_image_header(&file, &disk.format)?;
                use std::os::fd::AsRawFd;
                let fd = format!("/proc/{}/fd/{}", std::process::id(), file.as_raw_fd());
                let info = self
                    .runner
                    .capture_bounded(
                        &program,
                        &["info", "--output=json", "-f", &disk.format, &fd],
                        Duration::from_secs(30),
                        64 * 1024,
                    )
                    .await?;
                let info: serde_json::Value = serde_json::from_slice(&info)?;
                ensure!(
                    info["format"] == disk.format
                        && info["virtual-size"].as_u64().is_some_and(|v| v > 0)
                        && info.get("backing-filename").is_none()
                        && info.get("full-backing-filename").is_none()
                        && info
                            .get("snapshots")
                            .is_none_or(|v| v.as_array().is_some_and(Vec::is_empty))
                        && info
                            .get("encrypted")
                            .is_none_or(|v| v.as_bool() == Some(false)),
                    "Libvirt disk is not a standalone image without snapshots"
                );
                let after = file.metadata()?;
                ensure!(
                    (
                        before.dev(),
                        before.ino(),
                        before.len(),
                        before.mtime(),
                        before.mtime_nsec(),
                        before.ctime(),
                        before.ctime_nsec()
                    ) == (
                        after.dev(),
                        after.ino(),
                        after.len(),
                        after.mtime(),
                        after.mtime_nsec(),
                        after.ctime(),
                        after.ctime_nsec()
                    ),
                    "Libvirt disk changed during verification"
                );
            }
        }
        Ok(private)
    }
    pub(crate) async fn backup_export(
        &self,
        c: &Config,
        id: &str,
        object: &NativeObject,
        root: &File,
        end: Instant,
    ) -> Result<NativeExport> {
        use std::io::Write;
        let raw = self.validate_backup(c, id, object, true).await?;
        let path = format!(
            "native/libvirt/{}.xml",
            super::super::revision(object.name.as_bytes())
        );
        let mut file = capture::create_payload(root, &path)?;
        file.write_all(&raw)?;
        file.sync_all()?;
        ensure!(
            self.validate_backup(c, id, object, true).await? == raw,
            "Libvirt private definition changed during capture"
        );
        capture::native_export(
            root,
            &path,
            "libvirt_guest_state",
            vec![format!("service/{id}")],
            object.clone(),
            end,
        )
    }
}
