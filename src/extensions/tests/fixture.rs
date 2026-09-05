//! Shared fixtures: in-memory archives, manifests and a one-shot HTTP server.
//!
//! Helpers return `Result` so only the test bodies themselves unwrap.

use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
};

use camino::{Utf8Path, Utf8PathBuf};
use color_eyre::eyre::{Context, Result, eyre};
use flate2::{Compression, write::GzEncoder};
use tar::{EntryType, Header};

use crate::extensions::{ManifestArtifact, Sha256Hex, compile_target};

/// Version directory name used by fixture install trees.
pub(super) const PG_VERSION: &str = "17.11.0";

/// Regular files a well-formed fixture archive carries.
pub(super) const FIXTURE_FILES: [(&str, &[u8]); 3] = [
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

/// One tar entry to write into a fixture archive.
#[derive(Debug, Clone)]
pub(super) enum Entry {
    File(&'static str, &'static [u8]),
    Dir(&'static str),
    Symlink(&'static str, &'static str),
    HardLink(&'static str, &'static str),
}

/// A temporary directory and its UTF-8 path.
pub(super) fn temp_root() -> Result<(tempfile::TempDir, Utf8PathBuf)> {
    let temp = tempfile::tempdir().context("create tempdir")?;
    let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf())
        .map_err(|path| eyre!("tempdir is not UTF-8: {}", path.display()))?;
    Ok((temp, root))
}

/// Builds a gzip tar in memory from `entries`.
pub(super) fn archive_bytes(entries: &[Entry]) -> Result<Vec<u8>> {
    let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
    for entry in entries {
        append_entry(&mut builder, entry)?;
    }
    let encoder = builder.into_inner().context("finish tar")?;
    encoder.finish().context("finish gzip")
}

fn append_entry<W: Write>(builder: &mut tar::Builder<W>, entry: &Entry) -> Result<()> {
    match entry {
        Entry::File(name, body) => append_file(builder, name, body),
        Entry::Dir(name) => append_dir(builder, name),
        Entry::Symlink(name, target) => append_link(builder, EntryType::Symlink, name, target),
        Entry::HardLink(name, target) => append_link(builder, EntryType::Link, name, target),
    }
}

fn append_file<W: Write>(builder: &mut tar::Builder<W>, name: &str, body: &[u8]) -> Result<()> {
    let mut header = Header::new_gnu();
    header.set_entry_type(EntryType::Regular);
    header.set_size(body.len() as u64);
    header.set_mode(0o600);
    // Write the name straight into the header so hostile paths (`..`,
    // absolute) that the tar crate's setters refuse can be exercised exactly
    // as an attacker would ship them.
    set_raw_name(&mut header, name)?;
    builder.append(&header, body).context("append file")
}

fn append_dir<W: Write>(builder: &mut tar::Builder<W>, name: &str) -> Result<()> {
    let mut header = Header::new_gnu();
    header.set_entry_type(EntryType::Directory);
    header.set_size(0);
    header.set_mode(0o755);
    builder
        .append_data(&mut header, name, std::io::empty())
        .context("append dir")
}

fn append_link<W: Write>(
    builder: &mut tar::Builder<W>,
    kind: EntryType,
    name: &str,
    target: &str,
) -> Result<()> {
    let mut header = Header::new_gnu();
    header.set_entry_type(kind);
    header.set_size(0);
    builder
        .append_link(&mut header, name, target)
        .context("append link")
}

/// Copies `name` into the 100-byte GNU name field without validation.
fn set_raw_name(header: &mut Header, name: &str) -> Result<()> {
    let gnu = header
        .as_gnu_mut()
        .ok_or_else(|| eyre!("header is not GNU"))?;
    let bytes = name.as_bytes();
    gnu.name
        .get_mut(..bytes.len())
        .ok_or_else(|| eyre!("fixture name {name:?} does not fit the header"))?
        .copy_from_slice(bytes);
    header.set_cksum();
    Ok(())
}

/// The canonical fixture archive.
pub(super) fn fixture_archive() -> Result<Vec<u8>> {
    let entries: Vec<Entry> = FIXTURE_FILES
        .iter()
        .map(|(name, body)| Entry::File(name, body))
        .collect();
    archive_bytes(&entries)
}

/// Writes `bytes` to `dir/name` and returns the path.
pub(super) fn write_file(dir: &Utf8Path, name: &str, bytes: &[u8]) -> Result<Utf8PathBuf> {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {parent}"))?;
    }
    std::fs::write(&path, bytes).with_context(|| format!("write {path}"))?;
    Ok(path)
}

