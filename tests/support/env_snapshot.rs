//! Captures environment state for behavioural assertions.

use std::ffi::OsString;

use pg_embedded_setup_unpriv::TestBootstrapEnvironment;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvSnapshot {
    pub pgpassfile: Option<OsString>,
    pub tzdir: Option<OsString>,
    pub timezone: Option<OsString>,
}

impl EnvSnapshot {
    pub fn capture() -> Self {
        Self {
            pgpassfile: std::env::var_os("PGPASSFILE"),
            tzdir: std::env::var_os("TZDIR"),
            timezone: std::env::var_os("TZ"),
        }
    }

    pub fn from_environment(environment: &TestBootstrapEnvironment) -> Self {
        environment
            .to_env()
            .into_iter()
            .fold(Self::default(), |mut snapshot, (key, value)| {
                match (key.as_str(), value) {
                    ("PGPASSFILE", Some(env_value)) => {
                        snapshot.pgpassfile = Some(OsString::from(env_value));
                    }
                    ("PGPASSFILE", None) => snapshot.pgpassfile = None,
                    ("TZDIR", Some(env_value)) => {
                        snapshot.tzdir = Some(OsString::from(env_value));
                    }
                    ("TZDIR", None) => snapshot.tzdir = None,
                    ("TZ", Some(env_value)) => {
                        snapshot.timezone = Some(OsString::from(env_value));
                    }
                    ("TZ", None) => snapshot.timezone = None,
                    _ => {}
                }
                snapshot
            })
    }
}

#[cfg(test)]
mod tests {
    use camino::Utf8PathBuf;

    use super::*;

    #[test]
    fn from_environment_maps_supported_variables() {
        let environment = TestBootstrapEnvironment {
            home: Utf8PathBuf::from("/tmp/home"),
            xdg_cache_home: Utf8PathBuf::from("/tmp/cache"),
            xdg_runtime_dir: Utf8PathBuf::from("/tmp/run"),
            pgpass_file: Utf8PathBuf::from("/tmp/home/.pgpass"),
            tz_dir: Some(Utf8PathBuf::from("/usr/share/zoneinfo")),
            timezone: "UTC".into(),
        };

        let snapshot = EnvSnapshot::from_environment(&environment);

        assert_eq!(
            snapshot.pgpassfile,
            Some(OsString::from("/tmp/home/.pgpass"))
        );
        assert_eq!(snapshot.tzdir, Some(OsString::from("/usr/share/zoneinfo")));
        assert_eq!(snapshot.timezone, Some(OsString::from("UTC")));
    }
}
