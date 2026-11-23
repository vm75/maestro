//! Cached Linux resource samples shared across browser clients.

mod processes;
mod storage;
pub use processes::{ProcessList, Sort as ProcessSort};

use std::{
    collections::VecDeque,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::Serialize;

use crate::config::AppConfig;

use super::system_usage::{
    CpuStat, MemoryStat, NetworkStat, parse_proc_loadavg, parse_proc_meminfo, parse_proc_net_dev,
    parse_proc_stat, parse_proc_uptime,
};

struct Reading {
    cpu: Vec<CpuStat>,
    memory: MemoryStat,
    network: Option<NetworkStat>,
    uptime_seconds: Option<u64>,
    load_average: Option<[f64; 3]>,
    processes: Option<processes::Counters>,
    storage: storage::Storage,
}

impl Reading {
    fn collect(config: &AppConfig) -> anyhow::Result<Self> {
        let paths = &config.paths;
        let cpu = parse_proc_stat(&paths.proc_stat)?;
        let memory = parse_proc_meminfo(&paths.proc_meminfo)?;
        anyhow::ensure!(memory.total_kb > 0 && cpu.iter().any(|core| core.name == "cpu"));
        Ok(Self {
            cpu,
            memory,
            network: parse_proc_net_dev(&paths.proc_net_dev, &config.system.wan_interface).ok(),
            uptime_seconds: parse_proc_uptime(&paths.proc_uptime).ok(),
            load_average: parse_proc_loadavg(&paths.proc_loadavg)
                .ok()
                .filter(|load| load.iter().all(|value| value.is_finite())),
            processes: paths.proc_stat.parent().and_then(processes::Counters::read),
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
    cpu: Cpu,
    memory: Memory,
    uptime_seconds: Option<u64>,
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

#[derive(Clone, Default, Serialize)]
struct Cpu {
    usage: Option<f64>,
    cores: Vec<Core>,
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
                cpu: Cpu::default(),
                memory: Memory::default(),
                uptime_seconds: None,
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
                            samples.accept(usage, &config.system.wan_interface, Instant::now());
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
    fn accept(&mut self, mut usage: Reading, interface: &str, now: Instant) {
        let at = Utc::now();
        let previous = self.previous.as_ref();
        let cpu_usage = |cpu: &CpuStat| {
            previous.and_then(|(_, usage)| {
                usage
                    .cpu
                    .iter()
                    .find(|old| old.name == cpu.name)
                    .and_then(|old| cpu_delta(old, cpu))
            })
        };
        let cpu = Cpu {
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
        let total = usage.memory.total_kb.saturating_mul(1024);
        let available = usage.memory.available_kb.saturating_mul(1024).min(total);
        let memory = Memory {
            total,
            available,
            used: total - available,
            swap_total: usage.memory.swap_total_kb.saturating_mul(1024),
            swap_used: usage
                .memory
                .swap_total_kb
                .saturating_sub(usage.memory.swap_free_kb)
                .saturating_mul(1024),
        };
        let rate = |counter: u64, select: fn(&NetworkStat) -> u64| {
            let (sampled, old) = previous?;
            let elapsed = now.checked_duration_since(*sampled)?.as_secs_f64();
            if elapsed == 0.0 {
                return None;
            }
            Some(counter.checked_sub(select(old.network.as_ref()?))? as f64 / elapsed)
        };
        let network = usage.network.as_ref().map(|network| Network {
            name: interface.into(),
            rx_bytes: network.rx_bytes,
            tx_bytes: network.tx_bytes,
            rx_bytes_per_second: rate(network.rx_bytes, |old| old.rx_bytes),
            tx_bytes_per_second: rate(network.tx_bytes, |old| old.tx_bytes),
        });
        let history = &mut self.current.history;
        if history.len() == HISTORY_LIMIT {
            history.pop_front();
        }
        history.push_back(Point {
            at,
            cpu: cpu.usage,
            memory_percent: 100.0 * memory.used as f64 / total as f64,
            rx: network
                .as_ref()
                .and_then(|network| network.rx_bytes_per_second),
            tx: network
                .as_ref()
                .and_then(|network| network.tx_bytes_per_second),
        });
        self.current = Snapshot {
            valid: true,
            at: Some(at),
            interval_seconds: self.current.interval_seconds,
            cpu,
            memory,
            uptime_seconds: usage.uptime_seconds,
            load: usage.load_average,
            networks: network.into_iter().collect(),
            disk_io: usage.storage.rates(
                previous.map(|(at, sample)| (&sample.storage, now.saturating_duration_since(*at))),
            ),
            disks: std::mem::take(&mut usage.storage.disks),
            temperatures: std::mem::take(&mut usage.storage.temperatures),
            warnings: std::mem::take(&mut usage.storage.warnings),
            history: std::mem::take(history),
            last_error: None,
        };
        if let (Some(current), Some((sampled, previous))) = (&mut usage.processes, previous) {
            if let Some(previous) = &previous.processes {
                current.calculate(previous, now.saturating_duration_since(*sampled));
            }
        }
        self.previous = Some((now, usage));
    }
}

fn cpu_delta(old: &CpuStat, current: &CpuStat) -> Option<f64> {
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
    let idle = deltas.next()??.checked_add(deltas.next()??)?;
    let active = deltas.try_fold(0_u64, |total, delta| total.checked_add(delta?))?;
    let total = idle.checked_add(active)?;
    (total != 0).then(|| 100.0 * active as f64 / total as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(ticks: u64, network: Option<u64>) -> Reading {
        Reading {
            cpu: vec![CpuStat {
                name: "cpu".into(),
                user: ticks,
                idle: ticks,
                ..CpuStat::default()
            }],
            memory: MemoryStat {
                total_kb: 100,
                available_kb: 25,
                ..MemoryStat::default()
            },
            network: network.map(|rx_bytes| NetworkStat {
                rx_bytes,
                tx_bytes: 0,
            }),
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
    fn bounded_history_distinguishes_unavailable_and_reset_counters() {
        let mut samples = Samples::default();
        let start = Instant::now();
        samples.accept(reading(100, Some(100)), "eth0", start);
        assert!(samples.current.cpu.usage.is_none());
        assert!(samples.current.networks[0].rx_bytes_per_second.is_none());
        samples.accept(
            reading(120, Some(140)),
            "eth0",
            start + Duration::from_secs(2),
        );
        assert_eq!(samples.current.cpu.usage, Some(50.0));
        assert_eq!(samples.current.networks[0].rx_bytes_per_second, Some(20.0));
        samples.accept(reading(1, None), "eth0", start + Duration::from_secs(4));
        assert!(samples.current.cpu.usage.is_none());
        assert!(samples.current.networks.is_empty());
        samples.accept(reading(3, Some(2)), "eth0", start + Duration::from_secs(6));
        assert!(samples.current.networks[0].rx_bytes_per_second.is_none());
        for index in 0..HISTORY_LIMIT {
            samples.accept(
                reading(10 + index as u64, None),
                "eth0",
                start + Duration::from_secs(10 + index as u64),
            );
        }
        assert_eq!(samples.current.history.len(), HISTORY_LIMIT);
        assert_eq!(
            samples.current.history.front().unwrap().memory_percent,
            75.0
        );
    }
}
