//! Shared Loom model-checking configuration for concurrency tests.
//!
//! Every Loom model in this crate runs through [`run_loom_model`], which is the
//! only place that builds a [`loom::model::Builder`]. Centralizing the builder
//! keeps the exploration bounds in one place and lets the CI runner report the
//! bounds each model actually used.
//!
//! # Profiles
//!
//! The `PG_EMBED_LOOM_PROFILE` environment variable selects an exploration
//! profile. Unset and `pr` select [`LoomProfile::Pr`], the bounds a pull
//! request uses. `scheduled` selects [`LoomProfile::Scheduled`], the larger
//! bounds the daily run uses. Any other value panics: silently falling back
//! would run different bounds than the caller asked for.
//!
//! Both profiles cap the participant count at [`MAX_THREADS`]. The scheduled
//! profile raises the branch budget and the preemption bound, which is where
//! the extra exploration comes from.
//!
//! # Markers
//!
//! Each run prints a `loom-model-start` line before Loom explores and a
//! `loom-model-complete` line only after exploration returns. Both lines name
//! the profile, every effective bound, and the test. The CI runner uses them to
//! prove that a model ran to completion under the profile it was asked for.

use std::{env, io::Write as _};

/// The participant cap both profiles use.
pub(crate) const MAX_THREADS: usize = 3;

/// The environment variable that selects an exploration profile.
pub(crate) const PROFILE_VARIABLE: &str = "PG_EMBED_LOOM_PROFILE";

/// The exploration profile a Loom run uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LoomProfile {
    /// Bounds sized for a pull-request run.
    Pr,
    /// Bounds sized for the daily scheduled run.
    Scheduled,
}

impl LoomProfile {
    /// Returns the profile named by `value`.
    ///
    /// `None` and `"pr"` select [`LoomProfile::Pr`]; `"scheduled"` selects
    /// [`LoomProfile::Scheduled`].
    ///
    /// # Panics
    ///
    /// Panics for any other value, because falling back to a default would
    /// explore different bounds than the caller asked for.
    pub(crate) fn from_value(value: Option<&str>) -> Self {
        match value {
            None | Some("pr") => Self::Pr,
            Some("scheduled") => Self::Scheduled,
            Some(other) => {
                panic!("unknown Loom profile {other:?}; expected \"pr\" or \"scheduled\"")
            }
        }
    }

    /// Returns the profile's name as the markers print it.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Pr => "pr",
            Self::Scheduled => "scheduled",
        }
    }

    /// Returns the branch budget for this profile.
    ///
    /// The scheduled budget is roughly 64 times `loom`'s default of 1,000.
    /// Measured runs never exhausted it, so it acts as a ceiling that turns a
    /// pathological model into a `branch_limit` outcome rather than a run that
    /// keeps exploring.
    pub(crate) const fn max_branches(self) -> usize {
        match self {
            Self::Pr => 64,
            Self::Scheduled => 65_536,
        }
    }

    /// Returns the preemption bound for this profile.
    ///
    /// The scheduled bound was chosen by measurement against the current
    /// inventory. Each step above 7 roughly doubles the work, so a larger bound
    /// would erode the margin against the profile's deadline without covering a
    /// materially different class of schedule. `.config/loom-models.toml`
    /// records the measured durations and the deadline chosen from them.
    pub(crate) const fn preemption_bound(self) -> usize {
        match self {
            Self::Pr => 3,
            Self::Scheduled => 10,
        }
    }
}

/// Applies `profile`'s bounds to `builder`, clearing every bound it does not set.
///
/// `Builder::new` reads several `LOOM_*` variables. Clearing the duration,
/// permutation and checkpoint bounds makes the run depend only on the profile,
/// so a stray variable cannot silently change what was explored.
fn apply_profile(builder: &mut loom::model::Builder, profile: LoomProfile) {
    builder.max_threads = MAX_THREADS;
    builder.max_branches = profile.max_branches();
    builder.preemption_bound = Some(profile.preemption_bound());
    builder.max_duration = None;
    builder.max_permutations = None;
    builder.checkpoint_file = None;
}

/// Returns `value` as text, or `none` when it is unset.
fn optional<T: std::fmt::Display>(value: Option<T>) -> String {
    value.map_or_else(|| "none".to_owned(), |inner| inner.to_string())
}

/// Returns the name libtest gave the current test thread.
fn test_name() -> String {
    std::thread::current()
        .name()
        .map_or_else(|| "<unnamed>".to_owned(), str::to_owned)
}

