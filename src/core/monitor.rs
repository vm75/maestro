//! Cached Linux resource samples shared across browser clients.

mod processes;
mod source;
mod storage;
pub use processes::{ProcessList, Sort as ProcessSort};

use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::Serialize;

use crate::config::AppConfig;

use super::system_usage::{CpuStat, NetworkStat};

struct Reading {
    cpu: Vec<CpuStat>,
    host: Host,
    frequency_mhz: Option<f64>,
    memory: Memory,
    network: BTreeMap<String, NetworkStat>,
    uptime_seconds: Option<f64>,
    warnings: Vec<&'static str>,
    load_average: Option<[f64; 3]>,
    processes: Option<processes::Counters>,
    storage: storage::Storage,
}

impl Reading {
    fn collect(config: &AppConfig) -> anyhow::Result<Self> {
        let paths = &config.paths;
        let cpu = source::parse_cpu(&source::read(&paths.proc_stat)?)?;
        let cpuinfo = paths
            .proc_stat
            .parent()
            .and_then(|root| source::read(&root.join("cpuinfo")).ok());
        let host = paths.proc_stat.parent().map_or_else(Host::default, |root| {
            source::host_identity(root, cpuinfo.as_deref().unwrap_or_default())
        });
        let frequency_mhz = cpuinfo.as_deref().and_then(source::cpu_frequency);
        let memory = source::parse_memory(&source::read(&paths.proc_meminfo)?)?;
        let mut warnings = Vec::new();
        let network = source::read(&paths.proc_net_dev)
            .and_then(|text| source::parse_network(&text))
            .unwrap_or_else(|_| {
                warnings.push("Network counters are unavailable.");
                BTreeMap::new()
            });
        let uptime_seconds = source::read(&paths.proc_uptime)
            .ok()
            .and_then(|text| source::parse_uptime(&text));
        if uptime_seconds.is_none() {
            warnings.push("Uptime is unavailable.");
        }
        let load_average = source::read(&paths.proc_loadavg)
            .ok()
            .and_then(|text| source::parse_load(&text));
        if load_average.is_none() {
            warnings.push("Load averages are unavailable.");
        }
        let processes = paths.proc_stat.parent().and_then(processes::Counters::read);
        if processes.is_none() {
            warnings.push("Process inventory is unavailable.");
        } else if processes.as_ref().is_some_and(processes::Counters::limited) {
            warnings.push("Process sampling is limited to 4096 visible processes.");
        }
        Ok(Self {
            cpu,
            host,
            frequency_mhz,
            memory,
            network,
            uptime_seconds,
            load_average,
            warnings,
            processes,
            storage: paths
                .proc_stat
                .parent()
                .map_or_else(storage::Storage::default, |proc_root| {
                    storage::Storage::read(proc_root, &paths.sys_root)
                }),
        })
    }
}

const INTERVAL: Duration = Duration::from_secs(3);
const HISTORY_LIMIT: usize = 60;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Point {
    at: DateTime<Utc>,
    cpu: Option<f64>,
    memory_percent: f64,
    rx: Option<f64>,
    tx: Option<f64>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    valid: bool,
    at: Option<DateTime<Utc>>,
    interval_seconds: u64,
    host: Host,
    cpu: Cpu,
    memory: Memory,
    uptime_seconds: Option<f64>,
    process_count: usize,
    processes_limited: bool,
    load: Option<[f64; 3]>,
    networks: Vec<Network>,
    disks: Vec<storage::Disk>,
    #[serde(rename = "diskIO")]
    disk_io: Vec<storage::DiskIo>,
    temperatures: Vec<storage::Temperature>,
    warnings: Vec<&'static str>,
    history: VecDeque<Point>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_error: Option<&'static str>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Host {
    hostname: String,
    os: String,
    kernel: String,
    architecture: String,
    cpu_model: String,
}

impl Default for Host {
    fn default() -> Self {
        let architecture = match std::env::consts::ARCH {
            "x86_64" => "amd64",
            "x86" => "386",
            "aarch64" => "arm64",
            "powerpc64" if cfg!(target_endian = "little") => "ppc64le",
            "powerpc64" => "ppc64",
            other => other,
        };
        Self {
            hostname: String::new(),
            os: "Linux".into(),
            kernel: String::new(),
            architecture: architecture.into(),
            cpu_model: "Unknown CPU".into(),
        }
    }
}

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct Cpu {
    usage: Option<f64>,
    cores: Vec<Core>,
    #[serde(rename = "frequencyMHz")]
    frequency_mhz: Option<f64>,
}

