//! World state and fixture builders for the extension install scenarios.

use std::{cell::RefCell, io::Write};

use camino::{Utf8Path, Utf8PathBuf};
use color_eyre::eyre::{Context, Result, eyre};
use flate2::{Compression, write::GzEncoder};
use pg_embedded_setup_unpriv::{
    BootstrapError,
    BootstrapErrorKind,
    extensions::{
        ExtensionName,
        ExtensionRequest,
        InstalledExtension,
        ManifestSource,
        Sha256Hex,
        compile_target,
    },
};
use tar::{EntryType, Header};

/// Version directory the scratch tree is named after.
pub const PG_VERSION: &str = "17.11.0";

/// Regular files the fixture archive carries.
pub const FIXTURE_FILES: [(&str, &[u8]); 3] = [
    ("lib/fixture.so", b"\x7fELF-not-really"),
    (
        "share/extension/fixture.control",
        b"default_version = '1.0'\nmodule_pathname = '$libdir/fixture'\n",
    ),
    (
        "share/extension/fixture--1.0.sql",
        b"CREATE FUNCTION fixture() RETURNS int AS 'MODULE_PATHNAME' LANGUAGE C;\n",
    ),
];

/// State shared by the steps of one scenario.
pub struct ExtensionWorld {
    _temp: tempfile::TempDir,
    pub install_dir: Utf8PathBuf,
    pub cache_dir: Utf8PathBuf,
    pub manifest_path: Utf8PathBuf,
    pub archive_bytes: Vec<u8>,
    pub names: Vec<&'static str>,
    pub result: Option<Result<Vec<InstalledExtension>, BootstrapError>>,
    pub inode_before: Option<u64>,
}

/// Fixture type shared with the scenario functions.
pub type ExtensionWorldFixture = Result<RefCell<ExtensionWorld>>;

/// Borrows the world or surfaces the fixture error.
///
/// # Errors
///
/// Returns the fixture construction error.
pub fn borrow_world(world: &ExtensionWorldFixture) -> Result<&RefCell<ExtensionWorld>> {
    world
        .as_ref()
        .map_err(|err| eyre!("extension world fixture failed: {err}"))
}

