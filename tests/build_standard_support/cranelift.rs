//! The Cranelift half of the build standard.
//!
//! A repository whose `.cargo/config.toml` selects the Cranelift backend for the development
//! profile must declare the nightly Cargo feature that lets it (`[unstable] codegen-backend =
//! true`), pin a toolchain that ships the component (`rustc-codegen-cranelift-preview`), and have
//! its Whitaker install provision the same component (`cranelift: 'true'` on the
//! `install-whitaker` step), or a clean checkout fails at the first build. The coverage action owns
//! the LLVM override that instrumentation needs, so that is not repeated here.
//!
//! Whether the repository selects Cranelift is recorded in [`SELECTED`] and pinned both ways, so
//! adding or dropping the selection is a deliberate edit of this contract.

use super::{
    ci_steps::{Workflow, cranelift_workflow_problems, whitaker_cranelift_problems},
    config::{CONFIG, Problems, TOOLCHAIN},
};

/// Whether this repository selects Cranelift for the development profile.
pub const SELECTED: bool = false;

/// The component a toolchain must ship for the backend.
const COMPONENT: &str = "rustc-codegen-cranelift-preview";

/// Returns the `(table, key, value)` triples of a Cargo configuration, skipping comments.
fn entries(config: &str) -> Vec<(String, String, String)> {
    let mut table = String::new();
    let mut found = Vec::new();
    for line in config
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        if let Some(name) = line.strip_prefix('[') {
            name.trim_end_matches(']').trim().clone_into(&mut table);
        } else if let Some((key, raw)) = line.split_once('=') {
            let value = raw.split(" #").next().unwrap_or_default().trim();
            found.push((table.clone(), key.trim().to_owned(), value.to_owned()));
        }
    }
    found
}

/// Returns whether a table holds a key with exactly this value.
fn holds(config: &str, table: &str, key: &str, value: &str) -> bool {
    entries(config)
        .iter()
        .any(|(t, k, v)| t == table && k == key && v == value)
}

/// Returns whether the configuration selects Cranelift for the development profile.
///
/// ```text
/// [profile.dev]
/// codegen-backend = "cranelift"   -> true
/// ```
pub fn selects_cranelift(config: &str) -> bool {
    holds(config, "profile.dev", "codegen-backend", "\"cranelift\"")
}

/// Returns the complaints about a configuration that selects Cranelift: the Cargo feature that
/// lets it must be on.
pub fn config_problems(config: &str) -> Problems {
    let mut problems = Vec::new();
    if !selects_cranelift(config) {
        problems.push("[profile.dev] does not select cranelift".to_owned());
    }
    if !holds(config, "unstable", "codegen-backend", "true") {
        problems.push("[unstable] does not set codegen-backend = true".to_owned());
    }
    problems
}

/// Returns the complaints about a toolchain file that must ship the Cranelift component.
pub fn toolchain_problems(toolchain: &str) -> Problems {
    let listed = toolchain
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .any(|line| line.contains(&format!("\"{COMPONENT}\"")));
    if listed {
        Vec::new()
    } else {
        vec![format!("no {COMPONENT} component in rust-toolchain.toml")]
    }
}

/// Turns a list of complaints into a test result.
fn none_of(problems: &Problems) -> Result<(), String> {
    if problems.is_empty() {
        Ok(())
    } else {
        Err(format!("{problems:#?}"))
    }
}

const CONFIG_OK: &str = concat!(
    "[unstable]\ncodegen-backend = true\n\n",
    "[profile.dev]\ncodegen-backend = \"cranelift\" # fast debug builds\n",
);
const CONFIG_NO_FEATURE: &str = "[profile.dev]\ncodegen-backend = \"cranelift\"\n";
const CONFIG_FEATURE_OFF: &str = concat!(
    "[unstable]\ncodegen-backend = false\n\n",
    "[profile.dev]\ncodegen-backend = \"cranelift\"\n",
);
const CONFIG_OTHER_BACKEND: &str = concat!(
    "[unstable]\ncodegen-backend = true\n\n",
    "[profile.dev]\ncodegen-backend = \"llvm\"\n",
);
const CONFIG_WRONG_TABLE: &str = concat!(
    "[unstable]\ncodegen-backend = true\n\n",
    "[profile.release]\ncodegen-backend = \"cranelift\"\n",
);
const CONFIG_COMMENTED: &str = concat!(
    "[unstable]\n# codegen-backend = true\n\n",
    "[profile.dev]\n# codegen-backend = \"cranelift\"\n",
);
const TOOLCHAIN_OK: &str = concat!(
    "[toolchain]\nchannel = \"nightly-2026-05-28\"\n",
    "components = [\n  \"clippy\",\n  \"rustc-codegen-cranelift-preview\",\n]\n",
);
const TOOLCHAIN_LACKING: &str = concat!(
    "[toolchain]\nchannel = \"nightly-2026-05-28\"\n",
    "components = [\"clippy\", \"rustfmt\"]\n",
);
const TOOLCHAIN_COMMENTED: &str = concat!(
    "[toolchain]\nchannel = \"nightly-2026-05-28\"\n",
    "# components = [\"rustc-codegen-cranelift-preview\"]\n",
);
const TOOLCHAIN_LOOKALIKE: &str = "[toolchain]\ncomponents = [\"rustc-codegen-cranelift\"]\n";

