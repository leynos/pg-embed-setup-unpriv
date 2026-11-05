//! Target-OS classification shared by `build.rs` and its regression tests.

/// Operating systems on which the crate can drop privileges.
pub const PRIVILEGED_UNIX_TARGET_OS: [&str; 5] =
    ["linux", "android", "freebsd", "openbsd", "dragonfly"];

/// Reports whether `target_os` (Cargo's `CARGO_CFG_TARGET_OS`, if set) names a
/// platform that can drop privileges. An absent or unknown value is `false`.
pub fn is_privileged_unix_platform(target_os: Option<&str>) -> bool {
    target_os.is_some_and(|os| PRIVILEGED_UNIX_TARGET_OS.contains(&os))
}
