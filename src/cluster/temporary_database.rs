//! RAII guard for automatic database cleanup.
//!
//! `TemporaryDatabase` drops its associated database when the guard goes out of
//! scope, mirroring the `TestCluster` lifecycle semantics.

use color_eyre::eyre::WrapErr;
use tracing::info_span;

use super::connection::{connect_admin, escape_identifier};
use crate::{error::BootstrapResult, observability::LOG_TARGET};

/// RAII guard that drops a database when it goes out of scope.
///
/// The guard stores the database name and connection URL rather than borrowing
/// a connection, avoiding lifetime issues and allowing reconnection in `Drop`.
///
/// # Examples
///
/// ```no_run
/// use pg_embedded_setup_unpriv::TestCluster;
///
/// # fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
/// let cluster = TestCluster::new()?;
///
/// // Create a temporary database that is dropped when the guard is dropped
/// let temp_db = cluster.connection().temporary_database("my_temp_db")?;
///
/// // Use the database
/// let url = temp_db.url();
/// // ... run queries ...
///
/// // Database is dropped automatically when temp_db goes out of scope
/// drop(temp_db);
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct TemporaryDatabase {
    name: String,
    admin_url: String,
    database_url: String,
}

impl TemporaryDatabase {
    /// Creates a new `TemporaryDatabase` guard.
    ///
    /// This constructor is intended for internal use. Prefer using
    /// [`TestClusterConnection::temporary_database`] or
    /// [`TestClusterConnection::temporary_database_from_template`].
    pub(crate) const fn new(name: String, admin_url: String, database_url: String) -> Self {
        Self {
            name,
            admin_url,
            database_url,
        }
    }

    /// Returns the database name.
    #[must_use]
    pub fn name(&self) -> &str { &self.name }

    /// Returns the connection URL for this database.
    #[must_use]
    pub fn url(&self) -> &str { &self.database_url }

    /// Drops the database, failing if connections exist.
    ///
    /// This mirrors `PostgreSQL`'s native behaviour where `DROP DATABASE` fails
    /// if there are active connections to the database.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The database has active connections
    /// - The database does not exist
    /// - The connection to the admin database fails
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use pg_embedded_setup_unpriv::TestCluster;
    ///
    /// # fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
    /// let cluster = TestCluster::new()?;
    /// let temp_db = cluster.connection().temporary_database("my_temp_db")?;
    ///
    /// // Explicitly drop (consumes the guard)
    /// temp_db.drop_database()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn drop_database(self) -> BootstrapResult<()> { self.try_drop() }

    /// Drops the database, terminating any active connections first.
    ///
    /// This is useful when you need to ensure the database is dropped even if
    /// there are lingering connections (e.g., from connection pools that
    /// haven't been drained).
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The database does not exist
    /// - The connection to the admin database fails
    /// - Terminating connections fails
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use pg_embedded_setup_unpriv::TestCluster;
    ///
    /// # fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
    /// let cluster = TestCluster::new()?;
    /// let temp_db = cluster.connection().temporary_database("my_temp_db")?;
    ///
    /// // Force drop even if connections exist
    /// temp_db.force_drop()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn force_drop(self) -> BootstrapResult<()> {
        let _span = info_span!("force_drop_database", db = %self.name).entered();
        let mut client = connect_admin(&self.admin_url)?;

        // Terminate active connections using parameterized query
        client
            .execute(
                "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = $1 AND \
                 pid <> pg_backend_pid()",
                &[&self.name],
            )
            .wrap_err(format!(
                "failed to terminate connections to database '{}'",
                self.name
            ))
            .map_err(crate::error::BootstrapError::from)?;

        // Drop the database with escaped identifier
        let escaped = escape_identifier(&self.name);
        let drop_sql = format!("DROP DATABASE \"{escaped}\"");
        client
            .batch_execute(&drop_sql)
            .wrap_err(format!("failed to drop database '{}'", self.name))
            .map_err(crate::error::BootstrapError::from)?;

