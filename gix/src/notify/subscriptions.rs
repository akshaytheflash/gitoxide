use std::{path::PathBuf, time::Instant};

use super::{Changes, Domain, InvalidationPath, RepositoryMonitor, absolute_path};
use crate::Result;

impl RepositoryMonitor {
    /// Replace the linked-worktree selection and application dependencies in place.
    ///
    /// Other options remain as returned by [`options()`](Self::options). Paths are resolved like
    /// construction, relative to the original [`crate::Repository::current_dir()`], using the last
    /// observed filesystem options. Resolution performs filesystem work; invalid paths leave both
    /// subscriptions unchanged. Call this on a blocking worker when using an asynchronous executor.
    ///
    /// The next [`service()`](Self::service) installs the changed watches without restarting
    /// unaffected checkout streams. Removed checkouts publish [`super::WorktreeChange::Removed`];
    /// newly selected checkouts publish `Added`. Previous and new application invalidations are
    /// merged into that outcome so queued events cannot be lost during replacement. Registration
    /// failures are reported in the outcome and retried with the new subscriptions retained.
    /// Equal resolved subscriptions are a no-op. Scheduling and coverage policies require a new monitor.
    /// Native checkout selection can change separately with [`update_checkout_filter()`](Self::update_checkout_filter).
    pub fn update_subscriptions(
        &mut self,
        mut worktree_filter: Option<Vec<PathBuf>>,
        mut additional_paths: Vec<InvalidationPath>,
    ) -> Result<()> {
        for path in worktree_filter.iter_mut().flatten() {
            *path = absolute_path(path, &self.current_dir, self.precompose_unicode)?;
        }
        for path in &mut additional_paths {
            path.path = absolute_path(&path.path, &self.current_dir, self.precompose_unicode)?;
        }
        let selection_changed = self.options.worktree_filter != worktree_filter;
        let dependencies_changed = self.options.additional_paths != additional_paths;
        if !selection_changed && !dependencies_changed {
            return Ok(());
        }
        let now = Instant::now();
        if dependencies_changed {
            let mut changes = Changes::default();
            for path in self.options.additional_paths.iter().chain(&additional_paths) {
                changes.merge(path.changes.clone());
            }
            self.pending_metadata.add(changes, now, &self.options, true);
        }
        self.options.worktree_filter = worktree_filter;
        self.options.additional_paths = additional_paths;
        if dependencies_changed || selection_changed && self.all_worktrees_enabled() {
            self.metadata_dirty = true;
            self.maintenance = Some(now);
            #[cfg(feature = "async-notify")]
            self.wake.notify_one();
        }
        Ok(())
    }
    /// Replace native checkout coverage without removing private metadata or membership watches.
    ///
    /// Selection includes the primary repository and uses [`super::Options::checkout_filter`].
    /// Paths are resolved using the original current-directory base before any subscriptions change.
    /// Failed validation preserves all state. Newly covered checkouts receive a full baseline;
    /// disabling coverage also invalidates its previous contents so queued events cannot be lost.
    /// The next service registers changed coverage without publishing membership additions/removals.
    /// Call this on a blocking worker when using an asynchronous executor.
    pub fn update_checkout_filter(&mut self, mut checkout_filter: Option<Vec<PathBuf>>) -> Result<()> {
        for path in checkout_filter.iter_mut().flatten() {
            *path = absolute_path(path, &self.current_dir, self.precompose_unicode)?;
        }
        if self.options.checkout_filter == checkout_filter {
            return Ok(());
        }
        let now = Instant::now();
        self.replace_checkout_filter(checkout_filter.clone(), now);
        for other in self.other_worktrees.values_mut() {
            other.replace_checkout_filter(checkout_filter.clone(), now);
            self.pending_statistics
                .merge(std::mem::take(&mut other.pending_statistics));
            if other.maintenance.is_some_and(|deadline| deadline <= now) {
                self.maintenance = Some(now);
            }
        }
        #[cfg(feature = "async-notify")]
        self.wake.notify_one();
        Ok(())
    }

    fn replace_checkout_filter(&mut self, checkout_filter: Option<Vec<PathBuf>>, now: Instant) {
        let previous = self.worktree_enabled();
        self.options.checkout_filter = checkout_filter;
        let enabled = self.worktree_enabled();
        if previous == enabled {
            return;
        }
        self.pending_worktree.merge(Changes::worktree_all());
        if !enabled {
            self.pending_statistics.removed += self.worktree.registered.len();
            self.worktree = Domain::default();
            #[cfg(target_os = "macos")]
            {
                self.pending_statistics.removed += self.external_ignores.registered_len();
                self.external_ignores = Default::default();
            }
            self.directories.clear();
            self.nested_repositories.clear();
            self.directory_inventory = None;
            self.index_fingerprint = None;
            self.changed_directories.clear();
        }
        self.metadata_dirty = true;
        self.inventory_dirty = true;
        self.inventory_verify = false;
        self.index_dirty = true;
        self.maintenance = Some(now);
    }
}
