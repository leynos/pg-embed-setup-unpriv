//! The installation tree as a capability, not a path.
//!
//! Pass two writes into a directory the demoted worker owns, while the
//! bootstrap that performs the write may still be root: `run_post_setup` is
//! called directly in the parent process, so `Setup` and `Start` reach the
//! worker but the extension install does not. A root process writing by path
//! into a tree a less-privileged user owns is the wrong way round, because
//! that user can replace a directory between the moment it is checked and the
//! moment it is used, and a path-based `create_dir_all`, temporary-file
//! creation or rename then resolves the replacement instead.
//!
//! Every operation here therefore goes through one [`cap_std::fs::Dir`] handle
//! opened at the installation root and held until the last rename completes.
//! A `Dir` only accesses paths relative to itself, so a component swapped for a
//! symlink cannot redirect a write out of the tree however the swap is timed.
//! The check that a parent is a real directory remains, but it is no longer
//! what holds the boundary: it enforces the stricter in-tree policy that the
//! installation tree routes through no symlink at all, and its failure mode is
//! now a refusal inside a sandbox rather than an escape from one.

use camino::{Utf8Path, Utf8PathBuf};
use cap_std::{
    ambient_authority,
    fs::{Dir, File, OpenOptions},
};
use color_eyre::eyre::{Report, eyre};

use super::Sha256Hex;
use crate::{
    error::{BootstrapErrorKind, BootstrapResult},
    extensions::extension_error,
};

/// What the destination holds, as far as the planned bytes are concerned.
///
/// A query's answer, not an error: only [`Destination::Identical`] changes
/// what the caller does, and the rest are reported apart so the reason a file
/// is rewritten is available to the log rather than inferred from its absence.
pub(super) enum Destination {
    /// A regular file already holding exactly the planned bytes.
    Identical(File),
    /// Nothing is at the destination.
    Absent,
    /// Something is there that is not a regular file. A directory reaches
    /// this on Unix, where it opens and then fails the stat; on Windows it
    /// cannot be opened for reading without backup semantics, which this
    /// does not ask for, so it arrives as [`Destination::Unreadable`]
    /// instead. Either way it is refused and replaced.
    NotRegular,
    /// A regular file holding different bytes.
    Different,
    /// The destination could not be opened, stated or read. A symlink refused
    /// by `O_NOFOLLOW` arrives here.
    Unreadable(std::io::Error),
}

impl Destination {
    /// Returns why the destination cannot be reused, for the log.
    ///
    /// [`Destination::Identical`] has no reason, because nothing is rewritten.
    pub(super) fn rewrite_reason(&self) -> Option<String> {
        match self {
            Self::Identical(_) => None,
            Self::Absent => Some("nothing is there".to_owned()),
            Self::NotRegular => Some("what is there is not a regular file".to_owned()),
            Self::Different => Some("what is there holds different bytes".to_owned()),
            Self::Unreadable(err) => Some(format!("what is there cannot be read: {err}")),
        }
    }
}

/// Requires an opened destination to be a regular file.
///
/// A directory or a device where a file belongs is not a differing file, and
/// the distinction matters: the rename that follows replaces it, and an
/// operator should be told that is what happened.
fn require_regular(file: File) -> Result<File, Destination> {
    match file.metadata() {
        Ok(metadata) if metadata.is_file() => Ok(file),
        Ok(_) => Err(Destination::NotRegular),
        Err(err) => Err(Destination::Unreadable(err)),
    }
}

/// Compares an opened regular file against the planned bytes.
///
/// The handle is carried into [`Destination::Identical`] so the mode and
/// ownership repair acts on the file that was hashed rather than on the path.
fn compare_digest(file: File, bytes: &[u8]) -> Destination {
    match Sha256Hex::of_reader(&file) {
        Ok(digest) if digest == Sha256Hex::of_bytes(bytes) => Destination::Identical(file),
        Ok(_) => Destination::Different,
        Err(err) => Destination::Unreadable(err),
    }
}

/// A handle on the installation tree, plus its path for operator messages.
pub(super) struct InstallTree {
    dir: Dir,
    root: Utf8PathBuf,
}

