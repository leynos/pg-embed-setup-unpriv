"""Tests for the release archive packager."""

from __future__ import annotations

import importlib.util
import re
import sys
import tarfile
from pathlib import Path

import pytest
from cmd_mox import CmdMox

SCRIPT_PATH = Path(__file__).resolve().parents[1] / "release_archive.py"
sys.path.insert(0, str(SCRIPT_PATH.parent))
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
    spec: release_archive.ReleaseBuildSpec,
    *,
    expected_args: tuple[str, ...],
) -> None:
    """Assert the release binary build delegates to Cargo as expected."""
    program, program_args = release_archive._cargo_program_and_args(spec.cargo)
    with CmdMox() as mox:
        mox.mock(program).with_args(*program_args, *expected_args).returns()
        mox.replay()

        release_archive.build_release_binaries(spec)


def test_windows_targets_use_exe_suffix() -> None:
    """Windows targets use `.exe` while Unix-like release targets do not."""
    assert release_archive.binary_extension("x86_64-pc-windows-msvc") == ".exe"
    assert release_archive.binary_extension("aarch64-apple-darwin") == ""


@pytest.mark.parametrize(
    ("manifest_content", "expected_reason", "match_kind"),
    [
        pytest.param(None, "No such file", "contains", id="missing-manifest"),
        pytest.param("[package\n", "invalid TOML:", "startswith", id="invalid-toml"),
        pytest.param('package = 1\n', "package must be a table", "exact", id="package-table"),
        pytest.param(
            '[package]\nname = "pg-embed-setup-unpriv"\n',
            "missing key: 'version'",
            "exact",
            id="missing-version",
        ),
        pytest.param(
            '[package]\nname = "pg-embed-setup-unpriv"\nversion = 1\n',
            "package.version must be a string",
            "exact",
            id="string-version",
        ),
    ],
)
def test_manifest_version_reports_manifest_errors(
    tmp_path: Path,
    manifest_content: str | None,
    expected_reason: str,
    match_kind: str,
) -> None:
    """Manifest version discovery reports missing and malformed manifests."""
    manifest = tmp_path / "Cargo.toml"
    if manifest_content is not None:
        manifest.write_text(manifest_content)

    with pytest.raises(release_archive.ManifestVersionError) as exc_info:
        release_archive.manifest_version(manifest)

    assert exc_info.value.manifest_path == manifest
    if match_kind == "contains":
        assert expected_reason in exc_info.value.reason
    elif match_kind == "startswith":
        assert exc_info.value.reason.startswith(expected_reason)
    else:
        assert exc_info.value.reason == expected_reason


def test_stage_archive_uses_cargo_binstall_layout_for_windows(tmp_path: Path) -> None:
    """Windows archives use cargo-binstall layout with `.exe` binaries."""
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


def test_stage_archive_rejects_path_like_target(tmp_path: Path) -> None:
    """Archive staging rejects targets that would escape the archive root."""
    spec = release_archive.ReleaseArchiveSpec(
        repo=tmp_path,
        target="../x86_64-unknown-linux-gnu",
        version="0.5.1",
        dist_dir=tmp_path / "dist",
        binaries=("pg_embedded_setup_unpriv",),
    )

    with pytest.raises(SystemExit, match=re.escape("target cannot contain '..'")):
        release_archive.stage_archive(spec)


def test_stage_archive_rejects_path_like_binary(tmp_path: Path) -> None:
    """Archive staging rejects binary names that would escape the root."""
    spec = release_archive.ReleaseArchiveSpec(
        repo=tmp_path,
        target="x86_64-unknown-linux-gnu",
        version="0.5.1",
        dist_dir=tmp_path / "dist",
        binaries=("../pg_worker",),
    )

    with pytest.raises(SystemExit, match=re.escape("binary cannot contain '..'")):
        release_archive.stage_archive(spec)


@pytest.mark.parametrize(
    ("target", "binaries", "expected_message"),
    [
        pytest.param("", ("pg_embedded_setup_unpriv",), "target cannot be empty", id="empty-target"),
        pytest.param("..", ("pg_embedded_setup_unpriv",), "target cannot contain '..'", id="parent-target"),
        pytest.param(
            "x86_64/linux",
            ("pg_embedded_setup_unpriv",),
            "target cannot contain path separators",
            id="separator-target",
        ),
        pytest.param("x86_64-unknown-linux-gnu", ("",), "binary cannot be empty", id="empty-binary"),
        pytest.param("x86_64-unknown-linux-gnu", ("..",), "binary cannot contain '..'", id="parent-binary"),
        pytest.param(
            "x86_64-unknown-linux-gnu",
            ("bin/pg",),
            "binary cannot contain path separators",
            id="separator-binary",
        ),
    ],
)
def test_validate_release_spec_components_rejects_path_like_values(
    target: str,
    binaries: tuple[str, ...],
    expected_message: str,
) -> None:
    """Release spec validation rejects empty or path-like components."""
    with pytest.raises(SystemExit, match=re.escape(expected_message)):
        release_archive.validate_release_spec_components(target, binaries)


