//! Compile-time checks for feature-gated public test surfaces.

// A single trybuild invocation shares its generated Cargo project and avoids
// six simultaneous Cargo processes contending for the same build and cache locks.
#[test]
#[cfg(not(windows))]
fn public_surfaces_compile() {
    let tests = trybuild::TestCases::new();
    tests.pass("tests/ui/pass/shutdown_hook_test_support.rs");
    tests.pass("tests/ui/pass/install_root_surface.rs");
    tests.pass("tests/ui/pass/password_reuse_surface.rs");
    tests.pass("tests/ui/pass/observability_surface.rs");
    tests.pass("tests/ui/pass/extensions_compile_target.rs");
    tests.pass("tests/ui/pass/const_connection_accessors.rs");
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

#[cfg(windows)]
#[path = "ui/pass/password_reuse_surface.rs"]
mod password_reuse_surface;

#[test]
#[cfg(windows)]
fn password_reuse_surface_smoke_compiles() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
    password_reuse_surface::verify_surface()
}

#[cfg(windows)]
#[path = "ui/pass/observability_surface.rs"]
mod observability_surface;

#[test]
#[cfg(windows)]
fn observability_surface_smoke_compiles() { observability_surface::verify_surface(); }

#[cfg(windows)]
#[path = "ui/pass/extensions_compile_target.rs"]
mod extensions_compile_target;

#[test]
#[cfg(windows)]
fn extensions_public_surface_smoke_compiles() {
    extensions_compile_target::verify_surface().expect("extension surface should compile and run");
}

#[cfg(windows)]
#[path = "ui/pass/const_connection_accessors.rs"]
mod const_connection_accessors;

#[test]
#[cfg(windows)]
fn const_connection_accessors_smoke_compiles() { const_connection_accessors::verify_surface(); }
