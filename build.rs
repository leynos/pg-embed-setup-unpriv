//! Build-time cfg aliases shared across crate targets and tests.

#![recursion_limit = "1024"]

fn main() {
    export_target_triple();
    cfg_aliases::cfg_aliases! {
        privileged_unix_platform: {
            any(
                target_os = "linux",
                target_os = "android",
                target_os = "freebsd",
                target_os = "openbsd",
                target_os = "dragonfly"
            )
        },
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
