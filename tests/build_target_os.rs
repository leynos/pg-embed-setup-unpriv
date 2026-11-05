//! Coverage for the target-OS classification that drives the
//! `privileged_unix_platform` cfg emitted by `build.rs`.

#[path = "../build_support/target_os.rs"]
mod target_os;

use rstest::rstest;
use target_os::is_privileged_unix_platform;

#[rstest]
#[case("linux")]
#[case("android")]
#[case("freebsd")]
#[case("openbsd")]
#[case("dragonfly")]
fn accepted_operating_systems_are_privileged(#[case] os: &str) {
    assert!(is_privileged_unix_platform(Some(os)));
}

#[rstest]
#[case("macos")]
#[case("windows")]
#[case("netbsd")]
#[case("Linux")]
#[case("")]
fn other_operating_systems_are_not_privileged(#[case] os: &str) {
    assert!(!is_privileged_unix_platform(Some(os)));
}

#[test]
fn an_absent_value_is_not_privileged() {
    assert!(!is_privileged_unix_platform(None));
}
