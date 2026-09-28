//! Connection helpers for `TestCluster`, including metadata accessors and optional Diesel support.

use camino::{Utf8Path, Utf8PathBuf};
#[cfg(feature = "diesel-support")]
use color_eyre::eyre::WrapErr;
use postgres::{Client, NoTls};
use postgresql_embedded::Settings;

use crate::{TestBootstrapSettings, error::BootstrapResult};

/// Escapes a SQL identifier by doubling embedded double quotes.
///
/// `PostgreSQL` identifiers are quoted with double quotes. Any embedded
/// double quote must be escaped by doubling it.
pub(crate) fn escape_identifier(name: &str) -> String { name.replace('"', "\"\"") }

/// Creates a new `PostgreSQL` client connection from the given URL.
///
/// This is a shared helper for admin database connections used by both
/// `TestClusterConnection` and `TemporaryDatabase`.
pub(crate) fn connect_admin(url: &str) -> BootstrapResult<Client> {
    Client::connect(url, NoTls).map_err(admin_connect_error)
}

/// Wraps a failed admin connection, keeping the driver's error as the source.
///
/// `tokio_postgres` displays a server-side failure as just `db error`, with
/// the reason (such as `password authentication failed`, code `28P01`) in its
/// source. The message therefore names that source too, and the error itself
/// stays in the chain for callers that walk it.
pub(crate) fn admin_connect_error(err: postgres::Error) -> crate::error::BootstrapError {
    let reason = std::error::Error::source(&err)
        .map_or_else(|| err.to_string(), |source| format!("{err}: {source}"));
    let message = format!("failed to connect to admin database: {reason}");
    crate::error::BootstrapError::from(color_eyre::Report::new(err).wrap_err(message))
}

/// Provides ergonomic accessors for connection-oriented cluster metadata.
///
/// # Examples
/// ```no_run
/// use pg_embedded_setup_unpriv::TestCluster;
///
/// # fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
/// let cluster = TestCluster::new()?;
/// let metadata = cluster.connection().metadata();
/// assert_eq!(metadata.host(), "localhost");
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct ConnectionMetadata {
    settings: Settings,
    pgpass_file: Utf8PathBuf,
}

impl ConnectionMetadata {
    pub(crate) fn from_settings(settings: &TestBootstrapSettings) -> Self {
        Self {
            settings: settings.settings.clone(),
            pgpass_file: settings.environment.pgpass_file.clone(),
        }
    }

    /// Returns the configured database host.
    #[must_use]
    pub fn host(&self) -> &str { self.settings.host.as_str() }

    /// Returns the configured port.
    #[must_use]
    pub const fn port(&self) -> u16 { self.settings.port }

    /// Returns the configured superuser name.
    #[must_use]
    pub fn superuser(&self) -> &str { self.settings.username.as_str() }

    /// Returns the generated superuser password.
    #[must_use]
    pub fn password(&self) -> &str { self.settings.password.as_str() }

    /// Returns the prepared `.pgpass` file path.
    #[must_use]
    pub fn pgpass_file(&self) -> &Utf8Path { self.pgpass_file.as_ref() }

    /// Constructs a libpq-compatible URL for `database` using the underlying
    /// `postgresql_embedded` helper.
    #[must_use]
    pub fn database_url(&self, database: &str) -> String { self.settings.url(database) }
}

/// Accessor for connection helpers derived from a
/// [`TestCluster`](crate::TestCluster).
///
/// Enable the `diesel-support` feature to call the Diesel connection helper.
///
/// # Examples
/// ```no_run
/// use pg_embedded_setup_unpriv::TestCluster;
///
/// # fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
/// let cluster = TestCluster::new()?;
/// let url = cluster.connection().database_url("postgres");
/// assert!(url.contains("postgresql://"));
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct TestClusterConnection {
    metadata: ConnectionMetadata,
}

impl TestClusterConnection {
    pub(crate) fn new(settings: &TestBootstrapSettings) -> Self {
        Self {
            metadata: ConnectionMetadata::from_settings(settings),
        }
    }