        Ok(())
    }

    /// Attempts to drop the database without consuming self.
    ///
    /// Used by the `Drop` implementation for best-effort cleanup.
    fn try_drop(&self) -> BootstrapResult<()> {
        let _span = info_span!("drop_database", db = %self.name).entered();
        let mut client = connect_admin(&self.admin_url)?;

        let escaped = escape_identifier(&self.name);
        let sql = format!("DROP DATABASE \"{escaped}\"");
        client
            .batch_execute(&sql)
            .wrap_err(format!("failed to drop database '{}'", self.name))
            .map_err(crate::error::BootstrapError::from)?;

        Ok(())
    }
}

impl Drop for TemporaryDatabase {
    fn drop(&mut self) {
        if let Err(e) = self.try_drop() {
            tracing::warn!(
                target: LOG_TARGET,
                db = %self.name,
                error = ?e,
                "failed to drop temporary database"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    //! Tests for temporary database handling.
    use super::*;

    #[test]
    fn temporary_database_accessors() {
        let temp = TemporaryDatabase::new(
            "test_db".to_owned(),
            "postgresql://user:pass@localhost:5432/postgres".to_owned(),
            "postgresql://user:pass@localhost:5432/test_db".to_owned(),
        );

        assert_eq!(temp.name(), "test_db");
        assert!(temp.url().contains("test_db"));
    }

    /// An admin URL nothing listens on, with the attempt bounded.
    ///
    /// Two details, and both are load-bearing on Windows. The host is
    /// `127.0.0.1` rather than `localhost`, because the name resolves to
    /// `::1` as well and the client tries the addresses in turn, so a
    /// host that refuses one slowly is waited on before the other is
    /// reached. `connect_timeout` then bounds what is left.
    ///
    /// Without them the test relies on the operating system refusing a
    /// connection promptly, which Linux and macOS do in 0.01 to 0.04 s
    /// and Windows does not: it measured 8.07 s on run 34161062265 and
    /// then exceeded the profile's 180 s per-test allowance on run
    /// 34274541426, ending the whole Windows lane.
    const UNREACHABLE_ADMIN_URL: &str =
        "postgresql://user:pass@127.0.0.1:59999/postgres?connect_timeout=2";

    /// The matching database URL. See [`UNREACHABLE_ADMIN_URL`].
    const UNREACHABLE_DB_URL: &str =
        "postgresql://user:pass@127.0.0.1:59999/test_db?connect_timeout=2";

    #[test]
    fn drop_database_returns_error_on_connection_failure() {
        let temp = TemporaryDatabase::new(
            "test_db".to_owned(),
            UNREACHABLE_ADMIN_URL.to_owned(),
            UNREACHABLE_DB_URL.to_owned(),
        );

        let result = temp.drop_database();
        let Err(err) = result else {
            panic!("expected error when database unreachable");
        };
        let err_str = err.to_string();
        assert!(
            err_str.contains("failed to connect"),
            "expected connection failure, got: {err_str}"
        );
    }

    #[test]
    fn force_drop_returns_error_on_connection_failure() {
        let temp = TemporaryDatabase::new(
            "test_db".to_owned(),
            UNREACHABLE_ADMIN_URL.to_owned(),
            UNREACHABLE_DB_URL.to_owned(),
        );

        let result = temp.force_drop();
        let Err(err) = result else {
            panic!("expected error when database unreachable");
        };
        let err_str = err.to_string();
        assert!(
            err_str.contains("failed to connect"),
            "expected connection failure, got: {err_str}"
        );
    }

    #[test]
    fn drop_trait_does_not_panic_on_connection_failure() {
        // Create a TemporaryDatabase with an unreachable URL
        let temp = TemporaryDatabase::new(
            "test_db".to_owned(),
            UNREACHABLE_ADMIN_URL.to_owned(),
            UNREACHABLE_DB_URL.to_owned(),
        );

        // Dropping should not panic even when cleanup fails
        // The Drop impl logs a warning but does not propagate errors
        drop(temp);
    }
}
