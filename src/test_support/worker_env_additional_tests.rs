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
#[rstest]
#[case::truncated(b"partial", 0o755)]
#[case::inaccessible(b"complete worker binary", 0o700)]
fn newer_invalid_staged_worker_requires_replacement(
    #[case] contents: &[u8],
    #[case] mode: u32,
) -> color_eyre::Result<()> {
    let dir = tempfile::tempdir()?;
    let source = dir.path().join("source");
    let staged = dir.path().join("staged");
    fs::write(&source, b"complete worker binary")?;
    fs::write(&staged, contents)?;
    fs::set_permissions(&staged, fs::Permissions::from_mode(mode))?;
    let newer = fs::metadata(&source)?.modified()? + std::time::Duration::from_secs(1);
    filetime::set_file_mtime(&staged, filetime::FileTime::from_system_time(newer))?;
    ensure!(
        should_restage(&source, &staged)?,
        "an invalid staged worker must be replaced"
    );
    Ok(())
}