    /// Returns host metadata without exposing internal storage.
    #[must_use]
    pub fn host(&self) -> &str { self.metadata.host() }

    /// Returns the configured port.
    #[must_use]
    pub const fn port(&self) -> u16 { self.metadata.port() }

    /// Returns the configured superuser account name.
    #[must_use]
    pub fn superuser(&self) -> &str { self.metadata.superuser() }

    /// Returns the generated password for the superuser.
    #[must_use]
    pub fn password(&self) -> &str { self.metadata.password() }

    /// Returns the `.pgpass` file prepared during bootstrap.
    #[must_use]
    pub fn pgpass_file(&self) -> &Utf8Path { self.metadata.pgpass_file() }

    /// Provides an owned snapshot of the connection metadata.
    #[must_use]
    pub fn metadata(&self) -> ConnectionMetadata { self.metadata.clone() }

    /// Builds a libpq-compatible database URL for `database`.
    #[must_use]
    pub fn database_url(&self, database: &str) -> String { self.metadata.database_url(database) }

    /// Establishes a Diesel connection for the target `database`.
    ///
    /// # Errors
    /// Returns a [`crate::error::BootstrapError`] when Diesel cannot connect.
    #[cfg(feature = "diesel-support")]
    pub fn diesel_connection(&self, database: &str) -> BootstrapResult<diesel::PgConnection> {
        use diesel::Connection;

        diesel::PgConnection::establish(&self.database_url(database))
            .wrap_err(format!("failed to connect to {database} via Diesel"))
            .map_err(crate::error::BootstrapError::from)
    }

    /// Connects to the `postgres` administration database.
    pub(super) fn admin_client(&self) -> BootstrapResult<Client> {
        connect_admin(&self.database_url("postgres"))
    }
}

#[cfg(test)]
mod tests {
    //! Tests for cluster connection settings.
    use std::time::Duration;

    use postgresql_embedded::Settings;

    use super::*;
    use crate::{
        CleanupMode,
        TestBootstrapSettings,
        bootstrap::{ExecutionMode, ExecutionPrivileges, TestBootstrapEnvironment},
    };

    fn sample_settings() -> TestBootstrapSettings {
        let settings = Settings {
            host: "127.0.0.1".into(),
            port: 55_321,
            username: "fixture_user".into(),
            password: "fixture_pass".into(),
            data_dir: "/tmp/cluster-data".into(),
            installation_dir: "/tmp/cluster-install".into(),
            ..Settings::default()
        };

        TestBootstrapSettings {
            privileges: ExecutionPrivileges::Unprivileged,
            execution_mode: ExecutionMode::InProcess,
            settings,
            environment: TestBootstrapEnvironment {
                home: Utf8PathBuf::from("/tmp/home"),
                xdg_cache_home: Utf8PathBuf::from("/tmp/home/cache"),
                xdg_runtime_dir: Utf8PathBuf::from("/tmp/home/run"),
                pgpass_file: Utf8PathBuf::from("/tmp/home/.pgpass"),
                tz_dir: Some(Utf8PathBuf::from("/usr/share/zoneinfo")),
                timezone: "UTC".into(),
            },
            worker_binary: None,
            setup_timeout: Duration::from_secs(1),
            start_timeout: Duration::from_secs(1),
            shutdown_timeout: Duration::from_secs(1),
            cleanup_mode: CleanupMode::default(),
            binary_cache_dir: None,
            extensions: None,
            installed_extensions: Vec::new(),
        }
    }

    #[test]
    fn metadata_reflects_underlying_settings() {
        let settings = sample_settings();
        let connection = TestClusterConnection::new(&settings);
        let metadata = connection.metadata();

        assert_eq!(metadata.host(), "127.0.0.1");
        assert_eq!(metadata.port(), 55_321);
        assert_eq!(metadata.superuser(), "fixture_user");
        assert_eq!(metadata.password(), "fixture_pass");
        assert_eq!(metadata.pgpass_file(), Utf8Path::new("/tmp/home/.pgpass"));
    }

