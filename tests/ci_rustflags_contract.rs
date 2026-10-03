//! Contract that the CI jobs which assign `RUSTFLAGS` keep the build standard's flags.
//!
//! An assigned `RUSTFLAGS` replaces every `rustflags` source in
//! `.cargo/config.toml`, so a job that sets one without the parallel frontend
//! (or, on Linux, mold) silently drops the standard. The coverage step is the
//! deliberate exception: a measurement stays off the fast flags. The workflow is
//! read at compile time, so the test needs no filesystem access.

const CI: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/.github/workflows/ci.yml"
));

/// Returns each `RUSTFLAGS` assignment in the workflow with its line number.
fn assignments() -> Vec<(usize, &'static str)> {
    CI.lines()
        .enumerate()
        .filter(|(_, line)| {
            line.contains("RUSTFLAGS=\"") || line.trim_start().starts_with("RUSTFLAGS:")
        })
        .map(|(index, line)| (index + 1, line))
        .collect()
}

/// Scenario: every `RUSTFLAGS` the workflow assigns, except the coverage step's.
///
/// Invariant: each carries the parallel frontend flag, and the Linux test step
/// carries the linker flag too.
#[test]
fn ci_rustflags_assignments_keep_the_standard_flags() {
    let found = assignments();
    assert!(
        found.len() >= 3,
        "the workflow assigns fewer RUSTFLAGS than expected: {found:?}"
    );
    for (line, text) in found {
        let is_coverage = CI
            .lines()
            .nth(line.saturating_sub(2))
            .is_some_and(|prev| prev.contains("Assigning RUSTFLAGS displaces"));
        if is_coverage {
            assert!(
                !text.contains("-Zthreads=8"),
                "line {line}: coverage must stay off the fast flags"
            );
            continue;
        }
        assert!(
            text.contains("-Zthreads=8"),
            "line {line} drops -Zthreads=8: {text}"
        );
        if text.contains("RUSTFLAGS=\"") {
            assert!(
                text.contains("-Clink-arg=-fuse-ld=mold"),
                "line {line} drops mold on Linux: {text}"
            );
        }
    }
}