/// Returns a marker line naming the profile and every effective bound.
fn marker(kind: &str, profile: LoomProfile, builder: &loom::model::Builder) -> String {
    let checkpoint = builder
        .checkpoint_file
        .as_ref()
        .map_or_else(|| "none".to_owned(), |path| path.display().to_string());
    format!(
        "{kind} profile={} max_threads={} max_branches={} preemption_bound={} max_duration={} \
         max_permutations={} checkpoint={} test={}",
        profile.name(),
        builder.max_threads,
        builder.max_branches,
        optional(builder.preemption_bound),
        optional(builder.max_duration.map(|duration| duration.as_secs())),
        optional(builder.max_permutations),
        checkpoint,
        test_name(),
    )
}

/// Writes one marker line to standard output.
///
/// The markers are diagnostic: the CI runner reads them from the captured test
/// output. A closed or full stream cannot be reported usefully from inside a
/// model, so a failed write is deliberately ignored; the runner then sees a
/// missing marker and classifies the model `incomplete`.
fn emit(kind: &str, profile: LoomProfile, builder: &loom::model::Builder) {
    let line = format!("{}\n", marker(kind, profile, builder));
    let _write_outcome = std::io::stdout().write_all(line.as_bytes());
}

/// Runs a Loom model under the profile selected by `PG_EMBED_LOOM_PROFILE`.
///
/// The profile is read once per model. The start marker is printed before
/// exploration and the complete marker only after it returns, so a model that
/// panics or is killed never reports completion.
pub(crate) fn run_loom_model<F>(f: F)
where
    F: Fn() + Send + Sync + 'static,
{
    let profile = LoomProfile::from_value(env::var(PROFILE_VARIABLE).ok().as_deref());
    let mut builder = loom::model::Builder::new();
    apply_profile(&mut builder, profile);
    emit("loom-model-start", profile, &builder);
    builder.check(f);
    emit("loom-model-complete", profile, &builder);
}

#[cfg(test)]
mod tests {
    //! Checks the profile parser, the bounds each profile documents, and the
    //! fields the shared runner deliberately clears.

    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case(None, LoomProfile::Pr)]
    #[case(Some("pr"), LoomProfile::Pr)]
    #[case(Some("scheduled"), LoomProfile::Scheduled)]
    fn profile_parser_maps_documented_values(
        #[case] value: Option<&str>,
        #[case] expected: LoomProfile,
    ) {
        assert_eq!(LoomProfile::from_value(value), expected);
    }

    #[test]
    #[should_panic(expected = "unknown Loom profile")]
    fn profile_parser_rejects_unknown_values() { let _ = LoomProfile::from_value(Some("nightly")); }

    #[rstest]
    #[case(LoomProfile::Pr)]
    #[case(LoomProfile::Scheduled)]
    fn apply_profile_clears_bounds_the_profile_does_not_set(#[case] profile: LoomProfile) {
        let mut builder = loom::model::Builder::new();
        builder.max_duration = Some(std::time::Duration::from_secs(1));
        builder.max_permutations = Some(1);
        builder.checkpoint_file = Some(std::path::PathBuf::from("checkpoint"));

        apply_profile(&mut builder, profile);

        assert_eq!(builder.max_duration, None);
        assert_eq!(builder.max_permutations, None);
        assert_eq!(builder.checkpoint_file, None);
    }

    #[test]
    fn profiles_document_their_bounds() {
        assert_eq!(MAX_THREADS, 3);
        assert_eq!(LoomProfile::Pr.max_branches(), 64);
        assert_eq!(LoomProfile::Pr.preemption_bound(), 3);
        assert_eq!(LoomProfile::Scheduled.max_branches(), 65_536);
        assert_eq!(LoomProfile::Scheduled.preemption_bound(), 10);
        assert!(LoomProfile::Scheduled.max_branches() > LoomProfile::Pr.max_branches());
        assert!(LoomProfile::Scheduled.preemption_bound() > LoomProfile::Pr.preemption_bound());
    }

    #[rstest]
    #[case(
        LoomProfile::Pr,
        "profile=pr max_threads=3 max_branches=64 preemption_bound=3"
    )]
    #[case(
        LoomProfile::Scheduled,
        "profile=scheduled max_threads=3 max_branches=65536 preemption_bound=10"
    )]
    fn complete_marker_reports_every_effective_setting(
        #[case] profile: LoomProfile,
        #[case] expected: &str,
    ) {
        let mut builder = loom::model::Builder::new();
        apply_profile(&mut builder, profile);

        let line = marker("loom-model-complete", profile, &builder);

        assert!(line.starts_with("loom-model-complete "), "{line}");
        assert!(line.contains(expected), "{line}");
        assert!(line.contains("max_duration=none"), "{line}");
        assert!(line.contains("max_permutations=none"), "{line}");
        assert!(line.contains("checkpoint=none"), "{line}");
        assert!(
            line.contains(
                "test=loom_model::tests::complete_marker_reports_every_effective_setting"
            ),
            "{line}"
        );
    }
}
