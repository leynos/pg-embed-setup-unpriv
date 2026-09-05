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
fn extensions_public_surface_compiles() {
    let tests = trybuild::TestCases::new();
    tests.pass("tests/ui/pass/extensions_compile_target.rs");
}

#[cfg(windows)]
#[path = "ui/pass/extensions_compile_target.rs"]
mod extensions_compile_target;

#[test]
#[cfg(windows)]
fn extensions_public_surface_smoke_compiles() { extensions_compile_target::main() }
