//! Management job lifecycle and durable audit records. An operation must finish cleanup before releasing its slot.

use std::{
    collections::VecDeque,
    fs::{File, OpenOptions},
    io::{self, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    sync::Arc,
};

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::watch;
use uuid::Uuid;

const HISTORY_LIMIT: usize = 64;
const OUTPUT_LIMIT: usize = 128 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub id: Uuid,
    pub tool: &'static str,
    pub state: JobState,
    pub cancellable: bool,
    pub cancellation_requested: bool,
    pub created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
    pub output: String,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
}

struct Entry {
    job: Job,
    cancel: watch::Sender<bool>,
}

#[derive(Clone)]
pub struct Jobs {
    entries: Arc<Mutex<VecDeque<Entry>>>,
    state_dir: Arc<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelRequest {
    Requested,
    NotFound,
    NotCancellable,
    Finished,
}

pub enum Outcome {
    Succeeded(Value),
    Failed(String),
    Cancelled,
}

/// A running operation holds this handle through execution and cleanup.
pub struct RunningJob {
    jobs: Jobs,
    id: Uuid,
    cancellation: watch::Receiver<bool>,
    _lock: File,
    audit: File,
}

impl Jobs {
    pub fn new(state_dir: PathBuf) -> Self {
        Self {
            entries: Arc::default(),
            state_dir: Arc::new(state_dir),
        }
    }

    pub fn start(&self, tool: &'static str) -> io::Result<Option<(Job, RunningJob)>> {
        let mut entries = self.entries.lock();
        if entries
            .front()
            .is_some_and(|entry| entry.job.state == JobState::Running)
        {
            return Ok(None);
        }
        let lock = open_private(&self.state_dir.join("management.lock"), false)?;
        // The lock stays open until the operation has finished its cleanup.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let failure = io::Error::last_os_error();
            return if failure.kind() == io::ErrorKind::WouldBlock {
                Ok(None)
            } else {
                Err(failure)
            };
        }
        let audit = open_private(&self.state_dir.join("management-audit.jsonl"), true)?;
        let job = Job {
            id: Uuid::new_v4(),
            tool,
            state: JobState::Running,
            cancellable: true,
            cancellation_requested: false,
            created_at: Utc::now(),
            finished_at: None,
            output: String::new(),
            truncated: false,
            error: None,
            result: None,
        };
        audit_record(&audit, &job)?;
        let (cancel, cancellation) = watch::channel(false);
        entries.push_front(Entry {
            job: job.clone(),
            cancel,
        });
        entries.truncate(HISTORY_LIMIT);
        Ok(Some((
            job.clone(),
            RunningJob {
                jobs: self.clone(),
                id: job.id,
                cancellation,
                _lock: lock,
                audit,
            },
        )))
    }

    pub fn list(&self) -> Vec<Job> {
        self.entries
            .lock()
            .iter()
            .map(|entry| Job {
                output: String::new(),
                result: None,
                ..entry.job.clone()
            })
            .collect()
    }

    pub fn get(&self, id: Uuid) -> Option<Job> {
        self.entries
            .lock()
            .iter()
            .find(|entry| entry.job.id == id)
            .map(|entry| entry.job.clone())
    }

    pub fn request_cancel(&self, id: Uuid) -> CancelRequest {
        let mut entries = self.entries.lock();
        let Some(entry) = entries.iter_mut().find(|entry| entry.job.id == id) else {
            return CancelRequest::NotFound;
        };
        if entry.job.state != JobState::Running {
            return CancelRequest::Finished;
        }
        if !entry.job.cancellable {
            return CancelRequest::NotCancellable;
        }
        entry.job.cancellation_requested = true;
        entry.cancel.send_replace(true);
        CancelRequest::Requested
    }

    #[cfg(test)]
    fn cancel(&self, id: Uuid) -> bool {
        matches!(
            self.request_cancel(id),
            CancelRequest::Requested | CancelRequest::Finished
        )
    }
}