#[derive(Clone, Serialize)]
struct Core {
    name: String,
    usage: Option<f64>,
}

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct Memory {
    total: u64,
    available: u64,
    used: u64,
    cached: u64,
    buffers: u64,
    swap_total: u64,
    swap_used: u64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Network {
    name: String,
    rx_bytes: u64,
    tx_bytes: u64,
    rx_bytes_per_second: Option<f64>,
    tx_bytes_per_second: Option<f64>,
}

struct Samples {
    current: Snapshot,
    previous: Option<(Instant, Reading)>,
}

impl Default for Samples {
    fn default() -> Self {
        Self {
            current: Snapshot {
                valid: false,
                at: None,
                interval_seconds: INTERVAL.as_secs(),
                host: Host::default(),
                cpu: Cpu::default(),
                memory: Memory::default(),
                uptime_seconds: None,
                process_count: 0,
                processes_limited: false,
                load: None,
                networks: vec![],
                disks: vec![],
                disk_io: vec![],
                temperatures: vec![],
                warnings: vec![],
                history: VecDeque::new(),
                last_error: None,
            },
            previous: None,
        }
    }
}

#[derive(Clone)]
pub struct Monitor {
    config: Arc<AppConfig>,
    cache: Arc<OnceLock<Arc<Mutex<Samples>>>>,
}

impl Monitor {
    pub fn new(config: Arc<AppConfig>) -> Self {
        Self {
            config,
            cache: Arc::default(),
        }
    }

    fn samples(&self) -> &Mutex<Samples> {
        self.cache
            .get_or_init(|| Self::start_sampling(self.config.clone()))
    }

    fn start_sampling(config: Arc<AppConfig>) -> Arc<Mutex<Samples>> {
        let interval = config
            .linux_authority
            .as_ref()
            .map_or(INTERVAL, |authority| {
                Duration::from_secs(authority.settings.sample_interval_seconds)
            });
        let mut initial = Samples::default();
        initial.current.interval_seconds = interval.as_secs();
        let cache = Arc::new(Mutex::new(initial));
        if let Some(runtime) = (!config.test_mode)
            .then(tokio::runtime::Handle::try_current)
            .and_then(Result::ok)
        {
            let weak = Arc::downgrade(&cache);
            runtime.spawn(async move {
                let mut ticker = tokio::time::interval(interval);
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    ticker.tick().await;
                    let Some(cache) = weak.upgrade() else { break };
                    let config_for_read = config.clone();
                    let reading =
                        tokio::task::spawn_blocking(move || Reading::collect(&config_for_read))
                            .await;
                    let mut samples = cache.lock();
                    match reading {
                        Ok(Ok(usage)) => {
                            samples.accept(usage, Instant::now(), Utc::now());
                        }
                        _ => {
                            samples.current.last_error =
                                Some("CPU or memory counters are unavailable.")
                        }
                    }
                }
            });
        }
        cache
    }

    pub fn snapshot(&self) -> Snapshot {
        self.samples().lock().current.clone()
    }

    pub fn processes(&self, sort: ProcessSort, filter: &str, limit: usize) -> ProcessList {
        let cache = self.samples().lock();
        let counters = cache
            .previous
            .as_ref()
            .and_then(|(_, reading)| reading.processes.as_ref());
        processes::list(counters, cache.current.at, sort, filter, limit)
    }
}

