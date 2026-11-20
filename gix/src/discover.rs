use std::path::Path;

pub use gix_discover::*;
use gix_error::{ResultExt, message};
use gix_sec::{Trust, trust::DefaultForLevel};

use crate::{Repository, Result, ThreadSafeRepository, bstr::BString};

impl ThreadSafeRepository {
    /// Try to open a git repository in `directory` and search upwards through its parents until one is found,
    /// using default trust options which matters in case the found repository isn't owned by the current user.
    pub fn discover(directory: impl AsRef<Path>) -> Result<Self> {
        Self::discover_opts(directory, Default::default(), crate::open::Options::default_for_level)
    }

    /// Search upwards from `directory` using `options`. When a repository is found, call `open_options_by_trust`
    /// with its initial trust level to obtain the options for opening it.
    ///
    /// Discovery supplies the initial trust level through [`upwards::Options::trust`], replacing any
    /// [initial](crate::open::Options::with_trust()) or [enforced](crate::open::Options::with_enforced_trust())
    /// trust level in the returned options.
    /// [`upwards::TrustPolicy::Assume`] supplies that initial level directly. Opening still applies
    /// `safe.directory` and checks ownership of the final worktree.
    pub fn discover_opts(
        directory: impl AsRef<Path>,
        options: upwards::Options<'_>,
        open_options_by_trust: impl FnOnce(Trust) -> crate::open::Options,
    ) -> Result<Self> {
        Self::discover_with_options(directory.as_ref(), options, |trust| {
            open_options_by_trust(trust).with_trust(trust)
        })
    }

    fn discover_with_options(
        directory: &Path,
        options: upwards::Options<'_>,
        open_options: impl FnOnce(Trust) -> crate::open::Options,
    ) -> Result<Self> {
        let _span = gix_trace::coarse!("ThreadSafeRepository::discover()");
        let (path, trust) = upwards_opts(directory, options)?;
        let (git_dir, worktree_dir) = path.into_repository_and_work_tree_directories();
        let mut options = open_options(trust);
        // Note that we will adjust the `current_dir` later so it matches the value of `core.precomposeUnicode`.
        options.current_dir =
            Some(gix_fs::current_dir(false).or_raise(|| message("Could not obtain the current working directory"))?);
        Self::open_from_paths(git_dir, worktree_dir, options, None)
    }

    /// Try to open a git repository directly from the environment.
    /// If that fails, discover upwards from `directory` until one is found,
    /// while applying discovery options from the environment.
    ///
    /// For more, see [`ThreadSafeRepository::discover_with_environment_overrides_opts()`].
    pub fn discover_with_environment_overrides(directory: impl AsRef<Path>) -> Result<Self> {
        Self::discover_with_environment_overrides_opts(
            directory,
            Default::default(),
            crate::open::Options::default_for_level,
        )
    }

    /// Try to open a git repository directly from the environment, which reads `GIT_DIR`
    /// if it is set. Once selected, the repository also honors these primary environment
    /// overrides if permitted by its options:
    ///
    /// - `GIT_WORK_TREE`
    /// - `GIT_INDEX_FILE`
    /// - `GIT_SHALLOW_FILE`
    /// - `GIT_NAMESPACE`
    ///
    /// If `GIT_DIR` is unset, discover upwards from `directory` until one is found,
    /// while applying `options` with discovery overrides from the environment which includes:
    ///
    /// - `GIT_DISCOVERY_ACROSS_FILESYSTEM`
    /// - `GIT_CEILING_DIRECTORIES`
    ///
    /// This is particularly useful for hooks, as Git exports repository-local environment variables
    /// to them. Honoring these variables preserves the repository and worktree context established by
    /// Git, including temporary overrides, instead of relying only on discovery from `directory`.
    ///
    /// Once a repository is found, call `open_options_by_trust` with its initial trust level, derived from
    /// ownership if `GIT_DIR` is set or supplied by upward discovery otherwise. As in [`Self::discover_opts()`],
    /// this level replaces any trust setting in the returned options.
    ///
    /// ### Note
    ///
    /// Consider to set [`match_ceiling_dir_or_error = false`](gix_discover::upwards::Options::match_ceiling_dir_or_error)
    /// to allow discovery if an outside environment variable sets non-matching ceiling directories for greater
    /// compatibility with Git.
    pub fn discover_with_environment_overrides_opts(
        directory: impl AsRef<Path>,
        options: upwards::Options<'_>,
        open_options_by_trust: impl FnOnce(Trust) -> crate::open::Options,
    ) -> Result<Self> {
        Self::discover_with_environment_and_options(directory.as_ref(), options, None, |trust| {
            open_options_by_trust(trust).with_trust(trust)
        })
    }

    fn discover_with_environment_and_options(
        directory: &Path,
        mut options: upwards::Options<'_>,
        assumed_trust: Option<Trust>,
        open_options: impl FnOnce(Trust) -> crate::open::Options,
    ) -> Result<Self> {
        fn apply_additional_environment(mut opts: upwards::Options<'_>) -> upwards::Options<'_> {
            use crate::bstr::ByteVec;

            if let Some(cross_fs) = std::env::var_os("GIT_DISCOVERY_ACROSS_FILESYSTEM")
                .and_then(|v| Vec::from_os_string(v).ok().map(BString::from))
                && let Ok(b) = gix_config::Boolean::try_from(cross_fs)
            {
                opts.cross_fs = b.into();
            }
            opts
        }

        if std::env::var_os("GIT_DIR").is_some() {
            return Self::open_with_environment_and_options(directory, assumed_trust, open_options);
        }

        options = apply_additional_environment(options.apply_environment());
        Self::discover_with_options(directory, options, open_options)
    }
}

impl crate::open::Options {
    /// Discover a repository by searching upwards from `directory`, without applying environment overrides to discovery.
    ///
    /// See [`crate::discover_opts()`] for the trust policy and to customize the upward search.
    pub fn discover(self, directory: impl AsRef<Path>) -> Result<Repository> {
        crate::discover_opts(directory, Default::default(), self)
    }

    /// Open the repository selected by `GIT_DIR`, or discover one upwards from `directory`.
    ///
    /// See [`crate::discover_with_environment_overrides_opts()`] for the supported overrides and trust policy.
    pub fn discover_with_environment_overrides(self, directory: impl AsRef<Path>) -> Result<Repository> {
        crate::discover_with_environment_overrides_opts(directory, Default::default(), self)
    }
}

pub(crate) fn discover_with_open_options(
    directory: &Path,
    options: upwards::Options<'_>,
    open_options: crate::open::Options,
    use_environment: bool,
) -> Result<Repository> {
    let open_trust = open_options.git_dir_trust;
    let options_for_trust = |trust| {
        if open_trust.is_some() {
            open_options
        } else {
            open_options.with_trust(trust)
        }
    };
    if use_environment {
        ThreadSafeRepository::discover_with_environment_and_options(
            directory,
            options,
            open_trust.map(crate::open::RepositoryTrust::level),
            options_for_trust,
        )
    } else {
        ThreadSafeRepository::discover_with_options(directory, options, options_for_trust)
    }
    .map(Into::into)
}
