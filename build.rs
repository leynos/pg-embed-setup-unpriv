//! Build-time cfg aliases shared across crate targets and tests.

#![recursion_limit = "1024"]

fn main() {
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