impl ExtensionWorld {
    /// Creates the scratch tree, the fixture archive in the cache and a manifest.
    ///
    /// # Errors
    ///
    /// Returns an error when the temporary tree cannot be created.
    pub fn new() -> Result<Self> {
        let temp = tempfile::tempdir().context("create scratch tempdir")?;
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf())
            .map_err(|path| eyre!("tempdir is not UTF-8: {}", path.display()))?;
        let install_dir = root.join(PG_VERSION);
        for sub in ["bin", "lib", "share/extension"] {
            std::fs::create_dir_all(install_dir.join(sub))?;
        }
        let cache_dir = root.join("ext-cache");
        let archive_bytes = fixture_archive(false)?;
        let mut world = Self {
            _temp: temp,
            install_dir,
            cache_dir,
            manifest_path: root.join("manifest.json"),
            archive_bytes,
            names: vec!["fixture"],
            result: None,
            inode_before: None,
        };
        world.publish(None)?;
        Ok(world)
    }

    /// Writes the archive into the cache and a manifest describing it,
    /// optionally recording a different digest than the archive has.
    ///
    /// # Errors
    ///
    /// Returns an error when the files cannot be written.
    pub fn publish(&mut self, digest_override: Option<Sha256Hex>) -> Result<()> {
        let real_digest = Sha256Hex::of_bytes(&self.archive_bytes);
        let recorded = digest_override.unwrap_or_else(|| real_digest.clone());
        let entry_dir = self.cache_dir.join(real_digest.as_str());
        std::fs::create_dir_all(&entry_dir)?;
        std::fs::write(entry_dir.join("fixture.tar.gz"), &self.archive_bytes)?;
        let files: Vec<&str> = FIXTURE_FILES.iter().map(|(name, _)| *name).collect();
        let manifest = serde_json::json!({
            "schema_version": 1,
            "release": "v1.0.0",
            "generated_at": "2026-09-05T00:00:00+00:00",
            "extensions": [{
                "name": "fixture",
                "package": "fixture-pkg",
                "version": "1.0.0",
                "source": {
                    "repository": "https://example.invalid/fixture",
                    "tag": "v1.0.0",
                    "commit": "0123456789abcdef0123456789abcdef01234567",
                },
                "artifacts": [{
                    "postgresql": PG_VERSION,
                    "target": compile_target(),
                    "file": "fixture.tar.gz",
                    "url": "http://127.0.0.1:9/unreachable/fixture.tar.gz",
                    "sha256": recorded.as_str(),
                    "size": self.archive_bytes.len(),
                    "files": files,
                }],
            }],
        });
        std::fs::write(&self.manifest_path, manifest.to_string())?;
        Ok(())
    }

    /// Builds the request the scenario installs.
    ///
    /// # Errors
    ///
    /// Returns an error when a name is invalid.
    pub fn request(&self) -> Result<ExtensionRequest> {
        let names = self
            .names
            .iter()
            .map(|name| ExtensionName::new(*name).map_err(|err| eyre!("{err}")))
            .collect::<Result<Vec<_>>>()?;
        Ok(ExtensionRequest {
            names,
            manifest: ManifestSource::Path {
                path: self.manifest_path.clone(),
                sha256: None,
            },
            cache_dir: self.cache_dir.clone(),
        })
    }

    /// Returns the kind of the recorded failure.
    ///
    /// # Errors
    ///
    /// Returns an error when no install ran or it succeeded.
    pub fn failure_kind(&self) -> Result<BootstrapErrorKind> {
        match &self.result {
            Some(Err(err)) => Ok(err.kind()),
            Some(Ok(_)) => Err(eyre!("install succeeded but a failure was expected")),
            None => Err(eyre!("no install has run")),
        }
    }

    /// Returns whether any fixture file exists in the scratch tree.
    #[must_use]
    pub fn tree_has_fixture_files(&self) -> bool {
        FIXTURE_FILES
            .iter()
            .any(|(name, _)| self.install_dir.join(name).exists())
    }
}

/// Builds the fixture gzip tar, optionally adding an entry that escapes `lib/`.
///
/// # Errors
///
/// Returns an error when the archive cannot be encoded.
pub fn fixture_archive(with_escape: bool) -> Result<Vec<u8>> {
    let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
    for (name, body) in FIXTURE_FILES {
        append_raw(&mut builder, name, body)?;
    }
    if with_escape {
        append_raw(&mut builder, "lib/../bin/evil", b"x")?;
    }
    let encoder = builder.into_inner().context("finish tar")?;
    encoder.finish().context("finish gzip")
}

fn append_raw<W: Write>(builder: &mut tar::Builder<W>, name: &str, body: &[u8]) -> Result<()> {
    let mut header = Header::new_gnu();
    header.set_entry_type(EntryType::Regular);
    header.set_size(body.len() as u64);
    header.set_mode(0o600);
    let gnu = header
        .as_gnu_mut()
        .ok_or_else(|| eyre!("header is not GNU"))?;
    let bytes = name.as_bytes();
    gnu.name
        .get_mut(..bytes.len())
        .ok_or_else(|| eyre!("name too long for header"))?
        .copy_from_slice(bytes);
    header.set_cksum();
    builder.append(&header, body).context("append entry")?;
    Ok(())
}

/// Returns the inode of a file for identity comparisons.
///
/// # Errors
///
/// Returns an error when the file cannot be inspected.
#[cfg(unix)]
pub fn inode_of(path: &Utf8Path) -> Result<u64> {
    use std::os::unix::fs::MetadataExt;
    Ok(std::fs::metadata(path)?.ino())
}

/// Non-Unix stand-in: identity cannot be observed, so report zero.
///
/// # Errors
///
/// Never fails.
#[cfg(not(unix))]
pub fn inode_of(_path: &Utf8Path) -> Result<u64> { Ok(0) }