impl InstallTree {
    /// Opens the installation root, refusing a symlink at the root itself.
    ///
    /// `Dir::open_ambient_dir` is explicitly not sandboxed: it follows
    /// symlinks to reach its target, and the root is a parent of every
    /// destination, so a link there would route the whole tree elsewhere. The
    /// root is the one component no handle can guard, because opening it is
    /// what creates the handle, so it is checked immediately before the open.
    /// Every component below is resolved through the returned handle instead.
    pub(super) fn open(root: &Utf8Path) -> BootstrapResult<Self> {
        let failed = |report| extension_error(BootstrapErrorKind::ExtensionInstallFailed, report);
        match std::fs::symlink_metadata(root) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(metadata) => {
                return Err(failed(eyre!(
                    "{root} is a {:?}, not a directory; the installation tree must not route \
                     through a symlink",
                    metadata.file_type()
                )));
            }
            Err(err) => return Err(failed(eyre!("cannot inspect {root}: {err}"))),
        }
        let dir = Dir::open_ambient_dir(root.as_std_path(), ambient_authority())
            .map_err(|err| failed(eyre!("cannot open installation tree {root}: {err}")))?;
        Ok(Self {
            dir,
            root: root.to_path_buf(),
        })
    }

    /// Names a tree-relative path in full, for errors the operator reads.
    pub(super) fn path_of(&self, relative: &Utf8Path) -> Utf8PathBuf { self.root.join(relative) }

    /// Refuses a destination whose parent components are not real directories.
    ///
    /// `classify_entry_path` validates the name the archive carries, not the
    /// tree the file lands in. A `lib` or `share/extension` that is a symlink
    /// is refused here rather than followed. Each component is inspected with
    /// the handle's own `symlink_metadata`, which does not follow, and a
    /// component that does not exist yet is one the creation step will make,
    /// so nothing below it can exist and the walk stops.
    ///
    /// The destination itself is not covered: a symlink there is replaced by
    /// the rename, which is how an installed file is meant to be rewritten.
    /// The branch that must not follow it is the identical-bytes one, which
    /// [`InstallTree::inspect_destination`] holds: a symlink there arrives as
    /// [`Destination::Unreadable`], because the open carries `O_NOFOLLOW`.
    pub(super) fn require_real_parents(&self, relative: &Utf8Path) -> Result<(), Report> {
        let components: Vec<&str> = relative
            .components()
            .map(|component| component.as_str())
            .collect();
        let parents = components.len().saturating_sub(1);
        let mut current = Utf8PathBuf::new();
        for component in components.into_iter().take(parents) {
            current.push(component);
            match self.dir.symlink_metadata(current.as_std_path()) {
                Ok(metadata) if metadata.is_dir() => {}
                Ok(metadata) => {
                    return Err(eyre!(
                        "{} is a {:?}, not a directory; the installation tree must not route \
                         through a symlink",
                        self.path_of(&current),
                        metadata.file_type()
                    ));
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => break,
                Err(err) => {
                    return Err(eyre!("cannot inspect {}: {err}", self.path_of(&current)));
                }
            }
        }
        Ok(())
    }

    /// Creates the destination's parent directories through the handle.
    pub(super) fn create_parents(&self, relative: &Utf8Path) -> Result<(), Report> {
        let Some(parent) = relative
            .parent()
            .filter(|parent| !parent.as_str().is_empty())
        else {
            return Ok(());
        };
        self.dir
            .create_dir_all(parent.as_std_path())
            .map_err(|err| eyre!("cannot create {}: {err}", self.path_of(parent)))
    }

    /// Inspects `relative` against `bytes`, naming what is there.
    ///
    /// The handle is returned so the mode and ownership repair acts on the
    /// file that was hashed rather than on the path. A symlink at the
    /// destination is otherwise followed alike by the digest, the `chmod` and
    /// the `chown`, and a bootstrap running as root would set the mode and the
    /// tree's ownership on whatever the link named without writing a byte
    /// through it. The open carries `O_NOFOLLOW` on Unix, so the symlink is
    /// refused by the open itself; every platform then requires the opened
    /// handle to report a regular file, which also refuses a directory.
    ///
    /// Three fallible steps, and each names its own outcome rather than
    /// collapsing into "not identical". Every outcome but
    /// [`Destination::Identical`] leads the caller to the same place, a fresh
    /// file and a rename that replaces whatever was there; but they are not
    /// the same event. An absent or differing destination is the ordinary
    /// course of an install, while a destination that cannot be read is a
    /// fault the operator should see named in the log, and a non-regular one
    /// is something planted where a file belongs. Returning `Option` reported
    /// all three as the first.
    pub(super) fn inspect_destination(&self, relative: &Utf8Path, bytes: &[u8]) -> Destination {
        match self.open_destination(relative).and_then(require_regular) {
            Ok(file) => compare_digest(file, bytes),
            Err(found) => found,
        }
    }

    /// Opens the destination without following a symlink at it.
    ///
    /// The error side carries the outcome rather than an error type, so the
    /// three steps of the inspection compose without any of them having to
    /// know what the others report.
    fn open_destination(&self, relative: &Utf8Path) -> Result<File, Destination> {
        match self
            .dir
            .open_with(relative.as_std_path(), &read_no_follow())
        {
            Ok(file) => Ok(file),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Err(Destination::Absent),
            // A symlink refused by `O_NOFOLLOW` arrives here rather than as a
            // successful open, under whichever errno the platform uses for
            // it, so it is reported as unreadable rather than ignored.
            Err(err) => Err(Destination::Unreadable(err)),
        }
    }

    /// Creates a fresh temporary file beside the destination.
    ///
    /// The name is created exclusively, so a file an attacker planted at the
    /// guessed name is a refusal rather than a handle onto their file; the
    /// counter then supplies a different name for the next attempt.
    pub(super) fn create_temp_beside(
        &self,
        relative: &Utf8Path,
    ) -> Result<(File, Utf8PathBuf), Report> {
        let parent = relative
            .parent()
            .filter(|parent| !parent.as_str().is_empty());
        let mut last: Option<std::io::Error> = None;
        for _ in 0..TEMP_NAME_ATTEMPTS {
            let name = temp_name();
            let candidate = parent.map_or_else(|| Utf8PathBuf::from(&name), |dir| dir.join(&name));
            match self
                .dir
                .open_with(candidate.as_std_path(), &create_new_write())
            {
                Ok(file) => return Ok((file, candidate)),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => last = Some(err),
                Err(err) => {
                    return Err(eyre!(
                        "cannot create temporary file in {}: {err}",
                        self.path_of(parent.unwrap_or_else(|| Utf8Path::new("")))
                    ));
                }
            }
        }
        Err(eyre!(
            "cannot create a temporary file in {} after {TEMP_NAME_ATTEMPTS} attempts: {}",
            self.path_of(parent.unwrap_or_else(|| Utf8Path::new(""))),
            last.map_or_else(|| "no error recorded".to_owned(), |err| err.to_string())
        ))
    }

    /// Moves a written temporary file onto its destination through the handle.
    pub(super) fn place(&self, temp: &Utf8Path, relative: &Utf8Path) -> Result<(), Report> {
        self.dir
            .rename(temp.as_std_path(), &self.dir, relative.as_std_path())
            .map_err(|err| {
                eyre!(
                    "cannot move file into place at {}: {err}",
                    self.path_of(relative)
                )
            })
    }

    /// Discards a temporary file whose write or placement failed.
    pub(super) fn discard_temp(&self, temp: &Utf8Path) {
        drop(self.dir.remove_file(temp.as_std_path()));
    }
}

/// Attempts allowed before a temporary name is treated as unobtainable.
const TEMP_NAME_ATTEMPTS: u8 = 16;

/// Builds a temporary name unique within the process and unlikely across them.
fn temp_name() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos: u32 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.subsec_nanos());
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!(".pg-embed-{}-{nanos}-{count}.tmp", std::process::id())
}

/// Read-only open options that refuse a symlink in the final component.
fn read_no_follow() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.read(true);
    no_follow(&mut options);
    options
}

/// Write options that refuse to open anything that already exists.
fn create_new_write() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    no_follow(&mut options);
    options
}

/// Adds `O_NOFOLLOW` where the platform has it.
#[cfg(unix)]
fn no_follow(options: &mut OpenOptions) {
    use cap_std::fs::OpenOptionsExt as _;
    options.custom_flags(libc::O_NOFOLLOW);
}

/// Windows has no `O_NOFOLLOW`; the handle's own type is what the caller
/// checks, and creating a symlink there needs privilege or developer mode.
#[cfg(not(unix))]
fn no_follow(_options: &mut OpenOptions) {}
