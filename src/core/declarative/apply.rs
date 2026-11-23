use anyhow::{Result, ensure};

use super::{ApplyPhase, Change, Config, Snapshot, Store, journal, parse, store};

/// A provider must verify ownership and readiness in preflight and recheck
/// ownership before mutating each resource.
pub trait ApplyProvider {
    fn preflight(
        &self,
        change: &Change,
        previous: &Config,
        desired: &Config,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
    fn apply(
        &self,
        change: &Change,
        previous: &Config,
        desired: &Config,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObservedResource {
    Before,
    After,
    Unknown,
}

/// Inspection must confirm ownership and the recorded resource fingerprint.
/// Unknown or partially applied states require explicit provider recovery.
pub trait RecoveryProvider: ApplyProvider {
    fn observe(
        &self,
        change: &Change,
        previous: &Config,
        desired: &Config,
    ) -> impl std::future::Future<Output = Result<ObservedResource>> + Send;
}

impl Store {
    /// Execute a reviewed plan. A failure leaves the journal in place for recovery.
    pub async fn apply_review(
        &self,
        bytes: &[u8],
        expected: &str,
        fingerprint: &str,
        provider: &impl ApplyProvider,
    ) -> Result<Snapshot> {
        let desired = parse(bytes)?;
        let store = self.clone();
        let directory = tokio::task::spawn_blocking(move || store.lock_directory()).await??;
        journal::ensure_idle(&directory)?;
        let (previous, plan) = Self::plan(store::current(&directory)?, bytes, expected, &desired)?;
        ensure!(
            plan.fingerprint == fingerprint,
            "reviewed plan no longer matches the candidate"
        );
        for change in &plan.changes {
            provider.preflight(change, &previous, &desired).await?;
        }
        let latest = store::current(&directory)?;
        if latest
            .as_ref()
            .map_or(super::MISSING_REVISION, |snapshot| &snapshot.revision)
            != expected
        {
            return Err(super::RevisionConflict.into());
        }
        let mut journal = journal::ApplyJournal::begin(&directory, plan, bytes)?;
        for change in &journal.plan.changes {
            provider.apply(change, &previous, &desired).await?;
            journal.completed += 1;
            journal.persist(&directory)?;
        }
        journal.phase = ApplyPhase::Publishing;
        journal.persist(&directory)?;
        let snapshot = Self::save_locked(&directory, bytes, expected, &desired)?;
        journal.phase = ApplyPhase::Complete;
        journal.persist(&directory)?;
        Ok(snapshot)
    }

    /// Resume only when the native state matches a reviewed before or after state.
    pub async fn resume_apply(
        &self,
        fingerprint: &str,
        provider: &impl RecoveryProvider,
    ) -> Result<Snapshot> {
        let store = self.clone();
        let directory = tokio::task::spawn_blocking(move || store.lock_directory()).await??;
        let mut journal = journal::read(&directory)?
            .ok_or_else(|| anyhow::anyhow!("no pending workload apply"))?;
        ensure!(
            journal.phase != ApplyPhase::Complete && journal.plan.fingerprint == fingerprint,
            "no matching pending workload apply"
        );

        let bytes = journal::candidate(&directory, &journal)?;
        let desired = parse(&bytes)?;
        let current = store::current(&directory)?;
        let current_revision = current
            .as_ref()
            .map_or(super::MISSING_REVISION, |data| &data.revision);
        let base = journal.plan.base_revision.clone();
        let published = journal.phase == ApplyPhase::Publishing
            && current_revision == journal.plan.desired_revision;
        ensure!(
            published || current_revision == base,
            "workload revision changed during recovery"
        );
        let base_snapshot = if current_revision == base {
            current
        } else if base == super::MISSING_REVISION {
            None
        } else {
            Some(self.read_history(&base)?)
        };
        let (previous, plan) = Self::plan(base_snapshot, &bytes, &base, &desired)?;
        ensure!(
            plan.fingerprint == fingerprint
                && journal.completed <= plan.changes.len()
                && (journal.phase == ApplyPhase::Applying
                    || journal.completed == plan.changes.len()),
            "pending journal does not match the reviewed plan"
        );
        journal.plan = plan;

        // Inspect every resource before doing new work, including completed
        // entries that could have drifted while the process was down.
        let mut observed = Vec::with_capacity(journal.plan.changes.len());
        for (index, change) in journal.plan.changes.iter().enumerate() {
            let state = provider.observe(change, &previous, &desired).await?;
            ensure!(
                state != ObservedResource::Unknown
                    && (index >= journal.completed || state == ObservedResource::After),
                "native state cannot be reconciled with the pending journal"
            );
            if state == ObservedResource::Before {
                provider.preflight(change, &previous, &desired).await?;
            }
            observed.push(state);
        }
        for (index, change) in journal
            .plan
            .changes
            .iter()
            .enumerate()
            .skip(journal.completed)
        {
            // Recheck after earlier changes. Apply itself verifies ownership
            // immediately before touching native state.
            let state = provider.observe(change, &previous, &desired).await?;
            ensure!(
                state == observed[index],
                "native state changed during recovery"
            );
            if state == ObservedResource::Before {
                provider.apply(change, &previous, &desired).await?;
                ensure!(
                    provider.observe(change, &previous, &desired).await? == ObservedResource::After,
                    "recovered resource does not match the reviewed state"
                );
            }
            journal.completed += 1;
            journal.persist(&directory)?;
        }
        journal.phase = ApplyPhase::Publishing;
        journal.persist(&directory)?;
        let snapshot = if published {
            let snapshot = store::current(&directory)?
                .ok_or_else(|| anyhow::anyhow!("published workload revision is missing"))?;
            ensure!(
                snapshot.revision == journal.plan.desired_revision,
                "published workload revision changed during recovery"
            );
            snapshot
        } else {
            Self::save_locked(&directory, &bytes, &base, &desired)?
        };
        journal.phase = ApplyPhase::Complete;
        journal.persist(&directory)?;
        Ok(snapshot)
    }
}

#[cfg(test)]
mod tests {
    use std::{os::unix::fs::PermissionsExt, sync::Mutex};

    use super::*;
    use crate::core::declarative::MISSING_REVISION;

    const YAML: &[u8] = b"version: 1\nservices:\n  a: {kind: container, containers: {app: {image: alpine}}}\n  b: {kind: container, depends_on: [a], containers: {app: {image: alpine}}}\n";

    struct FixtureProvider {
        path: std::path::PathBuf,
        fail_preflight: bool,
        fail_second: bool,
        unknown: bool,
        applied: Mutex<Vec<String>>,
    }

    impl ApplyProvider for FixtureProvider {
        async fn preflight(&self, _: &Change, _: &Config, _: &Config) -> Result<()> {
            ensure!(!self.fail_preflight, "provider is not ready");
            Ok(())
        }

        async fn apply(&self, change: &Change, _: &Config, _: &Config) -> Result<()> {
            let journal = Store::new(self.path.clone()).pending_apply()?.unwrap();
            ensure!(
                journal.phase == ApplyPhase::Applying,
                "journal was not durable before mutation"
            );
            let mut applied = self.applied.lock().unwrap();
            ensure!(
                journal.completed == applied.len(),
                "completed prefix was not recorded"
            );
            applied.push(change.resource.clone());
            ensure!(
                change.resource != "service/b" || !self.fail_second,
                "interrupted after mutation"
            );
            Ok(())
        }
    }

    impl RecoveryProvider for FixtureProvider {
        async fn observe(
            &self,
            change: &Change,
            _: &Config,
            _: &Config,
        ) -> Result<ObservedResource> {
            Ok(if self.unknown {
                ObservedResource::Unknown
            } else if self.applied.lock().unwrap().contains(&change.resource) {
                ObservedResource::After
            } else {
                ObservedResource::Before
            })
        }
    }

    #[tokio::test]
    async fn interrupted_apply_blocks_writes_and_retains_candidate() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let store = Store::new(dir.path().to_path_buf());
        let plan = store.preview(YAML, MISSING_REVISION).unwrap();
        let mut provider = FixtureProvider {
            path: dir.path().to_path_buf(),
            fail_preflight: true,
            fail_second: true,
            unknown: false,
            applied: Mutex::default(),
        };
        assert!(
            store
                .apply_review(YAML, MISSING_REVISION, &plan.fingerprint, &provider)
                .await
                .is_err()
        );
        assert!(store.pending_apply().unwrap().is_none());

        provider.fail_preflight = false;
        assert!(
            store
                .apply_review(YAML, MISSING_REVISION, &plan.fingerprint, &provider)
                .await
                .is_err()
        );
        let restarted = Store::new(dir.path().to_path_buf());
        let pending = restarted.pending_apply().unwrap().unwrap();
        assert_eq!(pending.phase, ApplyPhase::Applying);
        assert_eq!(pending.completed, 1);
        assert_eq!(
            *provider.applied.lock().unwrap(),
            ["service/a", "service/b"]
        );
        assert_eq!(
            restarted.pending_candidate(&plan.fingerprint).unwrap(),
            YAML
        );
        assert!(restarted.read().unwrap().is_none());
        assert!(restarted.save(YAML, MISSING_REVISION).is_err());
        assert!(
            restarted
                .apply_review(YAML, MISSING_REVISION, &plan.fingerprint, &provider)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn recovery_verifies_native_state_before_resuming_or_publishing() {
        for already_applied in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
            let store = Store::new(dir.path().to_path_buf());
            let plan = store.preview(YAML, MISSING_REVISION).unwrap();
            let mut provider = FixtureProvider {
                path: dir.path().to_path_buf(),
                fail_preflight: false,
                fail_second: true,
                unknown: false,
                applied: Mutex::default(),
            };
            assert!(
                store
                    .apply_review(YAML, MISSING_REVISION, &plan.fingerprint, &provider)
                    .await
                    .is_err()
            );
            if !already_applied {
                provider.applied.lock().unwrap().pop();
            }
            provider.fail_second = false;
            provider.unknown = true;
            assert!(
                store
                    .resume_apply(&plan.fingerprint, &provider)
                    .await
                    .is_err()
            );
            assert!(store.read().unwrap().is_none());
            assert_eq!(store.pending_apply().unwrap().unwrap().completed, 1);
            assert_eq!(
                provider.applied.lock().unwrap().len(),
                if already_applied { 2 } else { 1 }
            );

            provider.unknown = false;
            let finished = store
                .resume_apply(&plan.fingerprint, &provider)
                .await
                .unwrap();
            assert_eq!(finished.bytes, YAML);
            assert_eq!(
                *provider.applied.lock().unwrap(),
                ["service/a", "service/b"]
            );
            assert!(store.pending_apply().unwrap().is_none());
        }
    }

    #[tokio::test]
    async fn recovery_finalizes_a_revision_already_published_before_restart() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let store = Store::new(dir.path().to_path_buf());
        let base = store.save(b"version: 1\n", MISSING_REVISION).unwrap();
        let plan = store.preview(YAML, &base.revision).unwrap();
        let fingerprint = plan.fingerprint.clone();
        {
            let directory = store.lock_directory().unwrap();
            let mut journal = journal::ApplyJournal::begin(&directory, plan, YAML).unwrap();
            journal.completed = journal.plan.changes.len();
            journal.phase = ApplyPhase::Publishing;
            journal.persist(&directory).unwrap();
            Store::save_locked(&directory, YAML, &base.revision, &parse(YAML).unwrap()).unwrap();
        }
        let provider = FixtureProvider {
            path: dir.path().to_path_buf(),
            fail_preflight: true,
            fail_second: true,
            unknown: false,
            applied: Mutex::new(vec!["service/a".into(), "service/b".into()]),
        };
        assert_eq!(
            store
                .resume_apply(&fingerprint, &provider)
                .await
                .unwrap()
                .bytes,
            YAML
        );
        assert_eq!(provider.applied.lock().unwrap().len(), 2);
        assert!(store.pending_apply().unwrap().is_none());
    }

    #[tokio::test]
    async fn successful_apply_publishes_only_after_all_resources() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let store = Store::new(dir.path().to_path_buf());
        let plan = store.preview(YAML, MISSING_REVISION).unwrap();
        let provider = FixtureProvider {
            path: dir.path().to_path_buf(),
            fail_preflight: false,
            fail_second: false,
            unknown: false,
            applied: Mutex::default(),
        };
        assert!(
            store
                .apply_review(YAML, MISSING_REVISION, "wrong", &provider)
                .await
                .is_err()
        );
        let snapshot = store
            .apply_review(YAML, MISSING_REVISION, &plan.fingerprint, &provider)
            .await
            .unwrap();
        assert_eq!(snapshot.revision, plan.desired_revision);
        assert_eq!(store.read().unwrap().unwrap().bytes, YAML);
        assert!(store.pending_apply().unwrap().is_none());
    }
}
