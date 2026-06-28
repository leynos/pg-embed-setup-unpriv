//! Compile-time checks for feature-gated public test surfaces.

#[test]
fn shutdown_hook_test_support_surface_compiles() {
    let tests = trybuild::TestCases::new();
    tests.pass("tests/ui/pass/shutdown_hook_test_support.rs");
}
