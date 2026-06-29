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
    shutdown_hook_test_support::verify_surface();
}
