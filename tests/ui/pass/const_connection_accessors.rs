//! Compile-time fixture for the connection accessors' `const` signatures.
//!
//! `ConnectionMetadata` and `TestClusterConnection` expose their host, port,
//! superuser and password through `const fn` accessors. The unit tests call
//! them at run time, where a plain `fn` would pass too; only a `const fn`
//! wrapper in a consumer crate proves the accessors stay usable in constant
//! contexts, so dropping `const` from any of them breaks this fixture.
//!
//! `tests/ui.rs` uses this as a non-Windows trybuild pass fixture and as a
//! directly included Windows smoke-compile module.

use pg_embedded_setup_unpriv::{ConnectionMetadata, TestClusterConnection};

const fn metadata_host(metadata: &ConnectionMetadata) -> &str { metadata.host() }

const fn metadata_port(metadata: &ConnectionMetadata) -> u16 { metadata.port() }

const fn metadata_superuser(metadata: &ConnectionMetadata) -> &str { metadata.superuser() }

const fn metadata_password(metadata: &ConnectionMetadata) -> &str { metadata.password() }

const fn connection_host(connection: &TestClusterConnection) -> &str { connection.host() }

const fn connection_port(connection: &TestClusterConnection) -> u16 { connection.port() }

const fn connection_superuser(connection: &TestClusterConnection) -> &str { connection.superuser() }

const fn connection_password(connection: &TestClusterConnection) -> &str { connection.password() }

/// Names every wrapper so none is dead code, without starting anything.
pub fn verify_surface() {
    let metadata: [for<'a> fn(&'a ConnectionMetadata) -> &'a str; 3] =
        [metadata_host, metadata_superuser, metadata_password];
    let connection: [for<'a> fn(&'a TestClusterConnection) -> &'a str; 3] =
        [connection_host, connection_superuser, connection_password];
    let ports: (
        fn(&ConnectionMetadata) -> u16,
        fn(&TestClusterConnection) -> u16,
    ) = (metadata_port, connection_port);
    assert_eq!(metadata.len() + connection.len(), 6);
    let _ = ports;
}

#[cfg(not(windows))]
fn main() { verify_surface(); }
