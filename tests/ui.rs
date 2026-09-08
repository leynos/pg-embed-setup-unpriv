//! Compile-time checks for feature-gated public test surfaces.

#[test]
#[cfg(not(windows))]
fn shutdown_hook_test_support_surface_compiles() {
    let tests = trybuild::TestCases::new();
    tests.pass("tests/ui/pass/shutdown_hook_test_support.rs");
}

#[cfg(windows)]
#[path = "ui/pass/shutdown_hook_test_support.rs"]
mod shutdown_hook_test_support;

#[test]
#[cfg(windows)]
fn shutdown_hook_test_support_surface_smoke_compiles() {
    shutdown_hook_test_support::verify_surface()
        .expect("shutdown-hook test-support surface should compile and run");
}

#[test]
#[cfg(not(windows))]
fn password_reuse_surface_compiles() {
    let tests = trybuild::TestCases::new();
    tests.pass("tests/ui/pass/password_reuse_surface.rs");
}

#[cfg(windows)]
#[path = "ui/pass/password_reuse_surface.rs"]
mod password_reuse_surface;

#[test]
#[cfg(windows)]
fn password_reuse_surface_smoke_compiles() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
    password_reuse_surface::verify_surface()
}

#[test]
#[cfg(not(windows))]
fn observability_surface_compiles() {
    let tests = trybuild::TestCases::new();
    tests.pass("tests/ui/pass/observability_surface.rs");
}

#[cfg(windows)]
#[path = "ui/pass/observability_surface.rs"]
mod observability_surface;

#[test]
#[cfg(windows)]
fn observability_surface_smoke_compiles() { observability_surface::verify_surface(); }
