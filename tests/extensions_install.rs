//! Behavioural coverage for installing prebuilt extension archives into a
//! scratch `PostgreSQL` tree through `install_extensions`.

use std::cell::RefCell;

use color_eyre::eyre::{Result, ensure, eyre};
use pg_embedded_setup_unpriv::{BootstrapErrorKind, extensions::install_extensions};
use rstest::fixture;
use rstest_bdd_macros::{given, scenario, then, when};

#[path = "support/extensions_install_helpers.rs"]
mod extensions_install_helpers;
#[path = "support/scenario.rs"]
mod scenario;

use extensions_install_helpers::{
    ExtensionWorld,
    ExtensionWorldFixture,
    FIXTURE_FILES,
    borrow_world,
    fixture_archive,
    inode_of,
};
use pg_embedded_setup_unpriv::extensions::Sha256Hex;
use scenario::expect_fixture;

#[fixture]
fn world() -> ExtensionWorldFixture {
    let world = ExtensionWorld::new()?;
    Ok(RefCell::new(world))
}

#[given("a scratch PostgreSQL tree and a manifest describing a fixture archive")]
fn given_scratch_tree(world: &ExtensionWorldFixture) -> Result<()> {
    let state = borrow_world(world)?.borrow();
    ensure!(
        state.install_dir.join("lib").is_dir(),
        "scratch tree missing lib/"
    );
    ensure!(state.manifest_path.is_file(), "manifest missing");
    Ok(())
}

#[given("the manifest records the wrong archive digest")]
fn given_wrong_digest(world: &ExtensionWorldFixture) -> Result<()> {
    let mut state = borrow_world(world)?.borrow_mut();
    state.publish(Some(Sha256Hex::of_bytes(b"not the archive")))
}

#[given("the request also names an extension the manifest lacks")]
fn given_unknown_name(world: &ExtensionWorldFixture) -> Result<()> {
    borrow_world(world)?.borrow_mut().names.push("missing");
    Ok(())
}

#[given("the manifest file is removed")]
fn given_manifest_removed(world: &ExtensionWorldFixture) -> Result<()> {
    let state = borrow_world(world)?.borrow();
    std::fs::remove_file(&state.manifest_path)?;
    Ok(())
}

#[given("the archive gains an entry that escapes to the parent directory")]
fn given_escaping_entry(world: &ExtensionWorldFixture) -> Result<()> {
    let mut state = borrow_world(world)?.borrow_mut();
    state.archive_bytes = fixture_archive(true)?;
    state.publish(None)
}

fn run_install(world: &ExtensionWorldFixture) -> Result<()> {
    let cell = borrow_world(world)?;
    let request = cell.borrow().request()?;
    let install_dir = cell.borrow().install_dir.clone();
    let outcome = install_extensions(&request, &install_dir);
    cell.borrow_mut().result = Some(outcome);
    Ok(())
}

#[when("the declared extensions are installed")]
fn when_installed(world: &ExtensionWorldFixture) -> Result<()> { run_install(world) }

#[when("the declared extensions are installed again")]
fn when_installed_again(world: &ExtensionWorldFixture) -> Result<()> {
    {
        let mut world_ref = borrow_world(world)?.borrow_mut();
        let path = world_ref.install_dir.join("lib/fixture.so");
        world_ref.inode_before = Some(inode_of(&path)?);
    }
    run_install(world)
}

#[then("the three fixture files exist with library and share modes")]
fn then_files_exist(world: &ExtensionWorldFixture) -> Result<()> {
    let state = borrow_world(world)?.borrow();
    for (name, body) in FIXTURE_FILES {
        let path = state.install_dir.join(name);
        ensure!(std::fs::read(&path)? == body, "{name} content differs");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path)?.permissions().mode() & 0o777;
            let expected = if name.starts_with("lib/") {
                0o755
            } else {
                0o644
            };
            ensure!(
                mode == expected,
                "{name} has mode {mode:o}, expected {expected:o}"
            );
        }
    }
    Ok(())
}