impl RunningJob {
    /// Atomically close the cancellation window before an irreversible mutation.
    /// False means cancellation won the race; the caller must finish Cancelled.
    pub fn begin_mutation(&self) -> bool {
        let mut entries = self.jobs.entries.lock();
        let Some(entry) = entries
            .iter_mut()
            .find(|entry| entry.job.id == self.id && entry.job.state == JobState::Running)
        else {
            return false;
        };
        if entry.job.cancellation_requested {
            return false;
        }
        entry.job.cancellable = false;
        true
    }

    pub fn cancellation(&self) -> watch::Receiver<bool> {
        self.cancellation.clone()
    }

    pub fn append_output(&self, text: &str) {
        let mut entries = self.jobs.entries.lock();
        let Some(entry) = entries.iter_mut().find(|entry| entry.job.id == self.id) else {
            return;
        };
        let job = &mut entry.job;
        if text.len() >= OUTPUT_LIMIT {
            job.truncated |= !job.output.is_empty() || text.len() > OUTPUT_LIMIT;
            job.output.clear();
            job.output
                .push_str(&text[boundary_at_or_after(text, text.len() - OUTPUT_LIMIT)..]);
            return;
        }
        let excess = (job.output.len() + text.len()).saturating_sub(OUTPUT_LIMIT);
        if excess > 0 {
            job.output
                .drain(..boundary_at_or_after(&job.output, excess));
            job.truncated = true;
        }
        job.output.push_str(text);
    }

    pub fn finish(self, outcome: Outcome) {
        self.complete(outcome);
    }

    fn complete(&self, outcome: Outcome) {
        let mut entries = self.jobs.entries.lock();
        let Some(entry) = entries
            .iter_mut()
            .find(|entry| entry.job.id == self.id && entry.job.state == JobState::Running)
        else {
            return;
        };
        let job = &mut entry.job;
        job.finished_at = Some(Utc::now());
        job.cancellable = false;
        job.state = match outcome {
            Outcome::Succeeded(result) => {
                job.result = Some(result);
                JobState::Succeeded
            }
            Outcome::Failed(message) => {
                job.error = Some(message);
                JobState::Failed
            }
            Outcome::Cancelled => JobState::Cancelled,
        };
        if let Err(error) = audit_record(&self.audit, job) {
            tracing::error!(%error, job_id = %job.id, "failed to audit management job completion");
        }
        unsafe {
            libc::flock(self._lock.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

fn open_private(path: &Path, append: bool) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(!append)
        .write(true)
        .append(append)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let info = file.metadata()?;
    if !info.is_file()
        || info.nlink() != 1
        || info.mode() & 0o077 != 0
        || info.uid() != unsafe { libc::geteuid() }
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "job state file must be an owner-only regular file",
        ));
    }
    Ok(file)
}

fn audit_record(mut audit: &File, job: &Job) -> io::Result<()> {
    let mut record = serde_json::to_vec(&serde_json::json!({
        "id": job.id,
        "tool": job.tool,
        "state": job.state,
        "time": job.finished_at.unwrap_or(job.created_at),
    }))?;
    record.push(b'\n');
    audit.write_all(&record)?;
    audit.sync_data()
}

fn boundary_at_or_after(text: &str, mut offset: usize) -> usize {
    while !text.is_char_boundary(offset) {
        offset += 1;
    }
    offset
}

