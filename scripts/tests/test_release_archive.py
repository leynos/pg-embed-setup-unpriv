"""Tests for the release archive packager."""

from __future__ import annotations

import importlib.util
import sys
import tarfile
from pathlib import Path

from cmd_mox import CmdMox

SCRIPT_PATH = Path(__file__).resolve().parents[1] / "release_archive.py"
SPEC = importlib.util.spec_from_file_location("release_archive", SCRIPT_PATH)
assert SPEC is not None
release_archive = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = release_archive
SPEC.loader.exec_module(release_archive)


def write_manifest(repo: Path, version: str = "0.5.1") -> Path:
    """Write the minimal manifest needed by the packager."""
    manifest = repo / "Cargo.toml"
    manifest.write_text(f'[package]\nname = "pg-embed-setup-unpriv"\nversion = "{version}"\n')
    return manifest


def write_release_binary(
    repo: Path,
    target: str,
    name: str,
    content: str = "binary",
) -> Path:
    """Write a fake Cargo release binary for archive staging tests."""
    filename = f"{name}{release_archive.binary_extension(target)}"
    output = repo / "target" / target / "release" / filename
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(content)
    return output


def archive_members(archive: Path) -> list[str]:
    """Return archive member names in stored order."""
    with tarfile.open(archive, "r:gz") as tar:
        return tar.getnames()


def assert_build_release_binaries_invokes_cargo(
    repo: Path,
    *,
    binaries: tuple[str, ...],
    expected_args: tuple[str, ...],
    build_jobs: str | None,
    cargo: str = "cargo",
    program: str = "cargo",
) -> None:
    """Assert the release binary build delegates to Cargo as expected."""
    with CmdMox() as mox:
        mox.mock(program).with_args(*expected_args).returns()
        mox.replay()

        release_archive.build_release_binaries(
            release_archive.ReleaseBuildSpec(
                repo=repo,
                target="x86_64-unknown-linux-gnu",
                binaries=binaries,
                cargo=cargo,
                build_jobs=build_jobs,
            )
        )


def test_windows_targets_use_exe_suffix() -> None:
    assert release_archive.binary_extension("x86_64-pc-windows-msvc") == ".exe"
    assert release_archive.binary_extension("aarch64-apple-darwin") == ""


def test_stage_archive_uses_cargo_binstall_layout_for_windows(tmp_path: Path) -> None:
    target = "x86_64-pc-windows-msvc"
    binaries = ("pg_embedded_setup_unpriv", "pg_worker")
    for binary in binaries:
        write_release_binary(tmp_path, target, binary)

    archive = release_archive.stage_archive(
        release_archive.ReleaseArchiveSpec(
            repo=tmp_path,
            target=target,
            version="0.5.1",
            dist_dir=tmp_path / "dist",
            binaries=binaries,
        )
    )

    root = "pg-embed-setup-unpriv-x86_64-pc-windows-msvc-v0.5.1"
    assert archive.name == f"{root}.tgz"
    assert archive_members(archive) == [
        root,
        f"{root}/pg_embedded_setup_unpriv.exe",
        f"{root}/pg_worker.exe",
    ]


def test_build_release_binaries_invokes_cargo_with_all_bins(tmp_path: Path) -> None:
    binaries = ("pg_embedded_setup_unpriv", "pg_worker")
    expected_args = (
        "build",
        "--release",
        "--target",
        "x86_64-unknown-linux-gnu",
        "--bin",
        binaries[0],
        "--bin",
        binaries[1],
    )
    build_jobs = None

    assert_build_release_binaries_invokes_cargo(
        tmp_path,
        binaries=binaries,
        expected_args=expected_args,
        build_jobs=build_jobs,
    )


def test_build_release_binaries_preserves_build_jobs_flags(tmp_path: Path) -> None:
    binaries = ("pg_embedded_setup_unpriv",)
    expected_args = (
        "build",
        "--release",
        "--target",
        "x86_64-unknown-linux-gnu",
        "--jobs",
        "2",
        "--bin",
        binaries[0],
    )
    build_jobs = "--jobs 2"

    assert_build_release_binaries_invokes_cargo(
        tmp_path,
        binaries=binaries,
        expected_args=expected_args,
        build_jobs=build_jobs,
    )


def test_build_release_binaries_preserves_cargo_wrapper_args(tmp_path: Path) -> None:
    binaries = ("pg_embedded_setup_unpriv",)
    expected_args = (
        "cargo",
        "build",
        "--release",
        "--target",
        "x86_64-unknown-linux-gnu",
        "--bin",
        binaries[0],
    )
    build_jobs = None

    assert_build_release_binaries_invokes_cargo(
        tmp_path,
        binaries=binaries,
        expected_args=expected_args,
        build_jobs=build_jobs,
        cargo="sccache cargo",
        program="sccache",
    )


def test_main_rejects_version_mismatch_before_build(tmp_path: Path) -> None:
    manifest = write_manifest(tmp_path, version="0.5.1")

    try:
        release_archive.main(
            "x86_64-unknown-linux-gnu",
            release_version="0.5.2",
            manifest_path=manifest,
        )
    except SystemExit as err:
        assert "must match Cargo.toml package version" in str(err)
    else:
        raise AssertionError("expected version mismatch to abort")
