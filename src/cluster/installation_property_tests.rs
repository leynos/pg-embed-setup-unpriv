//! Property tests for selecting versioned PostgreSQL installations.

use std::fs;

use postgresql_embedded::{Settings, VersionReq};
use proptest::prelude::*;

use super::resolve_installed_dir;

/// Seed a tree in the supplied order and return the name selected from it.
fn select_from_tree(
    names: &[String],
    requirement: &VersionReq,
) -> Result<Option<String>, TestCaseError> {
    let temp = tempfile::tempdir()
        .map_err(|err| TestCaseError::fail(format!("create temporary directory: {err}")))?;
    for name in names {
        fs::create_dir_all(temp.path().join(name).join("bin"))
            .map_err(|err| TestCaseError::fail(format!("create candidate: {err}")))?;
    }
    // A directory without bin must never be considered, even if its name is
    // greater than every generated valid version or lexical fallback.
    fs::create_dir_all(temp.path().join("99.0.0"))
        .map_err(|err| TestCaseError::fail(format!("create incomplete candidate: {err}")))?;
    let settings = Settings {
        installation_dir: temp.path().to_path_buf(),
        version: requirement.clone(),
        ..Settings::default()
    };
    Ok(resolve_installed_dir(&settings).and_then(|path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned)
    }))
}

proptest! {
    #[test]
    fn selection_matches_numeric_oracle_and_is_independent_of_creation_order(
        versions in prop::collection::vec((1_u8..9, 0_u8..5, 0_u8..5), 0..8),
        lexical in prop::collection::vec(0_u8..10, 0..4),
        lower in 1_u8..5,
        upper in 5_u8..10,
    ) {
        let requirement = VersionReq::parse(&format!(">={lower}.0.0, <{upper}.0.0"))
            .map_err(|err| TestCaseError::fail(format!("parse requirement: {err}")))?;
        let mut names: Vec<String> = versions.iter()
            .map(|(major, minor, patch)| format!("{major}.{minor}.{patch}"))
            .chain(lexical.iter().map(|number| format!("custom-{number}")))
            .collect();
        // Numeric tuple ordering is independent of semver parsing and the
        // implementation's candidate sorting. The generated versions have no
        // prerelease components, so this is an exact oracle for the range.
        let expected = versions.iter()
            .filter(|(major, _, _)| *major >= lower && *major < upper)
            .max()
            .map(|(major, minor, patch)| format!("{major}.{minor}.{patch}"))
            .or_else(|| lexical.iter().map(|number| format!("custom-{number}")).max());
        let forward = select_from_tree(&names, &requirement)?;
        names.reverse();
        let reverse = select_from_tree(&names, &requirement)?;
        prop_assert_eq!(&forward, &expected);
        prop_assert_eq!(&reverse, &expected);
    }
}
