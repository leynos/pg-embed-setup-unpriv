//! Contract test for the development build footprint.
//!
//! The Concordat `rust-dev-build-footprint` rule needs the default development
//! profile to keep no incremental state and no debug information, and needs the
//! native Cargo setting that builds without embedded metadata on a dated nightly.
//! The first two live in `Cargo.toml` and the last in `.cargo/config.toml`, so an
//! edit to either file must not quietly re-enable them. The reader works on text
//! it is handed, so the tests also feed it altered files and require a complaint.

const MANIFEST: &str = include_str!("../Cargo.toml");
const CONFIG: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/.cargo/config.toml"));

/// Returns the uncommented, trimmed lines of one table, up to the next table header.
fn table<'a>(text: &'a str, header: &str) -> Vec<&'a str> {
    text.lines()
        .map(str::trim)
        .skip_while(|line| *line != header)
        .skip(1)
        .take_while(|line| !line.starts_with('['))
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect()
}

/// Returns the complaints about a manifest and a Cargo configuration.
fn footprint_problems(manifest: &str, config: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let dev = table(manifest, "[profile.dev]");
    for setting in ["incremental = false", "debug = 0"] {
        if !dev.contains(&setting) {
            problems.push(format!("[profile.dev] must set `{setting}`"));
        }
    }
    if !table(config, "[unstable]").contains(&"embed-metadata = false") {
        problems.push("[unstable] must set `embed-metadata = false`".to_owned());
    }
    problems
}

#[test]
fn the_repository_keeps_the_footprint_settings() {
    assert_eq!(
        footprint_problems(MANIFEST, CONFIG),
        Vec::<String>::new(),
        "the footprint settings drifted"
    );
}

#[test]
fn an_altered_manifest_or_configuration_is_refused() {
    let reenabled = MANIFEST.replace("incremental = false", "incremental = true");
    let debuggable = MANIFEST.replace("debug = 0", "debug = 2");
    let embedded = CONFIG.replace("embed-metadata = false", "embed-metadata = true");
    for (name, manifest, config, expected) in [
        (
            "incremental",
            reenabled.as_str(),
            CONFIG,
            "incremental = false",
        ),
        ("debug", debuggable.as_str(), CONFIG, "debug = 0"),
        (
            "metadata",
            MANIFEST,
            embedded.as_str(),
            "embed-metadata = false",
        ),
    ] {
        let problems = footprint_problems(manifest, config);
        assert!(
            problems.iter().any(|problem| problem.contains(expected)),
            "{name}: an altered file passed: {problems:?}"
        );
    }
}