impl Samples {
    fn accept(&mut self, mut usage: Reading, now: Instant, at: DateTime<Utc>) {
        // Preserve the reference's bounded time window and reject clock regressions.
        let previous = self.previous.as_ref().filter(|(sampled, _)| {
            now > *sampled && self.current.at.is_none_or(|old| at > old)
        });
        let cpu_usage = |cpu: &CpuStat| {
            previous.and_then(|(_, usage)| {
                usage
                    .cpu
                    .iter()
                    .find(|old| old.name == cpu.name)
                    .and_then(|old| cpu_delta(old, cpu).map(|(usage, _)| usage))
            })
        };
        let mut cpu = Cpu {
            frequency_mhz: usage.frequency_mhz,
            usage: usage
                .cpu
                .iter()
                .find(|cpu| cpu.name == "cpu")
                .and_then(cpu_usage),
            cores: usage
                .cpu
                .iter()
                .filter(|cpu| cpu.name != "cpu")
                .map(|cpu| Core {
                    name: cpu.name.clone(),
                    usage: cpu_usage(cpu),
                })
                .collect(),
        };
        cpu.cores.sort_by_key(|core| {
            core.name
                .strip_prefix("cpu")
                .and_then(|index| index.parse::<u32>().ok())
                .unwrap_or_default()
        });
        let aggregate_ticks = previous.and_then(|(_, old)| {
            let old = old.cpu.iter().find(|cpu| cpu.name == "cpu")?;
            let current = usage.cpu.iter().find(|cpu| cpu.name == "cpu")?;
            cpu_delta(old, current).map(|(_, total)| total)
        });
        let core_count = cpu.cores.len();
        let memory = usage.memory.clone();
        let networks: Vec<_> = usage
            .network
            .iter()
            .map(|(name, network)| {
                let rate = |counter: u64, select: fn(&NetworkStat) -> u64| {
                    let (sampled, old) = previous?;
                    let elapsed = now.checked_duration_since(*sampled)?.as_secs_f64();
                    if elapsed == 0.0 {
                        return None;
                    }
                    Some(counter.checked_sub(select(old.network.get(name)?))? as f64 / elapsed)
                };
                Network {
                    name: name.clone(),
                    rx_bytes: network.rx_bytes,
                    tx_bytes: network.tx_bytes,
                    rx_bytes_per_second: rate(network.rx_bytes, |old| old.rx_bytes),
                    tx_bytes_per_second: rate(network.tx_bytes, |old| old.tx_bytes),
                }
            })
            .collect();
        let history = &mut self.current.history;
        if history.back().is_some_and(|point| point.at >= at) {
            history.clear();
        }
        let cutoff = at - chrono::Duration::seconds(
            (self.current.interval_seconds * HISTORY_LIMIT as u64) as i64,
        );
        while history.front().is_some_and(|point| point.at < cutoff) {
            history.pop_front();
        }
        if history.len() == HISTORY_LIMIT {
            history.pop_front();
        }
        history.push_back(Point {
            at,
            cpu: cpu.usage,
            memory_percent: 100.0 * memory.used as f64 / memory.total as f64,
            rx: aggregate_rate(&networks, |network| network.rx_bytes_per_second),
            tx: aggregate_rate(&networks, |network| network.tx_bytes_per_second),
        });
        self.current = Snapshot {
            valid: true,
            at: Some(at),
            interval_seconds: self.current.interval_seconds,
            host: usage.host.clone(),
            cpu,
            memory,
            uptime_seconds: usage.uptime_seconds,
            process_count: usage.processes.as_ref().map_or(0, processes::Counters::len),
            processes_limited: usage.processes.as_ref().is_some_and(processes::Counters::limited),
            load: usage.load_average,
            networks,
            disk_io: usage.storage.rates(
                previous.map(|(at, sample)| (&sample.storage, now.saturating_duration_since(*at))),
            ),
            disks: std::mem::take(&mut usage.storage.disks),
            temperatures: std::mem::take(&mut usage.storage.temperatures),
            warnings: usage.warnings.iter().chain(&usage.storage.warnings).copied().collect(),
            history: std::mem::take(history),
            last_error: None,
        };
        if let (Some(current), Some((_, previous)), Some(ticks)) =
            (&mut usage.processes, previous, aggregate_ticks)
        {
            if let Some(previous) = &previous.processes {
                current.calculate(previous, ticks, core_count);
            }
        }
        self.previous = Some((now, usage));
    }
}

// The reference excludes loopback and never presents a partial aggregate as complete.
fn aggregate_rate(networks: &[Network], select: fn(&Network) -> Option<f64>) -> Option<f64> {
    let mut total = None;
    for network in networks.iter().filter(|network| network.name != "lo") {
        total = Some(total.unwrap_or(0.0) + select(network)?);
    }
    total
}

