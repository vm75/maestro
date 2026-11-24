use anyhow::{Result, ensure};

use super::{ApplyPhase, Change, Config, Snapshot, Store, journal, parse, store};

/// A provider must verify ownership and readiness in preflight and recheck
/// ownership before mutating each resource.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplyMode {
    #[default]
    Now,
    NextStart,
}

pub trait ApplyProvider: Sync {
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
    fn preflight_mode(
        &self,
        change: &Change,
        previous: &Config,
        desired: &Config,
        mode: ApplyMode,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        async move {
            ensure!(
                mode == ApplyMode::Now,
                "provider does not support next-start publication"
            );
            self.preflight(change, previous, desired).await
        }
    }
    fn apply_mode(
        &self,
        change: &Change,
        previous: &Config,
        desired: &Config,
        mode: ApplyMode,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        async move {
            ensure!(
                mode == ApplyMode::Now,
                "provider does not support next-start publication"
            );
            self.apply(change, previous, desired).await
        }
    }
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
    fn observe_mode(
        &self,
        change: &Change,
        previous: &Config,
        desired: &Config,
        mode: ApplyMode,
    ) -> impl std::future::Future<Output = Result<ObservedResource>> + Send {
        async move {
            ensure!(
                mode == ApplyMode::Now,
                "provider does not support next-start inspection"
            );
            self.observe(change, previous, desired).await
        }
    }
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
        self.apply_review_mode(bytes, expected, fingerprint, ApplyMode::Now, provider)
            .await
    }

    pub async fn apply_review_mode(
        &self,
        bytes: &[u8],
        expected: &str,
        fingerprint: &str,
        mode: ApplyMode,
        provider: &impl ApplyProvider,
    ) -> Result<Snapshot> {
        let desired = parse(bytes)?;
        let store = self.clone();
        let directory = tokio::task::spawn_blocking(move || store.lock_directory()).await??;
        super::backup_capture::ensure_idle_locked(self,&directory)?;
        journal::ensure_idle(&directory)?;
        let (previous, plan) = Self::plan(
            store::current(&directory, &self.filename)?,
            bytes,
            expected,
            &desired,
        )?;
        ensure!(
            plan.fingerprint == fingerprint,
            "reviewed plan no longer matches the candidate"
        );
        for change in &plan.changes {
            provider
                .preflight_mode(change, &previous, &desired, mode)
                .await?;
        }
        let latest = store::current(&directory, &self.filename)?;
        if latest
            .as_ref()
            .map_or(super::MISSING_REVISION, |snapshot| &snapshot.revision)
            != expected
        {
            return Err(super::RevisionConflict.into());
        }
        let mut journal = journal::ApplyJournal::begin_replacing_mode(
            &directory,
            &self.filename,
            plan,
            bytes,
            None,
            mode,
        )?;
        let snapshot = self.save_locked(&directory, bytes, expected, &desired)?;
        journal.phase = ApplyPhase::Applying;
        journal.persist(&directory)?;
        for change in &journal.plan.changes {
            if let Err(error) = provider.apply_mode(change, &previous, &desired, mode).await {
                let resource = change.resource.clone();
                journal.failed(&directory, resource)?;
                return Err(error);
            }
            journal.completed += 1;
            journal.persist(&directory)?;
        }
        journal.phase = ApplyPhase::Publishing;
        journal.persist(&directory)?;
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
        self.resume_apply_inner(fingerprint, provider, true)
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!("explicit resume did not produce a configuration snapshot")
            })
    }

    /// Automatic retry never repeats a removal, even if its prior definition
    /// remains fully owned. An operator must inspect that resource explicitly.
    pub async fn retry_apply(&self, provider: &impl RecoveryProvider) -> Result<serde_json::Value> {
        if let Some(pending) = self.pending_apply()? {
            self.resume_apply_inner(&pending.plan.fingerprint, provider, false)
                .await?;
        }
        self.apply_status()?
            .ok_or_else(|| anyhow::anyhow!("no recorded configuration apply"))
    }

    async fn resume_apply_inner(
        &self,
        fingerprint: &str,
        provider: &impl RecoveryProvider,
        allow_removals: bool,
    ) -> Result<Option<Snapshot>> {
        let store = self.clone();
        let directory = tokio::task::spawn_blocking(move || store.lock_directory()).await??;
        super::backup_capture::ensure_idle_locked(self,&directory)?;
        let mut journal = journal::read(&directory)?
            .ok_or_else(|| anyhow::anyhow!("no pending workload apply"))?;
        ensure!(
            !journal.phase.is_terminal() && journal.plan.fingerprint == fingerprint,
            "no matching pending workload apply"
        );

        ensure!(
            journal.authority == self.filename,
            "pending apply belongs to a different configuration"
        );
        let current = store::current(&directory, &self.filename)?;
        let current_revision = current
            .as_ref()
            .map_or(super::MISSING_REVISION, |data| &data.revision);
        let base = journal.plan.base_revision.clone();
        if journal.phase == ApplyPhase::Prepared && current_revision == base {
            if let Some(prior) = journal.superseded.take() {
                // The replacement intent reached disk but its desired YAML did
                // not. Restore the exact prior transition and candidate slot.
                prior.persist(&directory)?;
                return current
                    .map(Some)
                    .ok_or_else(|| anyhow::anyhow!("superseded configuration is missing"));
            }
            if !allow_removals {
                // Prepared intent alone is not authorization to publish it on
                // automatic retry. No native changes preceded YAML commit.
                journal.version = 6;
                journal.phase = ApplyPhase::Aborted;
                journal.persist(&directory)?;
                return Ok(current);
            }
        }
        let bytes = journal::candidate(&directory, &journal)?;
        let desired = parse(&bytes)?;
        let published = current_revision == journal.plan.desired_revision
            && (journal.version >= 2 || journal.phase == ApplyPhase::Publishing);
        ensure!(
            published
                || (current_revision == base
                    && (journal.version == 1 || journal.phase == ApplyPhase::Prepared)),
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
                && (matches!(journal.phase, ApplyPhase::Applying | ApplyPhase::Prepared)
                    || journal.completed == plan.changes.len()),
            "pending journal does not match the reviewed plan"
        );
        journal.plan = plan;

        // Inspect every resource before doing new work, including completed
        // entries that could have drifted while the process was down.
        let mut observed = Vec::with_capacity(journal.plan.changes.len());
        for (index, change) in journal.plan.changes.iter().enumerate() {
            let state = provider
                .observe_mode(change, &previous, &desired, journal.mode)
                .await?;
            ensure!(
                state != ObservedResource::Unknown
                    && (index >= journal.completed || state == ObservedResource::After),
                "native state cannot be reconciled with the pending journal"
            );
            ensure!(
                allow_removals
                    || change.operation != super::Operation::Remove
                    || state == ObservedResource::After,
                "interrupted removal requires explicit inspection; automatic destructive replay refused"
            );
            if state == ObservedResource::Before {
                provider
                    .preflight_mode(change, &previous, &desired, journal.mode)
                    .await?;
            }
            observed.push(state);
        }
        if journal.version >= 2 && !published {
            self.save_locked(&directory, &bytes, &base, &desired)?;
        }
        journal.failed_resource = None;
        if journal.phase == ApplyPhase::Prepared {
            journal.phase = ApplyPhase::Applying;
            journal.superseded = None;
            journal.persist(&directory)?;
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
            let state = provider
                .observe_mode(change, &previous, &desired, journal.mode)
                .await?;
            ensure!(
                state == observed[index],
                "native state changed during recovery"
            );
            let result = async {
                if state == ObservedResource::Before {
                    provider
                        .apply_mode(change, &previous, &desired, journal.mode)
                        .await?;
                    ensure!(
                        provider
                            .observe_mode(change, &previous, &desired, journal.mode)
                            .await?
                            == ObservedResource::After,
                        "recovered resource does not match the reviewed state"
                    );
                }
                Ok::<_, anyhow::Error>(())
            }
            .await;
            if let Err(error) = result {
                let resource = change.resource.clone();
                journal.failed(&directory, resource)?;
                return Err(error);
            }
            journal.completed += 1;
            journal.persist(&directory)?;
        }
        journal.phase = ApplyPhase::Publishing;
        journal.persist(&directory)?;
        let snapshot = if published || journal.version >= 2 {
            let snapshot = store::current(&directory, &self.filename)?
                .ok_or_else(|| anyhow::anyhow!("published workload revision is missing"))?;
            ensure!(
                snapshot.revision == journal.plan.desired_revision,
                "published workload revision changed during recovery"
            );
            snapshot
        } else {
            self.save_locked(&directory, &bytes, &base, &desired)?
        };
        journal.phase = ApplyPhase::Complete;
        journal.persist(&directory)?;
        Ok(Some(snapshot))
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
            let store = Store::new(self.path.clone());
            let journal = store.pending_apply()?.unwrap();
            if journal.version >= 2 {
                ensure!(
                    store
                        .read()?
                        .is_some_and(|snapshot| snapshot.revision == journal.plan.desired_revision),
                    "desired YAML was not durable before native mutation"
                );
            }
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

    struct DeferredProvider(Mutex<Vec<String>>);
    impl ApplyProvider for DeferredProvider {
        async fn preflight(&self, _: &Change, _: &Config, _: &Config) -> Result<()> {
            anyhow::bail!("now preflight must not run during deferred publication")
        }
        async fn apply(&self, _: &Change, _: &Config, _: &Config) -> Result<()> {
            anyhow::bail!("now apply must not run during deferred publication")
        }
        async fn preflight_mode(
            &self,
            _: &Change,
            _: &Config,
            _: &Config,
            mode: ApplyMode,
        ) -> Result<()> {
            ensure!(
                mode == ApplyMode::NextStart,
                "deferred mode changed during recovery"
            );
            Ok(())
        }
        async fn apply_mode(
            &self,
            change: &Change,
            _: &Config,
            _: &Config,
            mode: ApplyMode,
        ) -> Result<()> {
            ensure!(
                mode == ApplyMode::NextStart,
                "deferred mode changed during recovery"
            );
            self.0.lock().unwrap().push(change.resource.clone());
            ensure!(
                change.resource != "service/b",
                "interruption after deferred publication"
            );
            Ok(())
        }
    }
    impl RecoveryProvider for DeferredProvider {
        async fn observe(&self, _: &Change, _: &Config, _: &Config) -> Result<ObservedResource> {
            anyhow::bail!("now inspection must not run during deferred recovery")
        }
        async fn observe_mode(
            &self,
            change: &Change,
            _: &Config,
            _: &Config,
            mode: ApplyMode,
        ) -> Result<ObservedResource> {
            ensure!(
                mode == ApplyMode::NextStart,
                "deferred mode changed during recovery"
            );
            Ok(if self.0.lock().unwrap().contains(&change.resource) {
                ObservedResource::After
            } else {
                ObservedResource::Before
            })
        }
    }

    #[tokio::test]
    async fn recovery_preserves_next_start_mode_and_does_not_repeat_published_resources() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let store = Store::new(dir.path().to_path_buf());
        let plan = store.preview(YAML, MISSING_REVISION).unwrap();
        let provider = DeferredProvider(Mutex::default());
        assert!(
            store
                .apply_review_mode(
                    YAML,
                    MISSING_REVISION,
                    &plan.fingerprint,
                    ApplyMode::NextStart,
                    &provider
                )
                .await
                .is_err()
        );
        let pending = store.pending_apply().unwrap().unwrap();
        assert_eq!(pending.mode, ApplyMode::NextStart);
        assert_eq!(pending.completed, 1);
        assert_eq!(store.read().unwrap().unwrap().bytes, YAML);
        let restarted = Store::new(dir.path().to_path_buf());
        assert_eq!(
            restarted
                .resume_apply(&plan.fingerprint, &provider)
                .await
                .unwrap()
                .bytes,
            YAML
        );
        assert_eq!(provider.0.lock().unwrap().len(), 2);
        assert!(store.pending_apply().unwrap().is_none());
    }

    #[tokio::test]
    async fn unsupported_deferred_provider_is_rejected_before_intent_or_yaml_publication() {
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
                .apply_review_mode(
                    YAML,
                    MISSING_REVISION,
                    &plan.fingerprint,
                    ApplyMode::NextStart,
                    &provider
                )
                .await
                .is_err()
        );
        assert!(store.read().unwrap().is_none());
        assert!(store.pending_apply().unwrap().is_none());
        assert!(provider.applied.lock().unwrap().is_empty());
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
        assert_eq!(restarted.read().unwrap().unwrap().bytes, YAML);
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
            assert_eq!(store.read().unwrap().unwrap().bytes, YAML);
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
            let mut journal =
                journal::ApplyJournal::begin(&directory, &store.filename, plan, YAML).unwrap();
            journal.completed = journal.plan.changes.len();
            journal.phase = ApplyPhase::Publishing;
            journal.persist(&directory).unwrap();
            store
                .save_locked(&directory, YAML, &base.revision, &parse(YAML).unwrap())
                .unwrap();
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
    async fn recovery_handles_both_prepared_publication_windows_and_legacy_journals() {
        for (version, published) in [(2, false), (2, true), (1, false)] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
            let store = Store::new(dir.path().to_path_buf());
            let base = store.save(b"version: 1\n", MISSING_REVISION).unwrap();
            let plan = store.preview(YAML, &base.revision).unwrap();
            let fingerprint = plan.fingerprint.clone();
            {
                let directory = store.lock_directory().unwrap();
                let mut journal =
                    journal::ApplyJournal::begin(&directory, &store.filename, plan, YAML).unwrap();
                if version == 1 {
                    journal.version = 1;
                    journal.phase = ApplyPhase::Applying;
                    journal.persist(&directory).unwrap();
                }
                if published {
                    store
                        .save_locked(&directory, YAML, &base.revision, &parse(YAML).unwrap())
                        .unwrap();
                }
            }
            let mut provider = FixtureProvider {
                path: dir.path().to_path_buf(),
                fail_preflight: false,
                fail_second: false,
                unknown: true,
                applied: Mutex::default(),
            };
            assert!(store.resume_apply(&fingerprint, &provider).await.is_err());
            assert_eq!(
                store.read().unwrap().unwrap().revision,
                if published {
                    super::super::revision(YAML)
                } else {
                    base.revision.clone()
                }
            );
            provider.unknown = false;
            if version == 1 {
                provider.fail_second = true;
                assert!(store.resume_apply(&fingerprint, &provider).await.is_err());
                assert_eq!(store.read().unwrap().unwrap().revision, base.revision);
                let failed = store.pending_apply().unwrap().unwrap();
                assert_eq!(failed.version, 1);
                assert_eq!(failed.failed_resource.as_deref(), Some("service/b"));
                provider.fail_second = false;
            }
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
    }

    #[tokio::test]
    async fn successful_apply_publishes_before_native_resources() {
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
