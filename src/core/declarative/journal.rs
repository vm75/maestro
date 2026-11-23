use std::fs::File;

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use super::{Plan, Store, store};

const JOURNAL: &str = "apply-journal.json";
const CANDIDATE: &str = "apply-candidate.yaml";
const LIMIT: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplyPhase {
    Applying,
    Publishing,
    Complete,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApplyJournal {
    version: u8,
    pub plan: Plan,
    pub phase: ApplyPhase,
    pub completed: usize,
}

impl ApplyJournal {
    pub(super) fn begin(directory: &File, plan: Plan, candidate: &[u8]) -> Result<Self> {
        let journal = Self {
            version: 1,
            plan,
            phase: ApplyPhase::Applying,
            completed: 0,
        };
        store::publish(directory, CANDIDATE, candidate, false)?;
        journal.persist(directory)?;
        Ok(journal)
    }

    pub(super) fn persist(&self, directory: &File) -> Result<()> {
        let bytes = serde_json::to_vec(self)?;
        ensure!(bytes.len() <= LIMIT, "apply journal exceeds its limit");
        store::publish(directory, JOURNAL, &bytes, false)
    }
}

pub(super) fn read(directory: &File) -> Result<Option<ApplyJournal>> {
    let bytes = match store::read_private(directory, JOURNAL, LIMIT) {
        Ok(bytes) => bytes,
        Err(error)
            if error.downcast_ref::<rustix::io::Errno>() == Some(&rustix::io::Errno::NOENT) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let journal: ApplyJournal = serde_json::from_slice(&bytes)?;
    ensure!(
        journal.version == 1
            && journal.completed <= journal.plan.changes.len()
            && (journal.phase == ApplyPhase::Applying
                || journal.completed == journal.plan.changes.len()),
        "invalid apply journal"
    );
    Ok(Some(journal))
}

pub(super) fn ensure_idle(directory: &File) -> Result<()> {
    ensure!(
        read(directory)?.is_none_or(|journal| journal.phase == ApplyPhase::Complete),
        "workload apply is incomplete and requires recovery"
    );
    Ok(())
}

impl Store {
    /// Opening a store after restart reports incomplete operations without guessing
    /// whether the first unrecorded native change reached the host.
    pub fn pending_apply(&self) -> Result<Option<ApplyJournal>> {
        let directory = match self.directory() {
            Ok(directory) => directory,
            Err(error)
                if error.downcast_ref::<rustix::io::Errno>() == Some(&rustix::io::Errno::NOENT) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        Ok(read(&directory)?.filter(|journal| journal.phase != ApplyPhase::Complete))
    }

    /// Recover the exact bytes from a specific pending operation.
    pub fn pending_candidate(&self, fingerprint: &str) -> Result<Vec<u8>> {
        let directory = self.lock_directory()?;
        let journal =
            read(&directory)?.ok_or_else(|| anyhow::anyhow!("no pending workload apply"))?;
        ensure!(
            journal.phase != ApplyPhase::Complete && journal.plan.fingerprint == fingerprint,
            "no matching pending workload apply"
        );
        candidate(&directory, &journal)
    }
}

pub(super) fn candidate(directory: &File, journal: &ApplyJournal) -> Result<Vec<u8>> {
    let bytes = store::read_private(directory, CANDIDATE, super::MAX_CONFIG_BYTES)?;
    ensure!(
        super::revision(&bytes) == journal.plan.desired_revision,
        "pending candidate revision does not match its contents"
    );
    Ok(bytes)
}
