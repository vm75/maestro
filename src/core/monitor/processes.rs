use std::os::unix::fs::OpenOptionsExt;
use std::{collections::BTreeMap, fs, io::Read, path::Path};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

const SCAN_LIMIT: usize = 4096;
const STAT_LIMIT: usize = 4096;

#[derive(Clone, Serialize)]
struct Process {
    pid: u32,
    name: String,
    state: char,
    cpu: Option<f64>,
    memory: u64,
    threads: u32,
    nice: i32,
}

struct Counter {
    process: Process,
    ticks: u64,
    start: u64,
}

pub(super) struct Counters {
    rows: BTreeMap<u32, Counter>,
    limited: bool,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Sort {
    #[default]
    Cpu,
    Memory,
    Pid,
    Name,
}

#[derive(Serialize)]
pub struct ProcessList {
    at: Option<DateTime<Utc>>,
    processes: Vec<Process>,
    matched: usize,
    limited: bool,
}

impl Counters {
    pub(super) fn read(proc_root: &Path) -> Option<Self> {
        let entries = fs::read_dir(proc_root).ok()?;
        let page_size = sysconf(libc::_SC_PAGESIZE)?;
        let mut rows = BTreeMap::new();
        let mut scanned = 0;
        let mut limited = false;
        for entry in entries.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
            else {
                continue;
            };
            if scanned == SCAN_LIMIT {
                limited = true;
                break;
            }
            scanned += 1;
            let mut bytes = Vec::new();
            let Ok(file) = fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(entry.path().join("stat"))
            else {
                continue;
            };
            if file
                .take(STAT_LIMIT as u64 + 1)
                .read_to_end(&mut bytes)
                .is_err()
                || bytes.len() > STAT_LIMIT
            {
                continue;
            }
            if let Some(row) = parse_stat(&String::from_utf8_lossy(&bytes), pid, page_size) {
                rows.insert(pid, row);
            }
        }
        Some(Self {
            rows,
            limited,
        })
    }

    pub(super) fn len(&self) -> usize {
        self.rows.len()
    }

    pub(super) fn limited(&self) -> bool {
        self.limited
    }

    // Match the reference sampler: use aggregate CPU jiffies rather than
    // assuming a fixed USER_HZ, and cap a process at the sampled core capacity.
    pub(super) fn calculate(&mut self, previous: &Self, cpu_ticks: u64, cores: usize) {
        if cpu_ticks == 0 {
            return;
        }
        for (pid, row) in &mut self.rows {
            row.process.cpu = previous
                .rows
                .get(pid)
                .filter(|old| old.start == row.start)
                .and_then(|old| row.ticks.checked_sub(old.ticks))
                .map(|delta| {
                    let capacity = 100.0 * cores as f64;
                    (capacity * delta as f64 / cpu_ticks as f64).min(capacity)
                });
        }
    }
}

pub(super) fn list(
    counters: Option<&Counters>,
    at: Option<DateTime<Utc>>,
    sort: Sort,
    filter: &str,
    limit: usize,
) -> ProcessList {
    let filter = filter.trim().to_lowercase();
    let mut rows: Vec<_> = counters
        .into_iter()
        .flat_map(|data| data.rows.values())
        .map(|row| &row.process)
        .filter(|row| {
            filter.is_empty()
                || row.name.to_lowercase().contains(&filter)
                || row.pid.to_string().contains(&filter)
        })
        .collect();
    rows.sort_unstable_by(|left, right| {
        let order = match sort {
            Sort::Cpu => right
                .cpu
                .unwrap_or(-1.0)
                .total_cmp(&left.cpu.unwrap_or(-1.0))
                .then_with(|| right.memory.cmp(&left.memory)),
            Sort::Memory => right.memory.cmp(&left.memory),
            Sort::Pid => left.pid.cmp(&right.pid),
            Sort::Name => left.name.to_lowercase().cmp(&right.name.to_lowercase()),
        };
        order.then_with(|| left.pid.cmp(&right.pid))
    });
    ProcessList {
        at: counters.and(at),
        matched: rows.len(),
        limited: counters.is_some_and(|data| data.limited),
        processes: rows.into_iter().take(limit).cloned().collect(),
    }
}

fn sysconf(key: libc::c_int) -> Option<u64> {
    // POSIX sysconf reads process-wide kernel constants.
    let value = unsafe { libc::sysconf(key) };
    (value > 0).then_some(value as u64)
}

fn parse_stat(text: &str, pid: u32, page_size: u64) -> Option<Counter> {
    let (left, fields) = text.rsplit_once(')')?;
    let (record_pid, name) = left.split_once(" (")?;
    if record_pid.parse::<u32>().ok()? != pid {
        return None;
    }
    let columns: Vec<_> = fields.split_whitespace().collect();
    let field = |index| columns.get(index).copied();
    let user = field(11)?.parse::<u64>().ok()?;
    let system = field(12)?.parse::<u64>().ok()?;
    let pages = field(21)?.parse::<i64>().ok()?.max(0) as u64;
    Some(Counter {
        process: Process {
            pid,
            name: name.into(),
            state: field(0)?.chars().next()?,
            cpu: None,
            memory: pages.saturating_mul(page_size),
            threads: field(17)?.parse().ok()?,
            nice: field(16)?.parse().ok()?,
        },
        ticks: user.checked_add(system)?,
        start: field(19)?.parse().ok()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_names_with_parentheses_and_recycled_pids() {
        let row = |name: &str, ticks, start| {
            parse_stat(
                &format!("42 ({name}) S 1 0 0 0 0 0 0 0 0 0 {ticks} 0 0 0 20 -2 3 0 {start} 100 5"),
                42,
                4096,
            )
            .unwrap()
        };
        let counters = |row| Counters {
            rows: BTreeMap::from([(42, row)]),
            limited: false,
        };
        let previous = counters(row("hello) world", 10, 1));
        let mut same = counters(row("hello) world", 210, 1));
        same.calculate(&previous, 400, 4);
        let data = list(Some(&same), Some(Utc::now()), Sort::Cpu, "WORLD", 1);
        assert_eq!(data.matched, 1);
        assert_eq!(data.processes[0].name, "hello) world");
        assert_eq!(data.processes[0].cpu, Some(200.0));
        assert_eq!(data.processes[0].memory, 20_480);
        assert_eq!(data.processes[0].nice, -2);
        assert_eq!(data.processes[0].threads, 3);
        let mut reused = counters(row("new process", 210, 2));
        reused.calculate(&previous, 400, 4);
        assert_eq!(reused.rows[&42].process.cpu, None);
        let mut excessive = counters(row("busy", 1010, 1));
        excessive.calculate(&previous, 400, 4);
        assert_eq!(excessive.rows[&42].process.cpu, Some(400.0));
        let mut reset = counters(row("reset", 1, 1));
        reset.calculate(&previous, 400, 4);
        assert_eq!(reset.rows[&42].process.cpu, None);
        let mut missing = counters(row("missing aggregate", 210, 1));
        missing.calculate(&previous, 0, 4);
        assert_eq!(missing.rows[&42].process.cpu, None);
    }
}
