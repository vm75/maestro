//! Revert replaces desired definitions, never workload data or client effects.
use anyhow::{Result, ensure};
use std::fs::File;

use super::{
    ApplyMode, ApplyPhase, Config, ObservedResource, Plan, RecoveryProvider, Snapshot, Store,
    journal, parse, store,
};

impl Store {
    /// Validate pending-transition supersession without changing its journal.
    /// The caller also applies the runtime server-setting/restart policy.
    pub async fn validate_revert(
        &self,
        bytes: &[u8],
        expected: &str,
        provider: &impl RecoveryProvider,
    ) -> Result<()> {
        let desired = parse(bytes)?;
        let store = self.clone();
        let directory = tokio::task::spawn_blocking(move || store.lock_directory()).await??;
        super::backup_capture::ensure_idle_locked(self, &directory)?;
        let (previous, plan) = Self::plan(
            store::current(&directory, &self.filename)?,
            bytes,
            expected,
            &desired,
        )?;
        self.revert_prior(&directory, &previous, &plan, None, provider)
            .await?;
        Ok(())
    }

    /// Supersede a published pending transition only after verifying that no
    /// unresolved removal or unchanged native definition will be abandoned.
    pub async fn apply_revert_review(
        &self,
        bytes: &[u8],
        expected: &str,
        fingerprint: &str,
        provider: &impl RecoveryProvider,
    ) -> Result<Snapshot> {
        self.apply_revert_review_mode(bytes, expected, fingerprint, ApplyMode::Now, provider)
            .await
    }

    pub async fn apply_revert_review_mode(
        &self,
        bytes: &[u8],
        expected: &str,
        fingerprint: &str,
        mode: ApplyMode,
        provider: &impl RecoveryProvider,
    ) -> Result<Snapshot> {
        let desired = parse(bytes)?;
        let store = self.clone();
        let directory = tokio::task::spawn_blocking(move || store.lock_directory()).await??;
        super::backup_capture::ensure_idle_locked(self, &directory)?;
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
        let prior = self
            .revert_prior(&directory, &previous, &plan, Some(mode), provider)
            .await?;
        let mut observed = Vec::new();
        for change in &plan.changes {
            let native = provider
                .observe_mode(change, &previous, &desired, mode)
                .await?;
            ensure!(
                native != ObservedResource::Unknown,
                "native state cannot be reconciled with the revert"
            );
            if native == ObservedResource::Before {
                provider
                    .preflight_mode(change, &previous, &desired, mode)
                    .await?;
            }
            observed.push(native);
        }
        let mut journal = journal::ApplyJournal::begin_replacing_mode(
            &directory,
            &self.filename,
            plan,
            bytes,
            prior,
            mode,
        )?;
        let snapshot = self.save_locked(&directory, bytes, expected, &desired)?;
        journal.phase = ApplyPhase::Applying;
        journal.superseded = None;
        journal.persist(&directory)?;
        for (index, change) in journal.plan.changes.iter().enumerate() {
            let native = provider
                .observe_mode(change, &previous, &desired, mode)
                .await?;
            ensure!(
                native == observed[index],
                "native state changed during revert"
            );
            let result = async {
                if native == ObservedResource::Before {
                    provider
                        .apply_mode(change, &previous, &desired, mode)
                        .await?;
                    ensure!(
                        provider
                            .observe_mode(change, &previous, &desired, mode)
                            .await?
                            == ObservedResource::After,
                        "reverted resource does not match the reviewed state"
                    );
                }
                Ok::<_, anyhow::Error>(())
            }
            .await;
            if let Err(error) = result {
                let resource = change.resource.clone();
                journal.failed(&directory, resource, &error)?;
                return Err(error);
            }
            journal.completed += 1;
            journal.persist(&directory)?;
        }
        journal.phase = ApplyPhase::Complete;
        journal.persist(&directory)?;
        Ok(snapshot)
    }

