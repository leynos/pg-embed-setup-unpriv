//! Build-time cfg aliases shared across crate targets and tests.

#![recursion_limit = "1024"]

fn main() {
    export_target_triple();
    println!("cargo:rustc-check-cfg=cfg(privileged_unix_platform)");
    println!("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_OS");

    if is_privileged_unix_platform() {
        println!("cargo:rustc-cfg=privileged_unix_platform");
    }
}

fn is_privileged_unix_platform() -> bool {
    std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|target_os| {
        matches!(
            target_os.as_str(),
            "linux" | "android" | "freebsd" | "openbsd" | "dragonfly"
        )
    })
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
