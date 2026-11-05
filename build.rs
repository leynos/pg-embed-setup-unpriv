//! Build script: exports the compile target and emits the
//! `privileged_unix_platform` cfg for the targets that can drop privileges.

#![recursion_limit = "1024"]

#[path = "build_support/target_os.rs"]
mod target_os;

fn main() {
    export_target_triple();
    println!("cargo:rustc-check-cfg=cfg(privileged_unix_platform)");
    println!("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_OS");

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").ok();
    if target_os::is_privileged_unix_platform(target_os.as_deref()) {
        println!("cargo:rustc-cfg=privileged_unix_platform");
    }
}

/// Exposes the compile target so the extension hook can match manifest
/// artifacts by the same triple Theseus uses in its asset names.
fn export_target_triple() {
    let Ok(target) = std::env::var("TARGET") else {
        panic!("cargo always sets TARGET for build scripts");
    };
    println!("cargo:rustc-env=PG_EMBED_TARGET={target}");
    println!("cargo:rerun-if-env-changed=TARGET");
}