    async fn revert_prior(
        &self,
        directory: &File,
        current: &Config,
        plan: &Plan,
        requested_mode: Option<ApplyMode>,
        provider: &impl RecoveryProvider,
    ) -> Result<Option<journal::ApplyJournal>> {
        let Some(prior) = journal::read(directory)?.filter(|j| !j.phase.is_terminal()) else {
            return Ok(None);
        };
        ensure!(
            prior.authority == self.filename,
            "pending apply belongs to a different configuration"
        );
        ensure!(
            prior.phase != ApplyPhase::Prepared && prior.superseded.is_none(),
            "inspect and retry the prepared transition before reverting"
        );
        ensure!(
            prior.version >= 2 && prior.plan.desired_revision == plan.base_revision,
            "pending transition must publish its desired revision before reverting"
        );
        let bytes = journal::candidate(directory, &prior)?;
        let prior_desired = parse(&bytes)?;
        let base = if prior.plan.base_revision == super::MISSING_REVISION {
            None
        } else {
            Some(self.read_history(&prior.plan.base_revision)?)
        };
        let (before, verified) =
            Self::plan(base, &bytes, &prior.plan.base_revision, &prior_desired)?;
        ensure!(
            verified.fingerprint == prior.plan.fingerprint,
            "pending journal does not match its recorded configuration"
        );
        for change in &verified.changes {
            if plan
                .changes
                .iter()
                .any(|next| next.resource == change.resource)
            {
                continue;
            }
            // A removed resource absent from both current and target must be
            // confirmed absent, including native registration. An unchanged
            // target likewise must already match its published desired state.
            ensure!(
                provider
                    .observe_mode(change, &before, current, prior.mode)
                    .await?
                    == ObservedResource::After,
                "revert would abandon an unresolved removal or unchanged resource; restore its definition or resolve the pending native state first"
            );
            if let Some(mode) = requested_mode.filter(|mode| *mode != prior.mode) {
                ensure!(
                    provider
                        .observe_mode(change, &before, current, mode)
                        .await?
                        == ObservedResource::After,
                    "unchanged native state does not satisfy the requested apply mode"
                );
            }
        }
        Ok(Some(prior))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::declarative::{ApplyProvider, Change, MISSING_REVISION};
    use std::{collections::BTreeMap, os::unix::fs::PermissionsExt, sync::Mutex};

    const EMPTY: &[u8] = b"version: 1\n";
    const FIRST: &[u8] =
        b"version: 1\nservices:\n  web: {kind: container, containers: {app: {image: alpine}}}\n";
    const SECOND: &[u8] =
        b"version: 1\nservices:\n  web: {kind: container, containers: {app: {image: busybox}}}\n";

    #[derive(Default)]
    struct Native {
        resources: Mutex<BTreeMap<String, Option<String>>>,
        calls: Mutex<usize>,
        observations: Mutex<usize>,
        fail: Mutex<bool>,
    }
    impl ApplyProvider for Native {
        async fn preflight(&self, _: &Change, _: &Config, _: &Config) -> Result<()> {
            Ok(())
        }
        async fn apply(&self, change: &Change, _: &Config, _: &Config) -> Result<()> {
            ensure!(!*self.fail.lock().unwrap(), "fixture native failure");
            *self.calls.lock().unwrap() += 1;
            self.resources
                .lock()
                .unwrap()
                .insert(change.resource.clone(), change.after.clone());
            Ok(())
        }
    }
    impl RecoveryProvider for Native {
        async fn observe(
            &self,
            change: &Change,
            _: &Config,
            _: &Config,
        ) -> Result<ObservedResource> {
            *self.observations.lock().unwrap() += 1;
            let resources = self.resources.lock().unwrap();
            let observed = resources.get(&change.resource).cloned().flatten();
            Ok(if observed == change.after {
                ObservedResource::After
            } else if observed == change.before {
                ObservedResource::Before
            } else {
                ObservedResource::Unknown
            })
        }
    }
    fn fixture() -> (tempfile::TempDir, Store, Native) {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let store = Store::new(root.path().to_path_buf());
        (root, store, Native::default())
    }
    async fn apply(
        store: &Store,
        native: &Native,
        bytes: &[u8],
        expected: &str,
    ) -> Result<Snapshot> {
        let plan = store.preview(bytes, expected)?;
        store
            .apply_review(bytes, expected, &plan.fingerprint, native)
            .await
    }
    async fn revert(
        store: &Store,
        native: &Native,
        bytes: &[u8],
        expected: &str,
    ) -> Result<Snapshot> {
        store.validate_revert(bytes, expected, native).await?;
        let plan = store.preview(bytes, expected)?;
        store
            .apply_revert_review(bytes, expected, &plan.fingerprint, native)
            .await
    }

    #[tokio::test]
    async fn automatic_retry_aborts_uncommitted_intent_without_reading_candidate_or_native_state() {
        for existing in [false, true] {
            for mode in [ApplyMode::Now, ApplyMode::NextStart] {
                let (root, store, native) = fixture();
                let original = if existing {
                    Some(store.save(EMPTY, MISSING_REVISION).unwrap())
                } else {
                    None
                };
                let revision = original
                    .as_ref()
                    .map_or(MISSING_REVISION, |snapshot| snapshot.revision.as_str());
                let plan = store.preview(FIRST, revision).unwrap();
                let fingerprint = plan.fingerprint.clone();
                {
                    let directory = store.lock_directory().unwrap();
                    journal::ApplyJournal::begin_replacing_mode(
                        &directory,
                        &store.filename,
                        plan,
                        FIRST,
                        None,
                        mode,
                    )
                    .unwrap();
                }
                // This unused candidate is not authority and need not be parsed
                // or published to discard its uncommitted intent safely.
                std::fs::write(
                    root.path().join("apply-candidate.yaml"),
                    "broken unused candidate",
                )
                .unwrap();
                let status = store.retry_apply(&native).await.unwrap();
                assert_eq!(status["stage"], "aborted");
                assert_eq!(status["mode"], serde_json::to_value(mode).unwrap());
                assert!(store.pending_apply().unwrap().is_none());
                assert!(store.pending_candidate(&fingerprint).is_err());
                assert_eq!(*native.calls.lock().unwrap(), 0);
                assert_eq!(*native.observations.lock().unwrap(), 0);
                assert_eq!(
                    store.read().unwrap().map(|snapshot| snapshot.bytes),
                    original.as_ref().map(|snapshot| snapshot.bytes.clone())
                );
                assert_eq!(store.save(SECOND, revision).unwrap().bytes, SECOND);
            }
        }
    }

    #[tokio::test]
    async fn automatic_retry_reconciles_prepared_intent_after_yaml_commit() {
        let (_root, store, native) = fixture();
        let first = apply(&store, &native, FIRST, MISSING_REVISION)
            .await
            .unwrap();
        let plan = store.preview(SECOND, &first.revision).unwrap();
        {
            let directory = store.lock_directory().unwrap();
            journal::ApplyJournal::begin(&directory, &store.filename, plan, SECOND).unwrap();
            store
                .save_locked(&directory, SECOND, &first.revision, &parse(SECOND).unwrap())
                .unwrap();
        }
        let result = store.retry_apply(&native).await.unwrap();
        assert_eq!(result["stage"], "complete");
        assert_eq!(*native.calls.lock().unwrap(), 2);
        assert_eq!(store.read().unwrap().unwrap().bytes, SECOND);
    }

    #[tokio::test]
    async fn automatic_retry_preserves_prepared_intent_when_yaml_has_an_unrelated_revision() {
        let (root, store, native) = fixture();
        let original = store.save(EMPTY, MISSING_REVISION).unwrap();
        let plan = store.preview(FIRST, &original.revision).unwrap();
        {
            let directory = store.lock_directory().unwrap();
            journal::ApplyJournal::begin(&directory, &store.filename, plan, FIRST).unwrap();
        }
        let path = root.path().join("apply-journal.json");
        let before = std::fs::read(&path).unwrap();
        let external = b"version: 1\n# external edit\n";
        std::fs::write(store.path(), external).unwrap();
        assert!(store.retry_apply(&native).await.is_err());
        assert_eq!(std::fs::read(path).unwrap(), before);
        assert_eq!(store.read().unwrap().unwrap().bytes, external);
        assert_eq!(*native.calls.lock().unwrap(), 0);
        assert_eq!(*native.observations.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn automatic_retry_reports_durable_failure_and_never_replays_removals() {
        let (_root, store, native) = fixture();
        assert!(store.apply_status().unwrap().is_none());
        let first = apply(&store, &native, FIRST, MISSING_REVISION)
            .await
            .unwrap();
        *native.fail.lock().unwrap() = true;
        assert!(
            apply(&store, &native, EMPTY, &first.revision)
                .await
                .is_err()
        );
        let restarted = Store::new(store.path().parent().unwrap().to_path_buf());
        let failed = restarted.apply_status().unwrap().unwrap();
        assert_eq!(failed["stage"], "failed");
        assert_eq!(failed["resources"]["service/web"]["state"], "failed");
        assert!(failed["updatedAt"].is_string());
        assert!(!failed.to_string().contains("fixture native failure"));
        *native.fail.lock().unwrap() = false;
        let error = restarted.retry_apply(&native).await.err().unwrap();
        assert!(
            error
                .to_string()
                .contains("automatic destructive replay refused")
        );
        assert_eq!(*native.calls.lock().unwrap(), 1);
        assert_eq!(restarted.apply_status().unwrap().unwrap(), failed);
        native.resources.lock().unwrap().clear();
        let complete = restarted.retry_apply(&native).await.unwrap();
        assert_eq!(complete["stage"], "complete");
        assert_eq!(complete["resources"]["service/web"]["state"], "registered");
        assert_eq!(*native.calls.lock().unwrap(), 1);
        assert!(restarted.pending_apply().unwrap().is_none());
        assert_eq!(restarted.retry_apply(&native).await.unwrap(), complete);
        assert_eq!(*native.calls.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn automatic_retry_reconciles_an_owned_update_and_clears_failure() {
        let (_root, store, native) = fixture();
        let first = apply(&store, &native, FIRST, MISSING_REVISION)
            .await
            .unwrap();
        *native.fail.lock().unwrap() = true;
        assert!(
            apply(&store, &native, SECOND, &first.revision)
                .await
                .is_err()
        );
        *native.fail.lock().unwrap() = false;
        let complete = store.retry_apply(&native).await.unwrap();
        assert_eq!(complete["stage"], "complete");
        assert_eq!(complete["desiredRevision"], super::super::revision(SECOND));
        assert_eq!(*native.calls.lock().unwrap(), 2);
        assert!(store.pending_apply().unwrap().is_none());
    }

    #[tokio::test]
    async fn failed_update_revert_skips_already_matching_native_definition() {
        let (_root, store, native) = fixture();
        let first = apply(&store, &native, FIRST, MISSING_REVISION)
            .await
            .unwrap();
        *native.fail.lock().unwrap() = true;
        assert!(
            apply(&store, &native, SECOND, &first.revision)
                .await
                .is_err()
        );
        let failed = store.read().unwrap().unwrap();
        assert_eq!(failed.bytes, SECOND);
        assert!(
            revert(&store, &native, FIRST, &first.revision)
                .await
                .is_err()
        );
        *native.fail.lock().unwrap() = false;
        let restored = revert(&store, &native, FIRST, &failed.revision)
            .await
            .unwrap();
        assert_eq!(restored.bytes, FIRST);
        assert_eq!(*native.calls.lock().unwrap(), 1);
        assert_eq!(store.read_history(&failed.revision).unwrap().bytes, SECOND);
        assert!(store.pending_apply().unwrap().is_none());
    }

    #[tokio::test]
    async fn unresolved_removal_cannot_disappear_from_revert_tracking() {
        let (_root, store, native) = fixture();
        let initial = apply(&store, &native, EMPTY, MISSING_REVISION)
            .await
            .unwrap();
        let first = apply(&store, &native, FIRST, &initial.revision)
            .await
            .unwrap();
        *native.fail.lock().unwrap() = true;
        assert!(
            apply(&store, &native, EMPTY, &first.revision)
                .await
                .is_err()
        );
        let failed = store.read().unwrap().unwrap();
        let pending = store.pending_apply().unwrap().unwrap();
        let error = revert(&store, &native, EMPTY, &failed.revision)
            .await
            .err()
            .unwrap();
        assert!(error.to_string().contains("unresolved removal"));
        assert_eq!(
            store.pending_apply().unwrap().unwrap().plan.fingerprint,
            pending.plan.fingerprint
        );
        assert_eq!(
            store.pending_candidate(&pending.plan.fingerprint).unwrap(),
            EMPTY
        );
        assert_eq!(store.read().unwrap().unwrap().revision, failed.revision);
        *native.fail.lock().unwrap() = false;
        assert_eq!(
            revert(&store, &native, FIRST, &failed.revision)
                .await
                .unwrap()
                .bytes,
            FIRST
        );
        assert_eq!(*native.calls.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn completed_removal_can_be_superseded_without_repeating_native_work() {
        let (_root, store, native) = fixture();
        let first = apply(&store, &native, FIRST, MISSING_REVISION)
            .await
            .unwrap();
        *native.fail.lock().unwrap() = true;
        assert!(
            apply(&store, &native, EMPTY, &first.revision)
                .await
                .is_err()
        );
        native.resources.lock().unwrap().clear(); // native removal finished before the crash
        let current = store.read().unwrap().unwrap();
        assert_eq!(
            revert(&store, &native, EMPTY, &current.revision)
                .await
                .unwrap()
                .bytes,
            EMPTY
        );
        assert_eq!(*native.calls.lock().unwrap(), 1);
        assert!(store.pending_apply().unwrap().is_none());
    }

    #[tokio::test]
    async fn repeated_supersession_alternates_candidates_without_losing_pending_intent() {
        let (_root, store, native) = fixture();
        let first = apply(&store, &native, FIRST, MISSING_REVISION)
            .await
            .unwrap();
        *native.fail.lock().unwrap() = true;
        assert!(
            apply(&store, &native, SECOND, &first.revision)
                .await
                .is_err()
        );
        let prior = store.pending_apply().unwrap().unwrap();
        let change = &prior.plan.changes[0];
        native
            .resources
            .lock()
            .unwrap()
            .insert(change.resource.clone(), change.after.clone());
        let second_revision = store.read().unwrap().unwrap().revision;
        assert!(
            revert(&store, &native, FIRST, &second_revision)
                .await
                .is_err()
        );
        let pending = store.pending_apply().unwrap().unwrap();
        assert!(pending.alternate_candidate);
        assert_eq!(
            store.pending_candidate(&pending.plan.fingerprint).unwrap(),
            FIRST
        );
        let restored = revert(&store, &native, SECOND, &first.revision)
            .await
            .unwrap();
        assert_eq!(restored.bytes, SECOND);
        assert!(store.pending_apply().unwrap().is_none());
        let directory = store.lock_directory().unwrap();
        assert!(
            !journal::read(&directory)
                .unwrap()
                .unwrap()
                .alternate_candidate
        );
    }

    #[tokio::test]
    async fn invalid_replacement_is_rejected_before_overwriting_either_candidate() {
        let (_root, store, native) = fixture();
        let first = apply(&store, &native, FIRST, MISSING_REVISION)
            .await
            .unwrap();
        *native.fail.lock().unwrap() = true;
        assert!(
            apply(&store, &native, SECOND, &first.revision)
                .await
                .is_err()
        );
        let mut prior = store.pending_apply().unwrap().unwrap();
        let fingerprint = prior.plan.fingerprint.clone();
        let current = store.read().unwrap().unwrap();
        let plan = store.preview(FIRST, &current.revision).unwrap();
        prior.phase = ApplyPhase::Prepared;
        let directory = store.lock_directory().unwrap();
        assert!(
            journal::ApplyJournal::begin_replacing(
                &directory,
                &store.filename,
                plan,
                FIRST,
                Some(prior)
            )
            .is_err()
        );
        assert_eq!(
            journal::candidate(&directory, &journal::read(&directory).unwrap().unwrap()).unwrap(),
            SECOND
        );
        assert_eq!(
            journal::read(&directory).unwrap().unwrap().plan.fingerprint,
            fingerprint
        );
    }

    #[tokio::test]
    async fn prepared_supersession_recovers_both_publication_windows() {
        for (published, automatic) in [(false, false), (true, false), (false, true), (true, true)] {
            let (_root, store, native) = fixture();
            let first = apply(&store, &native, FIRST, MISSING_REVISION)
                .await
                .unwrap();
            *native.fail.lock().unwrap() = true;
            assert!(
                apply(&store, &native, SECOND, &first.revision)
                    .await
                    .is_err()
            );
            let failed = store.read().unwrap().unwrap();
            let prior = store.pending_apply().unwrap().unwrap();
            let prior_fingerprint = prior.plan.fingerprint.clone();
            let plan = store.preview(FIRST, &failed.revision).unwrap();
            let fingerprint = plan.fingerprint.clone();
            {
                let directory = store.lock_directory().unwrap();
                let replacement = journal::ApplyJournal::begin_replacing(
                    &directory,
                    &store.filename,
                    plan,
                    FIRST,
                    Some(prior),
                )
                .unwrap();
                assert!(replacement.alternate_candidate);
                let status = store.apply_status().unwrap().unwrap();
                assert_eq!(status["stage"], "prepared");
                assert_eq!(status["superseded"]["stage"], "failed");
                assert_eq!(status["superseded"]["desiredRevision"], failed.revision);
                assert_eq!(
                    status["superseded"]["resources"]["service/web"]["state"],
                    "failed"
                );
                // The old candidate remains addressable until desired commit.
                assert_eq!(
                    journal::candidate(&directory, replacement.superseded.as_ref().unwrap())
                        .unwrap(),
                    SECOND
                );
                if published {
                    store
                        .save_locked(&directory, FIRST, &failed.revision, &parse(FIRST).unwrap())
                        .unwrap();
                }
            }
            *native.fail.lock().unwrap() = false;
            let snapshot = if automatic {
                store.retry_apply(&native).await.unwrap();
                store.read().unwrap().unwrap()
            } else {
                store.resume_apply(&fingerprint, &native).await.unwrap()
            };
            if published {
                assert_eq!(snapshot.bytes, FIRST);
                assert!(store.pending_apply().unwrap().is_none());
            } else {
                assert_eq!(snapshot.bytes, SECOND);
                let pending = store.pending_apply().unwrap().unwrap();
                assert_eq!(pending.plan.fingerprint, prior_fingerprint);
                assert_eq!(store.pending_candidate(&prior_fingerprint).unwrap(), SECOND);
            }
            assert_eq!(*native.calls.lock().unwrap(), 1);
        }
    }
}
