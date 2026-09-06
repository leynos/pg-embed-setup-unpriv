//! Compile-time checks for feature-gated public test surfaces.

#[test]
#[cfg(not(windows))]
fn shutdown_hook_test_support_surface_compiles() {
    let tests = trybuild::TestCases::new();
    tests.pass("tests/ui/pass/shutdown_hook_test_support.rs");
}

#[test]
#[cfg(not(windows))]
fn install_root_surface_compiles() {
    let tests = trybuild::TestCases::new();
    tests.pass("tests/ui/pass/install_root_surface.rs");
}

#[cfg(windows)]
#[path = "ui/pass/shutdown_hook_test_support.rs"]
mod shutdown_hook_test_support;

#[cfg(windows)]
#[path = "ui/pass/install_root_surface.rs"]
mod install_root_surface;

#[test]
#[cfg(windows)]
fn install_root_surface_smoke_compiles() {
    install_root_surface::verify_surface().expect("install-root surface should compile and run");
}

#[test]
#[cfg(windows)]
fn shutdown_hook_test_support_surface_smoke_compiles() {
    shutdown_hook_test_support::verify_surface()
        .expect("shutdown-hook test-support surface should compile and run");
}
