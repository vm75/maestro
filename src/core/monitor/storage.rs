use std::{collections::BTreeMap, fs, io::Read, path::Path, time::Duration};

use serde::Serialize;

const MAX_TABLE: u64 = 1024 * 1024;
const LOCAL_FILESYSTEMS: &[&str] = &[
    "ext2", "ext3", "ext4", "xfs", "btrfs", "zfs", "f2fs", "bcachefs", "overlay", "vfat", "exfat",
    "ntfs3",
];

#[derive(Clone, Serialize)]
pub(super) struct Disk {
    device: String,
    mount: String,
    filesystem: String,
    total: u64,
    used: u64,
    available: u64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DiskIo {
    name: String,
    read_bytes_per_second: Option<f64>,
    write_bytes_per_second: Option<f64>,
}

#[derive(Clone, Serialize)]
pub(super) struct Temperature {
    name: String,
    celsius: f64,
}

#[derive(Default)]
pub(super) struct Storage {
    pub disks: Vec<Disk>,
    pub temperatures: Vec<Temperature>,
    pub warnings: Vec<&'static str>,
    counters: BTreeMap<String, (u64, u64)>,
}

impl Storage {
    pub fn read(proc_root: &Path, sys_root: &Path) -> Self {
        let mut result = Self {
            temperatures: temperatures(sys_root),
            ..Self::default()
        };
        if let Some(table) = read_text(&proc_root.join("diskstats"), MAX_TABLE) {
            result.counters = disk_counters(&table, sys_root);
        } else {
            result
                .warnings
                .push("Disk activity counters are unavailable.");
        }
        if let Some(table) = read_text(&proc_root.join("self/mountinfo"), MAX_TABLE) {
            for mut disk in mounts(&table) {
                let Ok(stat) = rustix::fs::statvfs(&disk.mount) else {
                    if !result
                        .warnings
                        .contains(&"Some mounted filesystems could not be read.")
                    {
                        result
                            .warnings
                            .push("Some mounted filesystems could not be read.");
                    }
                    continue;
                };
                disk.total = stat.f_blocks.saturating_mul(stat.f_frsize);
                disk.used = stat
                    .f_blocks
                    .saturating_sub(stat.f_bfree)
                    .saturating_mul(stat.f_frsize);
                disk.available = stat.f_bavail.saturating_mul(stat.f_frsize);
                result.disks.push(disk);
                if result.disks.len() == 32 {
                    break;
                }
            }
        } else {
            result.warnings.push("Mounted filesystems are unavailable.");
        }
        result
    }

    pub fn rates(&self, previous: Option<(&Self, Duration)>) -> Vec<DiskIo> {
        self.counters
            .iter()
            .map(|(name, &(read, write))| {
                let previous = previous.filter(|(_, elapsed)| !elapsed.is_zero()).and_then(
                    |(old, elapsed)| Some((old.counters.get(name)?, elapsed.as_secs_f64())),
                );
                DiskIo {
                    name: name.clone(),
                    read_bytes_per_second: previous.and_then(|(&(old, _), elapsed)| {
                        Some(read.checked_sub(old)? as f64 / elapsed)
                    }),
                    write_bytes_per_second: previous.and_then(|(&(_, old), elapsed)| {
                        Some(write.checked_sub(old)? as f64 / elapsed)
                    }),
                }
            })
            .collect()
    }
}

fn read_text(path: &Path, max: u64) -> Option<String> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .ok()?
        .take(max + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > max {
        return None;
    }
    String::from_utf8(bytes).ok()
}

fn disk_counters(table: &str, sys_root: &Path) -> BTreeMap<String, (u64, u64)> {
    table
        .lines()
        .filter_map(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            if fields.len() < 14 {
                return None;
            }
            let name = fields[2];
            if name.starts_with("loop")
                || name.starts_with("ram")
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_.!-".contains(&byte))
                || matches!(name, "." | "..")
                || !sys_root.join("block").join(name).is_dir()
            {
                return None;
            }
            let bytes = |index: usize| fields.get(index)?.parse::<u64>().ok()?.checked_mul(512);
            Some((name.into(), (bytes(5)?, bytes(9)?)))
        })
        .take(128)
        .collect()
}

