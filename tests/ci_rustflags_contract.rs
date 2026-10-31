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

/// Returns the lines of the workflow step whose `- name:` is `name`: from that line to the next line
/// indented no deeper than the list item, so a re-indented workflow or a later step cannot lend it
/// an environment it does not carry.
fn step_lines(name: &str) -> Vec<&'static str> {
    let marker = format!("- name: {name}");
    let mut lines = CI.lines().skip_while(|line| line.trim() != marker);
    let Some(first) = lines.next() else {
        return Vec::new();
    };
    let indent = first.len() - first.trim_start().len();
    std::iter::once(first)
        .chain(lines.take_while(|line| {
            line.trim().is_empty() || line.len() - line.trim_start().len() > indent
        }))
        .collect()
}

/// Returns the `RUSTFLAGS` value a workflow step assigns in its own `env:` block.
fn step_env_rustflags(name: &str) -> Option<String> {
    step_lines(name).into_iter().find_map(|line| {
        line.trim()
            .strip_prefix("RUSTFLAGS:")
            .map(|value| value.trim().to_owned())
    })
}

/// Scenario: the root job's direct `cargo build`, which `setup-rust`'s exported `-D warnings` would
/// otherwise leave without the build standard's flags (Concordat BD-009).
///
/// Invariant: the step assigns the warning policy, the parallel frontend and the mold linker in its
/// own `env:`, so dropping any of them, or the whole assignment, fails here.
#[test]
fn the_root_step_restates_the_build_standard_in_its_environment() {
    let value = step_env_rustflags("Install cargo-nextest and test (root)")
        .expect("the root test step must assign RUSTFLAGS in its env");
    let words: Vec<&str> = value.split_whitespace().collect();
    assert!(
        words.windows(2).any(|pair| pair == ["-D", "warnings"]),
        "the root step must keep denying warnings: {value}"
    );
    for flag in ["-Zthreads=8", "-Clink-arg=-fuse-ld=mold"] {
        assert!(
            words.contains(&flag),
            "the root step must restate {flag}: {value}"
        );
    }
}

/// The step reader stays inside its own step: a later step's assignment is not lent to an earlier one.
#[test]
fn the_step_reader_does_not_read_past_its_step() {
    assert_eq!(
        step_env_rustflags("Test with all features (unprivileged)"),
        None,
        "the unprivileged step assigns RUSTFLAGS on its command line, not in an env block"
    );
}
