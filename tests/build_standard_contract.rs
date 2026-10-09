//! Contract tests for the Rust build standard.
//!
//! The standard makes the parallel `rustc` frontend and, on Linux, mold the
//! default for every development build. Cargo applies one `rustflags` source
//! rather than merging them, and an assigned `RUSTFLAGS` replaces every source,
//! so the flags are repeated in each configuration source, restated by each
//! development recipe, and kept out of the coverage and release recipes. The
//! Makefile clauses read what `make -n` prints on a Linux and a macOS host; each
//! listed `setup-rust` step passes `install-mold`. Fixtures come first, so no
//! rule passes by detecting nothing.

#[path = "build_standard_support/chain_exhaustive.rs"]
mod chain_exhaustive;
#[path = "build_standard_support/ci_steps.rs"]
mod ci_steps;
#[path = "build_standard_support/command_reader.rs"]
mod command_reader;
#[path = "build_standard_support/config.rs"]
mod config;
#[path = "build_standard_support/coverage_env.rs"]
mod coverage_env;
#[path = "build_standard_support/cranelift.rs"]
mod cranelift;
#[path = "build_standard_support/development.rs"]
mod development;
#[path = "build_standard_support/exhaustive.rs"]
mod exhaustive;
#[path = "build_standard_support/fixtures.rs"]
mod fixtures;
#[path = "build_standard_support/injected.rs"]
mod injected;
#[path = "build_standard_support/injected_held_out.rs"]
mod injected_held_out;
#[path = "build_standard_support/make.rs"]
mod make;
#[path = "build_standard_support/policy_cases.rs"]
mod policy_cases;
#[path = "build_standard_support/process.rs"]
mod process;
#[path = "build_standard_support/reader_cases.rs"]
mod reader_cases;
#[path = "build_standard_support/shell.rs"]
mod shell;
#[path = "build_standard_support/workflow_exhaustive.rs"]
mod workflow_exhaustive;
use ci_steps::workflow_problems;
use config::{CONFIG, Pin, Problems, TOOLCHAIN, config_problems};
use coverage_env::coverage_recipe_problems;
use development::development_problems;
use make::{Host, held_out_problems, held_out_target_count};
use process::real_make;

/// Turns a list of complaints into a test result.
fn none_of(problems: &Problems) -> Result<(), String> {
    if problems.is_empty() {
        Ok(())
    } else {
        Err(format!("{problems:#?}"))
    }
}

/// Every workflow that builds under the standard installs mold. A repository
/// whose workflows do not set up Rust through `setup-rust` lists none, and the
/// check then reads nothing; a listed workflow must have a step to read.
#[test]
fn every_setup_rust_step_installs_linker() -> Result<(), String> { none_of(&workflow_problems()) }

#[test]
fn every_rustflags_source_is_consistent_with_the_pin() -> Result<(), String> {
    none_of(&config_problems(CONFIG, Pin::read(TOOLCHAIN)?)?)
}

#[test]
fn development_targets_restate_the_flags_on_linux() -> Result<(), String> {
    let (problems, read) = development_problems(real_make, Host::Linux, Pin::read(TOOLCHAIN)?)?;
    none_of(&problems)?;
    if read == 0 {
        return Err(
            "no development target assigns RUSTFLAGS, so the check proves nothing".to_owned(),
        );
    }
    Ok(())
}

#[test]
fn development_targets_keep_the_frontend_but_not_the_linker_elsewhere() -> Result<(), String> {
    none_of(&development_problems(real_make, Host::Darwin, Pin::read(TOOLCHAIN)?)?.0)
}

/// Coverage measures and release ships, so both stay on the default flags. A
/// repository that lists no such target has nothing local to hold out, and the
/// check then reads no commands; otherwise it must read at least one.
#[test]
fn coverage_and_release_take_neither_flag() -> Result<(), String> {
    let (problems, read) = held_out_problems(real_make)?;
    none_of(&problems)?;
    if held_out_target_count() > 0 && read == 0 {
        return Err(
            "the held-out targets run no cargo command, so the check proves nothing".to_owned(),
        );
    }
    Ok(())
}

/// The coverage recipe assigns what it recorded besides `RUSTFLAGS`: the LLVM backend, the linker
/// and the link arguments a measurement needs. A repository with no local coverage recipe records
/// nothing.
#[test]
fn the_coverage_recipe_assigns_its_recorded_environment() -> Result<(), String> {
    none_of(&coverage_recipe_problems(real_make)?)
}
