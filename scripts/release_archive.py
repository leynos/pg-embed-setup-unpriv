#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.13"
# dependencies = [
#   "cuprum==0.1.0",
#   "cyclopts==4.19.0",
# ]
# ///
"""Build and package release binaries for cargo-binstall."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Annotated

from cyclopts import App, Parameter

from release_archive_cargo import (
    _cargo_program_and_args,
    _looks_like_executable_path,
    _path_wrapper_program_and_args,
    _strip_matching_quotes,
    _windows_wrapper_program_and_args,
    build_release_binaries,
)
from release_archive_staging import (
    ManifestVersionError,
    archive_stem,
    binary_extension,
    copy_release_binaries,
    manifest_version,
    release_binary_path,
    stage_archive,
    validate_release_spec_components,
)

DEFAULT_BINARIES = ("pg_embedded_setup_unpriv", "pg_worker")

app = App(help=__doc__)

_TargetArg = Annotated[str, Parameter(help="Rust target triple to package.")]
_ReleaseVersionArg = Annotated[
    str | None,
    Parameter(
        name="--release-version",
        help="Release version without the leading v.",
    ),
]
_DistDirArg = Annotated[
    Path,
    Parameter(help="Directory where the .tgz archive is written."),
]
_ManifestPathArg = Annotated[Path, Parameter(help="Path to Cargo.toml.")]
_CargoArg = Annotated[str, Parameter(help="Cargo executable to invoke.")]
_BuildJobsArg = Annotated[
    str | None,
    Parameter(help="Optional Cargo job count or build-job flags."),
]
_BinaryArg = Annotated[
    list[str] | None,
    Parameter(
        name=("--binary", "--bin"),
        help="Binary to include; repeat to override the default production binary set.",
    ),
]


@dataclass(frozen=True)
class ReleaseBuildSpec:
    """Build inputs for the release binaries."""

    repo: Path
    target: str
    binaries: tuple[str, ...]
    cargo: str
    build_jobs: str | None = None


@dataclass(frozen=True)
class ReleaseArchiveSpec:
    """Archive staging inputs for cargo-binstall release assets."""

    repo: Path
    target: str
    version: str
    dist_dir: Path
    binaries: tuple[str, ...]


@dataclass(frozen=True)
class _ReleaseCliSpec:
    """Inputs supplied through the release archive CLI."""

    target: str
    release_version: str | None
    dist_dir: Path
    manifest_path: Path
    cargo: str
    build_jobs: str | None
    binary: list[str] | None


def _selected_release_version(manifest_path: Path, release_version: str | None) -> str:
    """Return the requested release version after matching the manifest."""
    try:
        expected_version = manifest_version(manifest_path)
    except ManifestVersionError as err:
        raise SystemExit(str(err)) from err
    selected_version = release_version or expected_version
    if selected_version != expected_version:
        message = (
            f"VERSION ({selected_version}) must match Cargo.toml package version "
            f"({expected_version})"
        )
        raise SystemExit(message)
    return selected_version


def _run_release_archive(spec: _ReleaseCliSpec) -> None:
    """Build, stage, and print the release archive path."""
    repo = spec.manifest_path.resolve().parent
    selected_version = _selected_release_version(spec.manifest_path, spec.release_version)
    binaries = tuple(spec.binary or DEFAULT_BINARIES)
    build_release_binaries(
        ReleaseBuildSpec(repo, spec.target, binaries, spec.cargo, spec.build_jobs)
    )
    archive_path = stage_archive(
        ReleaseArchiveSpec(repo, spec.target, selected_version, repo / spec.dist_dir, binaries)
    )
    print(archive_path)


@app.default
def main(
    target: _TargetArg,
    *,
    release_version: _ReleaseVersionArg = None,
    dist_dir: _DistDirArg = Path("dist"),
    manifest_path: _ManifestPathArg = Path("Cargo.toml"),
    cargo: _CargoArg = "cargo",
    build_jobs: _BuildJobsArg = None,
    binary: _BinaryArg = None,
) -> None:
    """Build and package the cargo-binstall release archive.

    Parameters
    ----------
    target : str
        Rust target triple to package.
    release_version : str | None, optional
        Release version without the leading `v`.
    dist_dir : Path, optional
        Directory where the `.tgz` archive is written.
    manifest_path : Path, optional
        Path to `Cargo.toml`.
    cargo : str, optional
        Cargo executable or wrapper command.
    build_jobs : str | None, optional
        Optional Cargo job count or build-job flags.
    binary : list[str] | None, optional
        Binary target names to include.

    Raises
    ------
    SystemExit
        Raised when version discovery, validation, or Cargo execution fails.

    Examples
    --------
    >>> main("x86_64-unknown-linux-gnu", release_version="0.5.1")  # doctest: +SKIP
    """
    _run_release_archive(
        _ReleaseCliSpec(
            target=target,
            release_version=release_version,
            dist_dir=dist_dir,
            manifest_path=manifest_path,
            cargo=cargo,
            build_jobs=build_jobs,
            binary=binary,
        )
    )


if __name__ == "__main__":
    app()
