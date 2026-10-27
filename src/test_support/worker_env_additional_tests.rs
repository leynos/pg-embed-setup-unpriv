//! Regression tests for concurrent worker staging and interrupted copies.

use super::*;

#[test]
fn pointer_file_writer_does_not_share_a_temporary_path() -> color_eyre::Result<()> {
    let dir = tempfile::tempdir()?;
    // The legacy fixed name may be left behind or occupied by another writer.
    fs::create_dir(dir.path().join("pg_worker_staged.path.tmp"))?;
    let worker = dir.path().join("worker");
    write_pointer_file(dir.path(), &worker)?;
    ensure!(
        fs::read(dir.path().join("pg_worker_staged.path"))?
            == worker.as_os_str().as_encoded_bytes(),
        "pointer must contain the entire worker path"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn newer_incomplete_staged_worker_requires_replacement() -> color_eyre::Result<()> {
    let dir = tempfile::tempdir()?;
    let source = dir.path().join("source");
    let staged = dir.path().join("staged");
    fs::write(&source, b"complete worker binary")?;
    fs::write(&staged, b"partial")?;
    fs::set_permissions(&staged, fs::Permissions::from_mode(0o755))?;
    let newer = fs::metadata(&source)?.modified()? + std::time::Duration::from_secs(1);
    filetime::set_file_mtime(&staged, filetime::FileTime::from_system_time(newer))?;
    ensure!(
        should_restage(&source, &staged)?,
        "a truncated staged worker must be replaced"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn newer_staged_worker_without_public_execute_permission_requires_replacement()
-> color_eyre::Result<()> {
    let dir = tempfile::tempdir()?;
    let source = dir.path().join("source");
    let staged = dir.path().join("staged");
    fs::write(&source, b"worker")?;
    fs::write(&staged, b"worker")?;
    fs::set_permissions(&staged, fs::Permissions::from_mode(0o700))?;
    let newer = fs::metadata(&source)?.modified()? + std::time::Duration::from_secs(1);
    filetime::set_file_mtime(&staged, filetime::FileTime::from_system_time(newer))?;
    ensure!(
        should_restage(&source, &staged)?,
        "an inaccessible staged worker must be replaced"
    );
    Ok(())
}