/// The head of an `install-whitaker` step, shared by the fixtures below.
const STEP: &str = concat!(
    "      - name: Install Whitaker\n",
    "        uses: org/shared-actions/.github/actions/install-whitaker@abc\n",
);
const WHITAKER_OK: &str = concat!(
    "      - name: Install Whitaker\n",
    "        uses: org/shared-actions/.github/actions/install-whitaker@abc\n",
    "        with:\n          cranelift: 'true'\n",
);
const WHITAKER_BARE: &str = concat!(
    "      - name: Install Whitaker\n",
    "        uses: org/shared-actions/.github/actions/install-whitaker@abc\n",
    "        with:\n          cranelift: true\n",
);
const WHITAKER_OFF: &str = concat!(
    "      - name: Install Whitaker\n",
    "        uses: org/shared-actions/.github/actions/install-whitaker@abc\n",
    "        with:\n          cranelift: 'false'\n",
);
const WHITAKER_BEFORE_A_SIBLING: &str = concat!(
    "      - name: Install Whitaker\n",
    "        uses: org/shared-actions/.github/actions/install-whitaker@abc\n",
    "      - name: Other\n        with:\n          cranelift: 'true'\n",
);
const WHITAKER_COMMENT_ONLY: &str = concat!(
    "      # install-whitaker@abc needs cranelift\n",
    "      - name: Other\n        run: true\n",
);

/// Returns the number of complaints about a fixture workflow's Whitaker steps.
fn whitaker_count(text: &str) -> usize {
    whitaker_cranelift_problems(&Workflow {
        file: "fixture.yml",
        text,
    })
    .len()
}

/// Scenario: the repository as recorded.
///
/// Invariant: the configuration selects Cranelift exactly when [`SELECTED`] says so, so the
/// clause below is applied, or left unapplied, on purpose.
#[test]
fn the_repository_selects_cranelift_as_recorded() -> Result<(), String> {
    if selects_cranelift(CONFIG) == SELECTED {
        Ok(())
    } else {
        let state = if SELECTED {
            "no longer selects"
        } else {
            "now selects"
        };
        Err(format!(
            "the configuration {state} Cranelift; SELECTED is {SELECTED}"
        ))
    }
}

/// Scenario: a repository that selects Cranelift.
///
/// Invariant: the Cargo feature, the toolchain component and every Whitaker install are in place;
/// a repository that does not select it has nothing to hold.
#[test]
fn a_cranelift_repository_declares_everything_the_backend_needs() -> Result<(), String> {
    if !SELECTED {
        return Ok(());
    }
    none_of(&config_problems(CONFIG))?;
    none_of(&toolchain_problems(TOOLCHAIN))?;
    none_of(&cranelift_workflow_problems())
}

/// Scenario: configurations that select the backend with and without the feature, and lookalikes.
///
/// Invariant: both declarations are needed, in their own tables, and a comment declares nothing.
#[test]
fn the_configuration_reader_wants_the_feature_and_the_selection() {
    assert!(config_problems(CONFIG_OK).is_empty());
    assert_eq!(config_problems(CONFIG_NO_FEATURE).len(), 1);
    assert_eq!(config_problems(CONFIG_FEATURE_OFF).len(), 1);
    assert_eq!(config_problems(CONFIG_OTHER_BACKEND).len(), 1);
    assert_eq!(config_problems(CONFIG_WRONG_TABLE).len(), 1);
    assert_eq!(config_problems(CONFIG_COMMENTED).len(), 2);
    assert!(selects_cranelift(CONFIG_OK) && !selects_cranelift(CONFIG_OTHER_BACKEND));
}

/// Scenario: toolchain files that list, omit, comment out or only resemble the component.
///
/// Invariant: only a listed `rustc-codegen-cranelift-preview` counts.
#[test]
fn the_toolchain_reader_wants_the_component() {
    assert!(toolchain_problems(TOOLCHAIN_OK).is_empty());
    for lacking in [TOOLCHAIN_LACKING, TOOLCHAIN_COMMENTED, TOOLCHAIN_LOOKALIKE] {
        assert_eq!(toolchain_problems(lacking).len(), 1, "{lacking}");
    }
}

/// Scenario: `install-whitaker` steps with and without the input.
///
/// Invariant: a step must pass `cranelift: 'true'` itself; a sibling step's input does not count
/// and a comment naming the action is not a step.
#[test]
fn the_whitaker_reader_wants_the_input_on_each_step() {
    assert_eq!(whitaker_count(WHITAKER_OK), 0);
    assert_eq!(whitaker_count(WHITAKER_BARE), 0);
    for refused in [STEP, WHITAKER_OFF, WHITAKER_BEFORE_A_SIBLING] {
        assert_eq!(whitaker_count(refused), 1, "{refused}");
    }
    assert_eq!(whitaker_count(WHITAKER_COMMENT_ONLY), 0);
}
