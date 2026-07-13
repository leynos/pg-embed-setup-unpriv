//! Shared Loom model-checking configuration for concurrency tests.

/// Runs a Loom model with the repository's bounded concurrency budget.
pub(crate) fn run_loom_model<F>(f: F)
where
    F: Fn() + Send + Sync + 'static,
{
    let mut builder = loom::model::Builder::new();
    builder.max_threads = 3;
    builder.max_branches = 64;
    builder.preemption_bound = Some(3);
    builder.check(f);
}