fn mounts(table: &str) -> Vec<Disk> {
    let mut by_device = BTreeMap::<_, Disk>::new();
    for line in table.lines() {
        let Some((left, right)) = line.split_once(" - ") else {
            continue;
        };
        let mut source = right.split_whitespace();
        let (Some(filesystem), Some(device)) = (source.next(), source.next()) else {
            continue;
        };
        if !LOCAL_FILESYSTEMS.contains(&filesystem) {
            continue;
        }
        let fields: Vec<_> = left.split_whitespace().collect();
        if fields.len() < 6 {
            continue;
        }
        let (device_id, mount) = (&fields[2], &fields[4]);
        let mount = unescape(mount);
        if !Path::new(&mount).is_absolute() {
            continue;
        }
        let key = (*device_id, filesystem);
        if by_device.get(&key).is_some_and(|old| {
            old.mount.len() <= mount.len()
        }) {
            continue;
        }
        by_device.insert(
            key,
            Disk {
                device: unescape(device),
                mount,
                filesystem: filesystem.into(),
                total: 0,
                used: 0,
                available: 0,
            },
        );
    }
    let mut result: Vec<_> = by_device.into_values().collect();
    result.sort_unstable_by(|left, right| left.mount.cmp(&right.mount));
    result
}

fn unescape(value: &str) -> String {
    value
        .replace(r"\040", " ")
        .replace(r"\011", "\t")
        .replace(r"\012", "\n")
        .replace(r"\134", "\\")
}

fn temperatures(sys_root: &Path) -> Vec<Temperature> {
    let Ok(entries) = fs::read_dir(sys_root.join("class/thermal")) else {
        return vec![];
    };
    entries
        .flatten()
        .take(256)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            if !name.strip_prefix("thermal_zone").is_some_and(|suffix| {
                !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
            }) {
                return None;
            }
            let path = entry.path();
            let millidegrees: i32 = read_text(&path.join("temp"), 128)?.trim().parse().ok()?;
            if !(-40_000..=150_000).contains(&millidegrees) {
                return None;
            }
            let label = read_text(&path.join("type"), 128)
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
                .unwrap_or(name);
            Some(Temperature {
                name: label,
                celsius: f64::from(millidegrees) / 1000.0,
            })
        })
        .take(16)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deduplicates_mounts_and_excludes_remote_filesystems() {
        let result = mounts(
            "1 0 8:1 / /srv/bind/long rw - ext4 /dev/sda1 rw\n\
             2 0 8:1 / /disk\\040one rw - ext4 /dev/sda1 rw\n\
             3 0 0:2 / /remote rw - nfs example:/data rw",
        );
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].mount, "/disk one");
        let equal = mounts(
            "1 0 8:1 / /zzz rw - ext4 /dev/sda1 rw\n\
             2 0 8:1 / /aaa rw - ext4 /dev/sda1 rw",
        );
        assert_eq!(equal[0].mount, "/zzz");
    }

    #[test]
    fn disk_rates_drop_resets_and_ignore_partitions() {
        let sys = tempfile::tempdir().unwrap();
        fs::create_dir_all(sys.path().join("block/sda")).unwrap();
        assert!(disk_counters("8 0 sda 1 1 10 1 1 1 20", sys.path()).is_empty());
        let old = Storage {
            counters: disk_counters("8 0 sda 1 1 10 1 1 1 20 0 0 0 0", sys.path()),
            ..Storage::default()
        };
        let new = Storage {
            counters: disk_counters(
                "8 0 sda 1 1 14 1 1 1 10 0 0 0 0\n8 1 sda1 1 1 14 1 1 1 10 0 0 0 0",
                sys.path(),
            ),
            ..Storage::default()
        };
        let rates = new.rates(Some((&old, Duration::from_secs(2))));
        assert_eq!(rates.len(), 1);
        assert_eq!(rates[0].read_bytes_per_second, Some(1024.0));
        assert_eq!(rates[0].write_bytes_per_second, None);
    }

    #[test]
    fn thermal_readings_use_millidegrees_and_skip_invalid_values() {
        let sys = tempfile::tempdir().unwrap();
        let zone = sys.path().join("class/thermal/thermal_zone0");
        std::fs::create_dir_all(&zone).unwrap();
        std::fs::write(zone.join("type"), "cpu-package\n").unwrap();
        std::fs::write(zone.join("temp"), "42875\n").unwrap();
        let readings = temperatures(sys.path());
        assert_eq!(readings.len(), 1);
        assert_eq!(readings[0].name, "cpu-package");
        assert_eq!(readings[0].celsius, 42.875);
        std::fs::write(zone.join("temp"), "999999").unwrap();
        assert!(temperatures(sys.path()).is_empty());
    }
}
