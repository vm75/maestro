use std::fs::File;

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use super::{ApplyMode, Plan, Store, store};

const JOURNAL: &str = "apply-journal.json";
const CANDIDATE: &str = "apply-candidate.yaml";
const LIMIT: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplyPhase {
    Prepared,
    Applying,
    Publishing,
    Complete,
    Aborted,
}

impl ApplyPhase {
    pub(super) fn is_terminal(self) -> bool {
        matches!(self, Self::Complete | Self::Aborted)
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApplyJournal {
    pub(super) version: u8,
    #[serde(default = "default_authority")]
    pub(super) authority: String,
    pub plan: Plan,
    pub phase: ApplyPhase,
    pub completed: usize,
    #[serde(default)]
    pub mode: ApplyMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) failed_resource: Option<String>,
    #[serde(default,skip_serializing_if="std::ops::Not::not")]
    pub(super) artifact_conflict: bool,
    #[serde(default)]
    pub(super) updated_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) superseded: Option<Box<ApplyJournal>>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(super) alternate_candidate: bool,
}

fn default_authority() -> String {
    store::CURRENT.into()
}

impl ApplyJournal {
    #[cfg(test)]
    pub(super) fn begin(
        directory: &File,
        authority: &str,
        plan: Plan,
        candidate: &[u8],
    ) -> Result<Self> {
        Self::begin_replacing(directory, authority, plan, candidate, None)
    }

    #[cfg(test)]
    pub(super) fn begin_replacing(
        directory: &File,
        authority: &str,
        plan: Plan,
        candidate: &[u8],
        superseded: Option<ApplyJournal>,
    ) -> Result<Self> {
        Self::begin_replacing_mode(
            directory,
            authority,
            plan,
            candidate,
            superseded,
            ApplyMode::Now,
        )
    }

    pub(super) fn begin_replacing_mode(
        directory: &File,
        authority: &str,
        plan: Plan,
        candidate: &[u8],
        superseded: Option<ApplyJournal>,
        mode: ApplyMode,
    ) -> Result<Self> {
        let alternate_candidate = superseded
            .as_ref()
            .is_some_and(|prior| !prior.alternate_candidate);
        let journal = Self {
            version: 6,
            authority: authority.into(),
            plan,
            phase: ApplyPhase::Prepared,
            completed: 0,
            mode,
            failed_resource: None,
            artifact_conflict: false,
            updated_at: Some(chrono::Utc::now()),
            superseded: superseded.map(Box::new),
            alternate_candidate,
        };
        validate(&journal)?;
        // Use the other bounded slot when replacing pending intent. A crash
        // before journal publication must leave the old candidate recoverable.
        store::publish(directory, journal.candidate_name(), candidate, false)?;
        journal.persist(directory)?;
        Ok(journal)
    }

    pub(super) fn failed(&mut self, directory: &File, resource: String, error:&anyhow::Error) -> Result<()> {
        // Preserve legacy publication ordering while recording recovery failure.
        self.failed_resource = Some(resource);
        self.artifact_conflict=super::artifact_recovery::is_conflict(error);
        self.persist(directory)
    }

    fn candidate_name(&self) -> &'static str {
        if self.alternate_candidate {
            "apply-candidate-next.yaml"
        } else {
            CANDIDATE
        }
    }

    pub(super) fn persist(&self, directory: &File) -> Result<()> {
        validate(self)?;
        let mut document = serde_json::to_value(self)?;
        document["updatedAt"] = serde_json::to_value(chrono::Utc::now())?;
        let bytes = serde_json::to_vec(&document)?;
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
    validate(&journal)?;
    Ok(Some(journal))
}

fn validate(journal: &ApplyJournal) -> Result<()> {
    ensure!(
        matches!(journal.version, 1..=6)
            && (journal.version >= 4 || journal.mode == ApplyMode::Now)
            && (journal.version >= 3
                || (!journal.alternate_candidate && journal.superseded.is_none()))
            && journal.completed <= journal.plan.changes.len()
            && match journal.phase {
                ApplyPhase::Prepared => journal.version >= 2 && journal.completed == 0,
                ApplyPhase::Applying => true,
                ApplyPhase::Aborted => journal.version >= 6 && journal.completed == 0,
                ApplyPhase::Publishing | ApplyPhase::Complete =>
                    journal.completed == journal.plan.changes.len(),
            },
        "invalid apply journal"
    );
    ensure!(!journal.artifact_conflict||journal.failed_resource.is_some(),"invalid artifact conflict journal marker");
    if let Some(failed) = &journal.failed_resource {
        ensure!(
            journal.phase == ApplyPhase::Applying
                && journal
                    .plan
                    .changes
                    .get(journal.completed)
                    .is_some_and(|change| &change.resource == failed),
            "invalid failed resource in apply journal"
        );
    }
    if let Some(prior) = &journal.superseded {
        ensure!(
            journal.phase == ApplyPhase::Prepared
                && prior.phase != ApplyPhase::Prepared
                && !prior.phase.is_terminal()
                && prior.superseded.is_none()
                && prior.authority == journal.authority
                && prior.plan.desired_revision == journal.plan.base_revision
                && prior.alternate_candidate != journal.alternate_candidate,
            "invalid superseded journal"
        );
        validate(prior)?;
    }
    Ok(())
}

