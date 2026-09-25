//! Tests for choosing one artefact for the running server and target.

use camino::Utf8PathBuf;
use postgresql_embedded::Version;
use rstest::rstest;

use super::{path_source, sample_manifest};
use crate::{
    error::BootstrapErrorKind,
    extensions::{ArtifactQuery, ExtensionName, Manifest, compile_target},
};

/// Selection matches name, running major and minor, and target.
///
/// The two minor cases are the ones that carry the rule. An archive built for
/// one major would load into any minor of it, so selection is narrower than
/// loading on purpose: the manifest pins one digest per name, version and
/// target, and accepting a neighbouring minor would install bytes the pinned
/// digest does not describe for the running server. Both directions are
/// driven, older and newer, because a comparison written as an inequality
/// would refuse one and accept the other.
///
/// The patch case stays an acceptance: Theseus's third component is a build
/// number, not a `PostgreSQL` release, so it is not a match key.
#[rstest]
#[case::match_(Version::new(17, 11, 0), true, "fixture", true)]
#[case::patch_ignored(Version::new(17, 11, 7), true, "fixture", true)]
#[case::older_minor(Version::new(17, 10, 0), true, "fixture", false)]
#[case::newer_minor(Version::new(17, 12, 3), true, "fixture", false)]
#[case::major_mismatch(Version::new(18, 11, 0), true, "fixture", false)]
#[case::target_mismatch(Version::new(17, 11, 0), false, "fixture", false)]
#[case::unknown_name(Version::new(17, 11, 0), true, "other", false)]
fn selection_rules(
    #[case] running: Version,
    #[case] same_target: bool,
    #[case] raw_name: &str,
    #[case] expected: bool,
) {
    let manifest =
        Manifest::parse(sample_manifest().expect("fixture").as_bytes()).expect("valid manifest");
    let target = if same_target {
        compile_target().to_owned()
    } else {
        "mips64-unknown-linux-gnuabi64".to_owned()
    };
    let name = ExtensionName::new(raw_name).expect("valid name");
    let query = ArtifactQuery {
        name: &name,
        running: &running,
        target: &target,
    };
    let source = path_source(Utf8PathBuf::from("/srv/manifest.json"));
    match manifest.select(query, &source) {
        Ok(selection) => {
            assert!(expected, "unexpected match");
            assert_eq!(selection.extension.name, "fixture");
        }
        Err(err) => {
            assert!(!expected, "unexpected miss: {err}");
            assert_eq!(err.kind(), BootstrapErrorKind::ExtensionUnavailable);
            assert!(err.to_string().contains("/srv/manifest.json"), "{err}");
        }
    }
}

/// The unavailable message names the running release and lists the offers.
///
/// The running version is named to both components, because the minor is a
/// match key: a message reading "`PostgreSQL` 16" beside an offer of 17.11.0
/// leaves a reader unable to tell an unmatched major from an unmatched
/// minor, and the second is the case that will actually be hit.
#[test]
fn unavailable_message_lists_offers() {
    let manifest =
        Manifest::parse(sample_manifest().expect("fixture").as_bytes()).expect("valid manifest");
    let name = ExtensionName::new("fixture").expect("valid");
    let running = Version::new(16, 15, 0);
    let query = ArtifactQuery {
        name: &name,
        running: &running,
        target: compile_target(),
    };
    let err = manifest
        .select(query, &path_source(Utf8PathBuf::from("m.json")))
        .expect_err("no 16.x artifact");
    let message = err.to_string();
    assert!(message.contains("PostgreSQL 16.15 on"), "{message}");
    assert!(
        message.contains(&format!("17.11.0 on {}", compile_target())),
        "{message}"
    );
}