def test_build_release_binaries_invokes_cargo_with_all_bins(tmp_path: Path) -> None:
    """Release builds invoke Cargo once with every configured binary."""
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
    spec = release_archive.ReleaseBuildSpec(
        repo=tmp_path,
        target="x86_64-unknown-linux-gnu",
        binaries=binaries,
        cargo="cargo",
        build_jobs=build_jobs,
    )

    assert_build_release_binaries_invokes_cargo(
        spec,
        expected_args=expected_args,
    )


def test_build_release_binaries_preserves_build_jobs_flags(tmp_path: Path) -> None:
    """Release builds preserve explicit Cargo job flags."""
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
    spec = release_archive.ReleaseBuildSpec(
        repo=tmp_path,
        target="x86_64-unknown-linux-gnu",
        binaries=binaries,
        cargo="cargo",
        build_jobs=build_jobs,
    )

    assert_build_release_binaries_invokes_cargo(
        spec,
        expected_args=expected_args,
    )


def test_build_release_binaries_preserves_cargo_wrapper_args(tmp_path: Path) -> None:
    """Release builds preserve wrapper commands before Cargo."""
    binaries = ("pg_embedded_setup_unpriv",)
    expected_args = (
        "build",
        "--release",
        "--target",
        "x86_64-unknown-linux-gnu",
        "--bin",
        binaries[0],
    )
    build_jobs = None
    spec = release_archive.ReleaseBuildSpec(
        repo=tmp_path,
        target="x86_64-unknown-linux-gnu",
        binaries=binaries,
        cargo="sccache cargo",
        build_jobs=build_jobs,
    )

    assert_build_release_binaries_invokes_cargo(
        spec,
        expected_args=expected_args,
    )


def test_main_discovers_manifest_version_when_release_version_is_none(
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
) -> None:
    """The CLI uses Cargo.toml when no release version override is supplied."""
    target = "x86_64-unknown-linux-gnu"
    manifest = write_manifest(tmp_path, version="0.5.2")
    for binary in release_archive.DEFAULT_BINARIES:
        write_release_binary(tmp_path, target, binary)

    with CmdMox() as mox:
        mox.mock("cargo").with_args(
            "build",
            "--release",
            "--target",
            target,
            "--bin",
            "pg_embedded_setup_unpriv",
            "--bin",
            "pg_worker",
        ).returns()
        mox.replay()

        release_archive.main(
            target,
            release_version=None,
            manifest_path=manifest,
            dist_dir=Path("dist"),
            cargo="cargo",
        )

    archive = Path(capsys.readouterr().out.strip())
    root = "pg-embed-setup-unpriv-x86_64-unknown-linux-gnu-v0.5.2"
    assert archive == tmp_path / "dist" / f"{root}.tgz"
    assert archive_members(archive) == [
        root,
        f"{root}/pg_embedded_setup_unpriv",
        f"{root}/pg_worker",
    ]


def test_cargo_program_and_args_preserves_absolute_wrapper_args() -> None:
    """Cargo command parsing keeps absolute wrapper paths and arguments."""
    cargo = "/usr/bin/sccache cargo"

    program, program_args = release_archive._cargo_program_and_args(cargo)

    assert program == "/usr/bin/sccache"
    assert program_args == ["cargo"]


def test_cargo_program_and_args_preserves_windows_wrapper_args() -> None:
    """Cargo command parsing keeps Windows `.exe` wrapper paths."""
    cargo = r"C:\Tools\sccache.exe cargo"

    program, program_args = release_archive._cargo_program_and_args(cargo)

    assert program == r"C:\Tools\sccache.exe"
    assert program_args == ["cargo"]


def test_build_release_binaries_treats_cargo_path_with_spaces_as_executable(
) -> None:
    """Cargo paths with spaces are treated as the executable."""
    cargo = r"C:\Program Files\Rust\cargo.exe"

    program, program_args = release_archive._cargo_program_and_args(cargo)

    assert program == cargo
    assert program_args == []


def test_cargo_program_and_args_rejects_malformed_quoting() -> None:
    """Malformed Cargo command quoting exits with a CLI error."""
    with pytest.raises(
        SystemExit,
        match=re.escape("invalid cargo executable command: No closing quotation"),
    ):
        release_archive._cargo_program_and_args("'unterminated cargo")


def test_main_rejects_version_mismatch_before_build(tmp_path: Path) -> None:
    """The CLI rejects release version mismatches before building."""
    manifest = write_manifest(tmp_path, version="0.5.1")

    expected_message = "VERSION (0.5.2) must match Cargo.toml package version (0.5.1)"
    with pytest.raises(SystemExit, match=re.escape(expected_message)):
        release_archive.main(
            "x86_64-unknown-linux-gnu",
            release_version="0.5.2",
            manifest_path=manifest,
        )