/// Creates `<root>/17.11.0/{bin,lib,share/extension}` and returns the versioned dir.
pub(super) fn install_tree(root: &Utf8Path) -> Result<Utf8PathBuf> {
    let versioned = root.join(PG_VERSION);
    for sub in ["bin", "lib", "share/extension"] {
        std::fs::create_dir_all(versioned.join(sub)).with_context(|| format!("create {sub}"))?;
    }
    Ok(versioned)
}

/// Builds a manifest artefact describing `bytes` for the current target.
pub(super) fn artifact_for(bytes: &[u8], file: &str, url: &str) -> ManifestArtifact {
    ManifestArtifact {
        postgresql: PG_VERSION.to_owned(),
        target: compile_target().to_owned(),
        file: file.to_owned(),
        url: url.to_owned(),
        sha256: Sha256Hex::of_bytes(bytes),
        size: bytes.len() as u64,
        files: FIXTURE_FILES
            .iter()
            .map(|(name, _)| (*name).to_owned())
            .collect(),
    }
}

/// Renders a schema-1 manifest JSON document for one extension.
pub(super) fn manifest_json(name: &str, artifacts: &[ManifestArtifact]) -> String {
    let artifacts_json: Vec<serde_json::Value> = artifacts
        .iter()
        .map(|artifact| {
            serde_json::json!({
                "postgresql": artifact.postgresql,
                "target": artifact.target,
                "file": artifact.file,
                "url": artifact.url,
                "sha256": artifact.sha256.as_str(),
                "size": artifact.size,
                "files": artifact.files,
            })
        })
        .collect();
    serde_json::json!({
        "schema_version": 1,
        "release": "v1.0.0",
        "generated_at": "2026-09-05T00:00:00+00:00",
        "extensions": [{
            "name": name,
            "package": format!("{name}-pkg"),
            "version": "1.0.0",
            "source": {
                "repository": "https://example.invalid/fixture",
                "tag": "v1.0.0",
                "commit": "0123456789abcdef0123456789abcdef01234567",
            },
            "artifacts": artifacts_json,
        }],
    })
    .to_string()
}

/// Serves `body` to exactly one HTTP request on a loopback port and returns
/// the URL to fetch it from.
pub(super) fn serve_once(body: Vec<u8>) -> Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0").context("bind loopback")?;
    let port = listener.local_addr().context("local addr")?.port();
    thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let mut request = [0_u8; 4096];
        if stream.read(&mut request).is_err() {
            return;
        }
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: \
             application/octet-stream\r\nConnection: close\r\n\r\n",
            body.len()
        );
        if stream.write_all(head.as_bytes()).is_err() || stream.write_all(&body).is_err() {
            return;
        }
        if stream.flush().is_err() {
            // The client already has the bytes; nothing more to do.
        }
    });
    Ok(format!("http://127.0.0.1:{port}/fixture.tar.gz"))
}

/// A URL nothing listens on, for download-failure paths.
pub(super) fn unreachable_url() -> Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0").context("bind loopback")?;
    let port = listener.local_addr().context("local addr")?.port();
    drop(listener);
    Ok(format!("http://127.0.0.1:{port}/missing.tar.gz"))
}