fn status(journal: &ApplyJournal) -> serde_json::Value {
    let mut resources = serde_json::Map::new();
    for (index, change) in journal.plan.changes.iter().enumerate() {
        let failed = journal.failed_resource.as_deref() == Some(change.resource.as_str());
        let state = if index < journal.completed {
            "registered"
        } else if failed {
            "failed"
        } else if index == journal.completed && journal.phase == ApplyPhase::Applying {
            "applying"
        } else {
            "pending"
        };
        let mut value = serde_json::json!({"fingerprint":change.after.as_ref().or(change.before.as_ref()),"state":state});
        if failed {
            value["message"] = serde_json::json!(if journal.artifact_conflict {
                super::artifact_recovery::CONFLICT_REMEDY
            } else {"Native publication or verification failed; inspect backend state before retry."});
            if journal.artifact_conflict{value["artifactRecovery"]=serde_json::json!(true);}
        }
        resources.insert(change.resource.clone(), value);
    }
    let stage = if journal.failed_resource.is_some() {
        "failed"
    } else {
        match journal.phase {
            ApplyPhase::Prepared => "prepared",
            ApplyPhase::Applying => "applying",
            ApplyPhase::Publishing => "applying",
            ApplyPhase::Complete => "complete",
            ApplyPhase::Aborted => "aborted",
        }
    };
    let mut status = serde_json::json!({"version":journal.version,
            "baseRevision":journal.plan.base_revision,"desiredRevision":journal.plan.desired_revision,
            "stage":stage,"mode":journal.mode,"updatedAt":journal.updated_at,
            "resources":resources,"fingerprint":journal.plan.fingerprint});
    if let Some(prior) = &journal.superseded {
        status["superseded"] = self::status(prior);
    }
    status
}

pub(super) fn ensure_idle(directory: &File) -> Result<()> {
    ensure!(
        read(directory)?.is_none_or(|journal| journal.phase.is_terminal()),
        "workload apply is incomplete and requires recovery"
    );
    Ok(())
}

impl Store {
    /// Read durable metadata only. This never invokes a native provider.
    pub fn apply_status(&self) -> Result<Option<serde_json::Value>> {
        let directory = match self.directory() {
            Ok(directory) => directory,
            Err(error)
                if error.downcast_ref::<rustix::io::Errno>() == Some(&rustix::io::Errno::NOENT) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let Some(journal) = read(&directory)? else {
            return Ok(None);
        };
        ensure!(
            journal.authority == self.filename,
            "apply journal belongs to a different configuration"
        );
        Ok(Some(status(&journal)))
    }

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
        let pending = read(&directory)?.filter(|journal| !journal.phase.is_terminal());
        if let Some(journal) = &pending {
            ensure!(
                journal.authority == self.filename,
                "pending apply belongs to a different configuration"
            );
        }
        Ok(pending)
    }

    /// Recover the exact bytes from a specific pending operation.
    pub fn pending_candidate(&self, fingerprint: &str) -> Result<Vec<u8>> {
        let directory = self.lock_directory()?;
        let journal =
            read(&directory)?.ok_or_else(|| anyhow::anyhow!("no pending workload apply"))?;
        ensure!(
            !journal.phase.is_terminal() && journal.plan.fingerprint == fingerprint,
            "no matching pending workload apply"
        );
        ensure!(
            journal.authority == self.filename,
            "pending apply belongs to a different configuration"
        );
        candidate(&directory, &journal)
    }
}

pub(super) fn candidate(directory: &File, journal: &ApplyJournal) -> Result<Vec<u8>> {
    let bytes = store::read_private(directory, journal.candidate_name(), super::MAX_CONFIG_BYTES)?;
    ensure!(
        super::revision(&bytes) == journal.plan.desired_revision,
        "pending candidate revision does not match its contents"
    );
    Ok(bytes)
}