fn cpu_delta(old: &CpuStat, current: &CpuStat) -> Option<(f64, u64)> {
    let counters = |cpu: &CpuStat| {
        [
            cpu.idle,
            cpu.iowait,
            cpu.user,
            cpu.nice,
            cpu.system,
            cpu.irq,
            cpu.softirq,
            cpu.steal,
        ]
    };
    let mut deltas = counters(current)
        .into_iter()
        .zip(counters(old))
        .map(|(current, old)| current.checked_sub(old));
    // Linux iowait can decrease without indicating a counter reset.
    // Match Server Man parse.go: clamp only that component.
    let idle = deltas.next()??.checked_add(deltas.next()?.unwrap_or(0))?;
    let active = deltas.try_fold(0_u64, |total, delta| total.checked_add(delta?))?;
    let total = idle.checked_add(active)?;
    (total != 0).then(|| (100.0 * active as f64 / total as f64, total))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(ticks: u64, network: Option<u64>) -> Reading {
        Reading {
            host: Host::default(),
            frequency_mhz: None,
            cpu: vec![CpuStat {
                name: "cpu".into(),
                user: ticks,
                idle: ticks,
                ..CpuStat::default()
            }],
            memory: Memory {
                total: 100 * 1024,
                available: 25 * 1024,
                used: 75 * 1024,
                ..Memory::default()
            },
            network: network
                .map(|rx_bytes| ("eth0".into(), NetworkStat { rx_bytes, tx_bytes: 0 }))
                .into_iter()
                .collect(),
            warnings: vec![],
            uptime_seconds: None,
            load_average: None,
            processes: None,
            storage: storage::Storage::default(),
        }
    }

    #[test]
    fn monitoring_initializes_on_first_read_and_shares_the_cache() {
        let monitor = Monitor::new(Arc::new(AppConfig::default()));
        let clone = monitor.clone();

        assert!(monitor.cache.get().is_none());
        assert!(!clone.snapshot().valid);
        assert!(monitor.cache.get().is_some());
        assert!(std::ptr::eq(monitor.samples(), clone.samples()));
    }

    #[test]
    fn snapshot_reports_reference_host_cpu_fields() {
        let mut sample = reading(10, None);
        sample.host.cpu_model = "Fixture CPU".into();
        sample.frequency_mhz = Some(2400.0);
        for name in ["cpu10", "cpu2"] {
            sample.cpu.push(CpuStat {
                name: name.into(),
                ..CpuStat::default()
            });
        }
        let mut samples = Samples::default();
        samples.accept(sample, Instant::now(), Utc::now());
        let json = serde_json::to_value(&samples.current).unwrap();
        assert_eq!(json["host"]["cpuModel"], "Fixture CPU");
        assert_eq!(json["cpu"]["frequencyMHz"], 2400.0);
        assert_eq!(json["cpu"]["cores"][0]["name"], "cpu2");
        assert_eq!(json["cpu"]["cores"][1]["name"], "cpu10");
    }

    #[test]
    fn cpu_delta_clamps_iowait_but_rejects_other_resets() {
        let old = CpuStat {
            user: 100,
            idle: 100,
            iowait: 20,
            ..CpuStat::default()
        };
        let mut current = CpuStat {
            user: 120,
            idle: 120,
            iowait: 10,
            ..CpuStat::default()
        };
        assert_eq!(cpu_delta(&old, &current), Some((50.0, 40)));
        current.user = 99;
        assert_eq!(cpu_delta(&old, &current), None);
    }

    #[test]
    fn network_aggregate_excludes_loopback_and_rejects_partial_rates() {
        let row = |name: &str, rate| Network {
            name: name.into(),
            rx_bytes: 0,
            tx_bytes: 0,
            rx_bytes_per_second: rate,
            tx_bytes_per_second: rate,
        };
        let rows = [row("eth0", Some(10.0)), row("lo", None), row("eth1", Some(20.0))];
        assert_eq!(aggregate_rate(&rows, |net| net.rx_bytes_per_second), Some(30.0));
        let rows = [row("eth0", Some(10.0)), row("eth1", None)];
        assert_eq!(aggregate_rate(&rows, |net| net.rx_bytes_per_second), None);
        assert_eq!(aggregate_rate(&[row("lo", Some(10.0))], |net| net.rx_bytes_per_second), None);
    }

    #[test]
    fn history_expires_and_resets_on_clock_regression() {
        let mut samples = Samples::default();
        let start = Instant::now();
        let at = Utc::now();
        samples.accept(reading(10, Some(10)), start, at);
        samples.accept(
            reading(20, Some(20)),
            start + Duration::from_secs(1000),
            at + chrono::Duration::seconds(1000),
        );
        assert_eq!(samples.current.history.len(), 1);
        samples.accept(
            reading(30, Some(30)),
            start + Duration::from_secs(1001),
            at,
        );
        assert_eq!(samples.current.history.len(), 1);
        assert_eq!(samples.current.networks[0].rx_bytes_per_second, None);
    }

    #[test]
    fn bounded_history_distinguishes_unavailable_and_reset_counters() {
        let mut samples = Samples::default();
        let start = Instant::now();
        samples.accept(reading(100, Some(100)), start, Utc::now());
        assert!(samples.current.cpu.usage.is_none());
        assert!(samples.current.networks[0].rx_bytes_per_second.is_none());
        samples.accept(
            reading(120, Some(140)),
            start + Duration::from_secs(2),
            Utc::now() + chrono::Duration::seconds(2),
        );
        assert_eq!(samples.current.cpu.usage, Some(50.0));
        assert_eq!(samples.current.networks[0].rx_bytes_per_second, Some(20.0));
        samples.accept(reading(1, None), start + Duration::from_secs(4), Utc::now() + chrono::Duration::seconds(4));
        assert!(samples.current.cpu.usage.is_none());
        assert!(samples.current.networks.is_empty());
        samples.accept(reading(3, Some(2)), start + Duration::from_secs(6), Utc::now() + chrono::Duration::seconds(6));
        assert!(samples.current.networks[0].rx_bytes_per_second.is_none());
        for index in 0..HISTORY_LIMIT {
            samples.accept(
                reading(10 + index as u64, None),
                start + Duration::from_secs(10 + index as u64),
                Utc::now() + chrono::Duration::seconds(10 + index as i64),
            );
        }
        assert_eq!(samples.current.history.len(), HISTORY_LIMIT);
        assert_eq!(
            samples.current.history.front().unwrap().memory_percent,
            75.0
        );
    }
}
