//! Linux-only /proc parsers translated from Server Man's monitor/parse.go and source.go.
//! Router system_usage parsing and response contracts are deliberately unchanged.

use std::{collections::BTreeMap, fs::File, io::Read, path::Path};

use anyhow::{Context, Result, ensure};

use super::{CpuStat, Memory, NetworkStat};

const MAX_PROC_BYTES: u64 = 1024 * 1024;

pub(super) fn read(path: &Path) -> Result<String> {
    let mut text = String::new();
    File::open(path)?
        .take(MAX_PROC_BYTES + 1)
        .read_to_string(&mut text)?;
    ensure!(text.len() as u64 <= MAX_PROC_BYTES, "proc counter file exceeds limit");
    Ok(text)
}

pub(super) fn parse_cpu(text: &str) -> Result<Vec<CpuStat>> {
    let mut result = Vec::new();
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let Some(name) = fields.next() else { continue };
        if !name.starts_with("cpu") {
            break;
        }
        let mut numbers = [0_u64; 8];
        let mut count = 0;
        // Guest and guest_nice are already counted in user and nice.
        for (target, field) in numbers.iter_mut().zip(fields) {
            *target = field.parse().context("invalid CPU counter")?;
            count += 1;
        }
        ensure!(count >= 4, "incomplete CPU counters");
        result.push(CpuStat {
            name: name.into(),
            user: numbers[0],
            nice: numbers[1],
            system: numbers[2],
            idle: numbers[3],
            iowait: numbers[4],
            irq: numbers[5],
            softirq: numbers[6],
            steal: numbers[7],
        });
    }
    ensure!(result.iter().any(|cpu| cpu.name == "cpu"), "aggregate CPU counters are missing");
    Ok(result)
}

pub(super) fn parse_memory(text: &str) -> Result<Memory> {
    let mut values = BTreeMap::new();
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        if let (Some(key), Some(value)) = (fields.next(), fields.next()) {
            if let Ok(number) = value.parse::<u64>() {
                values.insert(key.trim_end_matches(':'), number.saturating_mul(1024));
            }
        }
    }
    let get = |key: &str| values.get(key).copied().unwrap_or_default();
    let total = get("MemTotal");
    ensure!(total > 0, "memory total is missing");
    let cached = get("Cached").saturating_add(get("SReclaimable"));
    let buffers = get("Buffers");
    let available = values.get("MemAvailable").copied().unwrap_or_else(|| {
        get("MemFree").saturating_add(buffers).saturating_add(cached)
    }).min(total);
    let swap_total = get("SwapTotal");
    Ok(Memory {
        total,
        available,
        used: total - available,
        cached,
        buffers,
        swap_total,
        swap_used: swap_total.saturating_sub(get("SwapFree")),
    })
}

pub(super) fn parse_network(text: &str) -> Result<BTreeMap<String, NetworkStat>> {
    let mut result = BTreeMap::new();
    for line in text.lines() {
        let Some((name, data)) = line.split_once(':') else { continue };
        let fields: Vec<_> = data.split_whitespace().collect();
        ensure!(fields.len() >= 16, "incomplete network counters");
        let rx_bytes = fields[0].parse().context("invalid network receive counter")?;
        let tx_bytes = fields[8].parse().context("invalid network transmit counter")?;
        result.insert(name.trim().into(), NetworkStat { rx_bytes, tx_bytes });
    }
    Ok(result)
}

fn nonnegative(value: &str) -> Option<f64> {
    let value: f64 = value.parse().ok()?;
    (value.is_finite() && value >= 0.0).then_some(value)
}

pub(super) fn parse_uptime(text: &str) -> Option<f64> {
    nonnegative(text.split_whitespace().next()?)
}

pub(super) fn parse_load(text: &str) -> Option<[f64; 3]> {
    let mut fields = text.split_whitespace();
    Some([
        nonnegative(fields.next()?)?,
        nonnegative(fields.next()?)?,
        nonnegative(fields.next()?)?,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_counters_ignore_guest_and_reject_invalid_input() {
        let cpus = parse_cpu("cpu 100 10 20 800 30 2 3 4 50 5\ncpu0 1 2 3 4\n").unwrap();
        assert_eq!(cpus[0].user, 100);
        assert_eq!(cpus[0].steal, 4);
        assert_eq!(cpus[1].iowait, 0);
        for invalid in ["", "cpu x 2 3 4", "cpu 1 2 3", "cpu0 1 2 3 4"] {
            assert!(parse_cpu(invalid).is_err());
        }
    }

    #[test]
    fn memory_preserves_zero_available_and_old_kernel_cache_fallback() {
        let memory = parse_memory("MemTotal: 1000 kB\nMemAvailable: 600 kB\nCached: 200 kB\nSwapTotal: 100 kB\nSwapFree: 40 kB\n").unwrap();
        assert_eq!(memory.used, 400 * 1024);
        assert_eq!(memory.swap_used, 60 * 1024);
        let memory = parse_memory("MemTotal: 1000\nMemFree: 20\nBuffers: 10\nCached: 30\nSReclaimable: 40\n").unwrap();
        assert_eq!(memory.available, 100 * 1024);
        assert_eq!(memory.cached, 70 * 1024);
        let memory = parse_memory("MemTotal: 1000\nMemAvailable: 0\nMemFree: 20").unwrap();
        assert_eq!(memory.available, 0);
        assert!(parse_memory("MemTotal: not-a-number").is_err());
    }

    #[test]
    fn network_reads_all_interfaces_and_rejects_truncated_counters() {
        let text = "eth1: 1 0 0 0 0 0 0 0 2 0 0 0 0 0 0 0\nlo: 3 0 0 0 0 0 0 0 4 0 0 0 0 0 0 0";
        let networks = parse_network(text).unwrap();
        assert_eq!(networks.len(), 2);
        assert_eq!(networks["eth1"].tx_bytes, 2);
        assert_eq!(networks["lo"].rx_bytes, 3);
        assert!(parse_network("eth0: 1 2").is_err());
        assert!(parse_network(&text.replace("eth1: 1", "eth1: x")).is_err());
    }

    #[test]
    fn optional_metrics_never_fabricate_zero() {
        assert_eq!(parse_uptime("123.5 1"), Some(123.5));
        assert_eq!(parse_load("0.1 0.2 0.3 1/2"), Some([0.1, 0.2, 0.3]));
        assert_eq!(parse_uptime("NaN"), None);
        assert_eq!(parse_uptime("-1"), None);
        assert_eq!(parse_load("NaN +Inf 1"), None);
        assert_eq!(parse_load("0.1 0.2"), None);
    }
}