#[then("the report lists the fixture extension with three files")]
fn then_report_lists(world: &ExtensionWorldFixture) -> Result<()> {
    let state = borrow_world(world)?.borrow();
    let installed = match &state.result {
        Some(Ok(installed)) => installed,
        Some(Err(err)) => return Err(eyre!("install failed: {err}")),
        None => return Err(eyre!("no install ran")),
    };
    ensure!(
        installed.len() == 1,
        "expected one report, got {}",
        installed.len()
    );
    let report = installed.first().ok_or_else(|| eyre!("empty report"))?;
    ensure!(report.name.as_str() == "fixture", "unexpected name");
    ensure!(report.files.len() == 3, "expected three files");
    Ok(())
}

#[then("the fixture files are unchanged")]
fn then_files_unchanged(world: &ExtensionWorldFixture) -> Result<()> {
    let state = borrow_world(world)?.borrow();
    let after = inode_of(&state.install_dir.join("lib/fixture.so"))?;
    ensure!(
        state.inode_before == Some(after),
        "an identical file was rewritten"
    );
    Ok(())
}

#[then("the install fails with kind ExtensionArchiveUnavailable")]
fn then_archive_unavailable(world: &ExtensionWorldFixture) -> Result<()> {
    expect_kind(world, BootstrapErrorKind::ExtensionArchiveUnavailable)
}

#[then("the install fails with kind ExtensionUnavailable")]
fn then_unavailable(world: &ExtensionWorldFixture) -> Result<()> {
    expect_kind(world, BootstrapErrorKind::ExtensionUnavailable)
}

#[then("the install fails with kind ExtensionManifestUnavailable")]
fn then_manifest_unavailable(world: &ExtensionWorldFixture) -> Result<()> {
    expect_kind(world, BootstrapErrorKind::ExtensionManifestUnavailable)
}

#[then("the install fails with kind ExtensionArchiveInvalid")]
fn then_archive_invalid(world: &ExtensionWorldFixture) -> Result<()> {
    expect_kind(world, BootstrapErrorKind::ExtensionArchiveInvalid)
}

fn expect_kind(world: &ExtensionWorldFixture, expected: BootstrapErrorKind) -> Result<()> {
    let kind = borrow_world(world)?.borrow().failure_kind()?;
    ensure!(kind == expected, "expected {expected:?}, got {kind:?}");
    Ok(())
}

#[then("the scratch tree is untouched")]
fn then_tree_untouched(world: &ExtensionWorldFixture) -> Result<()> {
    let state = borrow_world(world)?.borrow();
    ensure!(
        !state.tree_has_fixture_files(),
        "files were written despite the failure"
    );
    Ok(())
}

#[scenario(path = "tests/features/extensions_install.feature", index = 0)]
fn scenario_install(world: ExtensionWorldFixture) {
    let _ = expect_fixture(world, "extension install world");
}

#[scenario(path = "tests/features/extensions_install.feature", index = 1)]
fn scenario_reinstall(world: ExtensionWorldFixture) {
    let _ = expect_fixture(world, "extension install world");
}

#[scenario(path = "tests/features/extensions_install.feature", index = 2)]
fn scenario_digest_mismatch(world: ExtensionWorldFixture) {
    let _ = expect_fixture(world, "extension install world");
}

#[scenario(path = "tests/features/extensions_install.feature", index = 3)]
fn scenario_unknown_name(world: ExtensionWorldFixture) {
    let _ = expect_fixture(world, "extension install world");
}

#[scenario(path = "tests/features/extensions_install.feature", index = 4)]
fn scenario_missing_manifest(world: ExtensionWorldFixture) {
    let _ = expect_fixture(world, "extension install world");
}

#[scenario(path = "tests/features/extensions_install.feature", index = 5)]
fn scenario_escaping_entry(world: ExtensionWorldFixture) {
    let _ = expect_fixture(world, "extension install world");
}
