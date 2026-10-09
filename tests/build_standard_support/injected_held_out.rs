//! The held-out half of the injected-runner checks: canned `make -n` text for the release and
//! coverage targets fed through the injected runner, so no process runs, and a synthetic held-out
//! target so the check fires in a repository that defines none.

use super::{
    injected::{canned, ensure, fake_make, undefined_make},
    make::{
        Host,
        MakeRunner,
        Target,
        held_out_problems,
        held_out_problems_for,
        held_out_target_count,
    },
};

// A fake runner whose command assigns an empty `RUSTFLAGS`, dropping the caller's.
fake_make!(
    release_clearing_make,
    "RUSTFLAGS=\"\" cargo build --release"
);

// A fake runner whose held-out target runs an inspection command beside an assigning build.
fake_make!(
    held_out_inspecting_make,
    "cargo metadata --format-version 1 --locked\nRUSTFLAGS=\"-D warnings\" cargo build --release"
);

// A fake runner whose held-out target runs only a metadata probe.
fake_make!(
    held_out_probe_only_make,
    "cargo metadata --format-version 1 --locked"
);

// A fake runner whose held-out command assigns `RUSTFLAGS` without a standard flag.
fake_make!(
    held_out_assigning_make,
    "RUSTFLAGS=\"-D warnings\" cargo build --release"
);

// A fake runner whose held-out command assigns nothing, so it takes the configuration's flags.
fake_make!(held_out_unassigned_make, "cargo build --release");

fake_make!(
    keeps_dash,
    "RUSTFLAGS=\"${RUSTFLAGS-}\" cargo build --release"
);
fake_make!(
    keeps_dash_with_flags,
    "RUSTFLAGS=\"${RUSTFLAGS-} -D warnings\" cargo build --release"
);
fake_make!(
    keeps_plus,
    "RUSTFLAGS=\"${RUSTFLAGS:+$RUSTFLAGS }\" cargo build --release"
);
fake_make!(
    keeps_plus_with_flags,
    "RUSTFLAGS=\"${RUSTFLAGS:+$RUSTFLAGS }-Zpolonius=next\" cargo build --release"
);
fake_make!(assigns_nothing_unquoted, "RUSTFLAGS= cargo build --release");
fake_make!(
    assigns_an_empty_string,
    "RUSTFLAGS=\"\" cargo build --release"
);
fake_make!(
    assigns_the_standard_flags_alone,
    "RUSTFLAGS=-Zthreads=8 -Clink-arg=-fuse-ld=mold cargo build --release"
);
fake_make!(
    assigns_its_own_flags_alone,
    "RUSTFLAGS=\"-D warnings\" cargo build --release"
);

/// A release command form, and whether the contract must accept it.
struct ReleaseForm {
    text: &'static str,
    runner: MakeRunner,
    accepted: bool,
}

/// Every release form the contract must tell apart: those that keep the caller's `RUSTFLAGS`, and
/// those that clear, replace or garble them.
const RELEASE_FORMS: [ReleaseForm; 8] = [
    ReleaseForm {
        text: "${RUSTFLAGS-}",
        runner: keeps_dash,
        accepted: true,
    },
    ReleaseForm {
        text: "${RUSTFLAGS-} plus flags",
        runner: keeps_dash_with_flags,
        accepted: true,
    },
    ReleaseForm {
        text: "${RUSTFLAGS:+$RUSTFLAGS }",
        runner: keeps_plus,
        accepted: true,
    },
    ReleaseForm {
        text: "${RUSTFLAGS:+$RUSTFLAGS } plus flags",
        runner: keeps_plus_with_flags,
        accepted: true,
    },
    ReleaseForm {
        text: "RUSTFLAGS=",
        runner: assigns_nothing_unquoted,
        accepted: false,
    },
    ReleaseForm {
        text: "RUSTFLAGS=\"\"",
        runner: assigns_an_empty_string,
        accepted: false,
    },
    ReleaseForm {
        text: "RUSTFLAGS=<standard flags> alone",
        runner: assigns_the_standard_flags_alone,
        accepted: false,
    },
    ReleaseForm {
        text: "RUSTFLAGS=\"-D warnings\" alone",
        runner: assigns_its_own_flags_alone,
        accepted: false,
    },
];

/// Scenario: a release command in each form it can take, fed through the injected runner.
///
/// Invariant: a release command is accepted exactly when it assigns `RUSTFLAGS` in a form that
/// keeps the caller's value, with or without flags of its own; an empty, unquoted, standard-only or
/// own-flags-only assignment is refused, whether as a complaint or as an unreadable line.
#[test]
fn a_release_command_is_accepted_only_when_it_keeps_the_callers_rustflags() -> Result<(), String> {
    for form in RELEASE_FORMS {
        let accepted = matches!(held_out_problems_for(form.runner, &["release"]), Ok((problems, _)) if problems.is_empty());
        ensure(
            accepted == form.accepted,
            &format!(
                "`{}` was judged accepted={accepted}, not {}",
                form.text, form.accepted
            ),
        )?;
    }
    Ok(())
}

/// Scenario: held-out targets fed canned `make -n` text, through a synthetic target so the check
/// fires in a repository that defines none.
///
/// Invariant: a held-out command must assign `RUSTFLAGS`; a release build must keep the caller's
/// flags while a coverage build, a measurement, need not; an inspection command is not refused; a
/// runner error reaches the caller.
#[test]
fn the_held_out_policy_runs_against_an_injected_runner() -> Result<(), String> {
    ensure(
        held_out_problems_for(undefined_make, &["synthetic"]).is_err(),
        "a held-out runner error was swallowed",
    )?;
    let (clean, read) = held_out_problems_for(held_out_assigning_make, &["synthetic"])?;
    ensure(
        clean.is_empty() && read == 1,
        &format!("an assigning held-out command raised {clean:?}"),
    )?;
    let (measuring, _) = held_out_problems_for(release_clearing_make, &["coverage"])?;
    ensure(
        measuring.is_empty(),
        &format!("a coverage build that ignores the caller's RUSTFLAGS raised {measuring:?}"),
    )?;
    let (inspecting, _) = held_out_problems_for(held_out_inspecting_make, &["synthetic"])?;
    ensure(
        inspecting.is_empty(),
        &format!("an inspection command in a held-out target raised {inspecting:?}"),
    )?;
    let (unassigned, _) = held_out_problems_for(held_out_unassigned_make, &["synthetic"])?;
    ensure(
        !unassigned.is_empty(),
        "a held-out command that assigns no RUSTFLAGS passed",
    )?;
    ensure(
        held_out_target_count() == 0 || held_out_problems(undefined_make).is_err(),
        "the listed held-out targets were not run",
    )
}

/// Scenario: a held-out target whose recipe runs a metadata probe and nothing that builds or
/// tests, and one whose recipe runs a build.
///
/// Invariant: each held-out target must run a build or test command of its own, so a probe-only
/// target is refused instead of hiding behind the commands another target contributes.
#[test]
fn a_held_out_target_must_run_a_build_or_test_command_of_its_own() -> Result<(), String> {
    let (probe_only, read) = held_out_problems_for(held_out_probe_only_make, &["coverage"])?;
    ensure(read == 1, "the probe was not read")?;
    ensure(
        probe_only
            .iter()
            .any(|problem| problem.contains("runs no build or test command")),
        "a target that only probes passed",
    )?;
    let (building, _) = held_out_problems_for(held_out_inspecting_make, &["coverage"])?;
    ensure(
        building.is_empty(),
        &format!("a target that builds, beside a probe, raised {building:?}"),
    )
}