impl Drop for RunningJob {
    fn drop(&mut self) {
        self.complete(Outcome::Failed(
            "Management operation ended without a result.".into(),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancellation_keeps_slot_until_cleanup_and_retains_bounded_output() {
        let directory = tempfile::tempdir().unwrap();
        let jobs = Jobs::new(directory.path().to_path_buf());
        let (job, operation) = jobs.start("test").unwrap().unwrap();
        let mut cancellation = operation.cancellation();
        assert!(jobs.cancel(job.id));
        cancellation.changed().await.unwrap();
        assert!(*cancellation.borrow());
        assert!(jobs.start("other").unwrap().is_none());
        operation.append_output(&"🦀".repeat(OUTPUT_LIMIT / 4));
        operation.append_output("done");
        let live = jobs.get(job.id).unwrap();
        assert!(live.truncated);
        assert!(live.output.ends_with("done"));
        assert!(live.output.len() <= OUTPUT_LIMIT);
        assert!(jobs.list()[0].output.is_empty());
        operation.finish(Outcome::Cancelled);
        assert_eq!(jobs.get(job.id).unwrap().state, JobState::Cancelled);
        let (success, operation) = jobs.start("success").unwrap().unwrap();
        operation.finish(Outcome::Succeeded(serde_json::json!({"ready": true})));
        assert_eq!(jobs.get(success.id).unwrap().result.unwrap()["ready"], true);
        assert_eq!(jobs.list()[0].id, success.id);
        assert!(jobs.list()[0].result.is_none());
        let (next, operation) = jobs.start("other").unwrap().unwrap();
        drop(operation);
        assert_eq!(jobs.get(next.id).unwrap().state, JobState::Failed);
    }

    #[test]
    fn cancellation_and_mutation_boundary_are_atomic() {
        let directory = tempfile::tempdir().unwrap();
        let jobs = Jobs::new(directory.path().to_path_buf());
        let (job, running) = jobs.start("native").unwrap().unwrap();
        assert_eq!(jobs.request_cancel(job.id), CancelRequest::Requested);
        assert!(!running.begin_mutation());
        assert!(jobs.get(job.id).unwrap().cancellation_requested);
        assert!(jobs.start("other").unwrap().is_none());
        running.finish(Outcome::Cancelled);
        assert_eq!(jobs.request_cancel(job.id), CancelRequest::Finished);

        let (job, running) = jobs.start("native").unwrap().unwrap();
        assert!(running.begin_mutation());
        assert!(!jobs.get(job.id).unwrap().cancellable);
        assert_eq!(jobs.request_cancel(job.id), CancelRequest::NotCancellable);
        assert!(!*running.cancellation().borrow());
        running.finish(Outcome::Succeeded(Value::Null));
    }

    #[test]
    fn history_evicts_oldest_finished_jobs() {
        let directory = tempfile::tempdir().unwrap();
        let jobs = Jobs::new(directory.path().to_path_buf());
        let (oldest, operation) = jobs.start("oldest").unwrap().unwrap();
        operation.finish(Outcome::Succeeded(Value::Null));
        for _ in 0..HISTORY_LIMIT {
            let (_, operation) = jobs.start("new").unwrap().unwrap();
            operation.finish(Outcome::Succeeded(Value::Null));
        }
        assert_eq!(jobs.list().len(), HISTORY_LIMIT);
        assert!(jobs.get(oldest.id).is_none());
        assert!(!jobs.cancel(oldest.id));
    }

    #[test]
    fn instances_share_a_lock_and_audit_lifecycle() {
        let directory = tempfile::tempdir().unwrap();
        let first = Jobs::new(directory.path().to_path_buf());
        let second = Jobs::new(directory.path().to_path_buf());
        let (job, running) = first.start("first").unwrap().unwrap();
        assert!(second.start("second").unwrap().is_none());
        running.finish(Outcome::Succeeded(Value::Null));
        let (_, running) = second.start("second").unwrap().unwrap();
        running.finish(Outcome::Cancelled);

        let audit =
            std::fs::read_to_string(directory.path().join("management-audit.jsonl")).unwrap();
        let records: Vec<Value> = audit
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(records.len(), 4);
        assert_eq!(records[0]["id"], job.id.to_string());
        assert_eq!(records[0]["state"], "running");
        assert_eq!(records[1]["state"], "succeeded");
        assert_eq!(records[2]["state"], "running");
        assert_eq!(records[3]["state"], "cancelled");
    }
}