    #[test]
    fn database_url_matches_postgresql_embedded() {
        let settings = sample_settings();
        let connection = TestClusterConnection::new(&settings);
        let expected = settings.settings.url("postgres");

        assert_eq!(connection.database_url("postgres"), expected);
    }

    /// Listens on a loopback port and serves one connection that rejects the
    /// password, returning the address to connect to.
    fn reject_password_once() -> std::io::Result<std::net::SocketAddr> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        std::thread::spawn(move || {
            // A failure here leaves the client with a transport error, which
            // the test's message assertion then reports.
            let _served = answer_with_28p01(&listener);
        });
        Ok(address)
    }

    /// Reads one startup message and answers it with a server-side `28P01`
    /// error, as a server rejecting the password does.
    #[expect(
        clippy::big_endian_bytes,
        reason = "the PostgreSQL wire protocol frames every length in network byte order"
    )]
    fn answer_with_28p01(listener: &std::net::TcpListener) -> std::io::Result<()> {
        use std::io::{Error, Read, Write};

        let (mut stream, _) = listener.accept()?;
        let mut length = [0_u8; 4];
        stream.read_exact(&mut length)?;
        let body_len = usize::try_from(u32::from_be_bytes(length))
            .map_err(Error::other)?
            .saturating_sub(4);
        let mut body = vec![0_u8; body_len];
        stream.read_exact(&mut body)?;
        let mut fields = Vec::new();
        for (code, value) in [
            (b'S', "FATAL"),
            (b'V', "FATAL"),
            (b'C', "28P01"),
            (b'M', "password authentication failed for user \"postgres\""),
        ] {
            fields.push(code);
            fields.extend_from_slice(value.as_bytes());
            fields.push(0);
        }
        fields.push(0);
        let reply_len = u32::try_from(fields.len() + 4).map_err(Error::other)?;
        let mut reply = vec![b'E'];
        reply.extend_from_slice(&reply_len.to_be_bytes());
        reply.extend_from_slice(&fields);
        stream.write_all(&reply)
    }

    /// A server-side failure, which `tokio_postgres` displays as just
    /// `db error`, reaches the message with its cause and stays in the chain.
    #[test]
    fn a_rejected_password_names_its_cause() {
        let address = reject_password_once().expect("a loopback port");
        let url = format!("postgresql://postgres:wrong@{address}/postgres");

        let err = connect_admin(&url)
            .err()
            .expect("the server rejects the password");

        let message = err.to_string();
        assert!(
            message.contains("db error: FATAL: password authentication failed"),
            "the nested cause must reach the message: {message}"
        );
        let report = err.into_report();
        let driver = report
            .chain()
            .find_map(|cause| cause.downcast_ref::<postgres::Error>())
            .expect("the postgres error stays in the chain");
        let code = driver.code().map(postgres::error::SqlState::code);
        assert_eq!(
            code,
            Some("28P01"),
            "the server's code survives: {report:?}"
        );
    }

    /// A failed admin connection keeps the driver's error in the chain and
    /// names its cause, so a caller sees why rather than a bare `db error`.
    #[test]
    fn a_failed_admin_connection_keeps_its_source() {
        // Port 1 on loopback refuses at once; no server is involved.
        let err = connect_admin("postgresql://postgres:x@127.0.0.1:1/postgres")
            .err()
            .expect("nothing listens on port 1");

        let message = err.to_string();
        let report = err.into_report();
        let chain_has_driver_error = report
            .chain()
            .any(|cause| cause.downcast_ref::<postgres::Error>().is_some());
        assert!(chain_has_driver_error, "the source was dropped: {report:?}");
        assert!(
            message.starts_with("failed to connect to admin database: "),
            "{message}"
        );
        assert!(
            message.to_lowercase().contains("refused"),
            "the cause must reach the message: {message}"
        );
    }
}
