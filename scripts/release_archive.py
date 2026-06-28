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

import shutil
import shlex
import tarfile
import tomllib
from dataclasses import dataclass
from pathlib import Path
from tempfile import TemporaryDirectory
from typing import Annotated

from cuprum import ExecutionContext, Program, ProjectSettings, ProgramCatalogue
from cuprum import sh as cuprum_sh
from cyclopts import App, Parameter

PACKAGE_NAME = "pg-embed-setup-unpriv"
DEFAULT_BINARIES = ("pg_embedded_setup_unpriv", "pg_worker")
SHELL_QUOTES = frozenset({"'", '"'})
PATH_SEPARATORS = frozenset({"/", "\\"})
CARGO = Program("cargo")
CATALOGUE = ProgramCatalogue(
    projects=[
        ProjectSettings(
            name="cargo",
            programs=(CARGO,),
            documentation_locations=("https://doc.rust-lang.org/cargo/",),
            noise_rules=(),
        )
    ]
)

app = App(help=__doc__)


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


def binary_extension(target: str) -> str:
    """Return the executable suffix used by `target`.

    Windows Rust targets emit `.exe` files; all current Unix targets used by
    this project emit extensionless binaries.
    """
    return ".exe" if "windows" in target else ""


def manifest_version(manifest_path: Path) -> str:
    """Read the package version from `manifest_path`."""
    with manifest_path.open("rb") as manifest:
        data = tomllib.load(manifest)
    return str(data["package"]["version"])


def archive_stem(target: str, version: str) -> str:
    """Return the cargo-binstall archive root directory name."""
    validate_path_component(target, "target")
    return f"{PACKAGE_NAME}-{target}-v{version}"


def release_binary_path(repo: Path, target: str, binary: str) -> Path:
    """Return Cargo's release output path for `binary` and `target`."""
    validate_path_component(target, "target")
    validate_path_component(binary, "binary")
    return repo / "target" / target / "release" / f"{binary}{binary_extension(target)}"


def build_release_binaries(spec: ReleaseBuildSpec) -> None:
    """Build the selected release binaries for `spec.target`."""
    validate_release_spec_components(spec.target, spec.binaries)
    program, program_args = cargo_program_and_args(spec.cargo)
    args = [*program_args, "build", "--release", "--target", spec.target]
    args.extend(cargo_build_job_args(spec.build_jobs))
    for binary in spec.binaries:
        args.extend(["--bin", binary])

    command = cuprum_sh.make(Program(program), catalogue=catalogue_for(program))
    result = command(*args).run_sync(
        capture=False,
        echo=True,
        context=ExecutionContext(cwd=spec.repo),
    )
    if result.exit_code != 0:
        raise SystemExit(result.exit_code)


def cargo_program_and_args(cargo: str) -> tuple[str, list[str]]:
    """Return the executable and wrapper arguments represented by `cargo`."""
    stripped_cargo = cargo.strip()
    if not stripped_cargo:
        raise SystemExit("cargo executable cannot be empty")
    if looks_like_executable_path(stripped_cargo):
        return strip_matching_quotes(stripped_cargo), []
    cargo_command = shlex.split(stripped_cargo)
    if not cargo_command:
        raise SystemExit("cargo executable cannot be empty")
    program, *program_args = cargo_command
    return program, program_args


def validate_release_spec_components(target: str, binaries: tuple[str, ...]) -> None:
    """Reject release identifiers that could escape Cargo's output tree."""
    validate_path_component(target, "target")
    for binary in binaries:
        validate_path_component(binary, "binary")


def validate_path_component(value: str, kind: str) -> None:
    """Reject path-like values accepted only as release identifiers."""
    if message := path_component_violation(value, kind):
        raise SystemExit(message)


def path_component_violation(value: str, kind: str) -> str | None:
    """Return the first validation error for a release path component."""
    return (
        empty_path_component_violation(value, kind)
        or parent_dir_path_component_violation(value, kind)
        or separator_path_component_violation(value, kind)
    )


def empty_path_component_violation(value: str, kind: str) -> str | None:
    """Return the validation error for an empty release path component."""
    if not value:
        return f"{kind} cannot be empty"
    return None


def parent_dir_path_component_violation(value: str, kind: str) -> str | None:
    """Return the validation error for a parent-directory path component."""
    if value in {".", ".."} or ".." in value:
        return f"{kind} cannot contain '..': {value}"
    return None


def separator_path_component_violation(value: str, kind: str) -> str | None:
    """Return the validation error for path separators in a release component."""
    if any(separator in value for separator in PATH_SEPARATORS):
        return f"{kind} cannot contain path separators: {value}"
    return None


def looks_like_executable_path(cargo: str) -> bool:
    """Return whether `cargo` names a path instead of a wrapper argv string."""
    executable = strip_matching_quotes(cargo)
    return "/" in executable or "\\" in executable


def strip_matching_quotes(value: str) -> str:
    """Strip one matching shell-quote pair around an executable path."""
    if has_matching_outer_quotes(value):
        return value[1:-1]
    return value


def has_matching_outer_quotes(value: str) -> bool:
    """Return whether `value` is enclosed in one matching shell quote pair."""
    if len(value) < 2:
        return False
    first = value[0]
    last = value[-1]
    return first == last and first in SHELL_QUOTES


def cargo_build_job_args(build_jobs: str | None) -> list[str]:
    """Return Cargo arguments represented by the Makefile `BUILD_JOBS` value."""
    if not build_jobs:
        return []
    parts = shlex.split(build_jobs)
    if len(parts) == 1 and parts[0].isdecimal():
        return ["--jobs", parts[0]]
    return parts


def catalogue_for(cargo: str) -> ProgramCatalogue:
    """Return a command catalogue that permits the configured Cargo binary."""
    if cargo == str(CARGO):
        return CATALOGUE
    return ProgramCatalogue(
        projects=[
            ProjectSettings(
                name="cargo",
                programs=(Program(cargo),),
                documentation_locations=("https://doc.rust-lang.org/cargo/",),
                noise_rules=(),
            )
        ]
    )


def stage_archive(spec: ReleaseArchiveSpec) -> Path:
    """Stage release binaries and return the produced `.tgz` path."""
    validate_release_spec_components(spec.target, spec.binaries)
    spec.dist_dir.mkdir(parents=True, exist_ok=True)
    stem = archive_stem(spec.target, spec.version)
    archive_path = spec.dist_dir / f"{stem}.tgz"
    archive_path.unlink(missing_ok=True)

    with TemporaryDirectory(prefix=f"{stem}-") as tmp:
        staging_root = Path(tmp) / stem
        staging_root.mkdir()
        copy_release_binaries(spec.repo, spec.target, spec.binaries, staging_root)
        with tarfile.open(archive_path, "w:gz", format=tarfile.PAX_FORMAT) as archive:
            archive.add(staging_root, arcname=stem)

    return archive_path


def copy_release_binaries(
    repo: Path,
    target: str,
    binaries: tuple[str, ...],
    staging_root: Path,
) -> None:
    """Copy Cargo release binaries into the archive staging directory."""
    for binary in binaries:
        source = release_binary_path(repo, target, binary)
        if not source.is_file():
            raise FileNotFoundError(f"release binary missing: {source}")
        shutil.copy2(source, staging_root / source.name)


@app.default
def main(
    target: Annotated[str, Parameter(help="Rust target triple to package.")],
    *,
    release_version: Annotated[
        str | None,
        Parameter(
            name="--release-version",
            help="Release version without the leading v.",
        ),
    ] = None,
    dist_dir: Annotated[
        Path,
        Parameter(help="Directory where the .tgz archive is written."),
    ] = Path("dist"),
    manifest_path: Annotated[
        Path,
        Parameter(help="Path to Cargo.toml."),
    ] = Path("Cargo.toml"),
    cargo: Annotated[str, Parameter(help="Cargo executable to invoke.")] = "cargo",
    build_jobs: Annotated[
        str | None,
        Parameter(help="Optional Cargo job count or build-job flags."),
    ] = None,
    binary: Annotated[
        list[str] | None,
        Parameter(
            name=("--binary", "--bin"),
            help="Binary to include; repeat to override the default production binary set.",
        ),
    ] = None,
) -> None:
    """Build and package the cargo-binstall release archive."""
    repo = manifest_path.resolve().parent
    expected_version = manifest_version(manifest_path)
    selected_version = release_version or expected_version
    if selected_version != expected_version:
        message = (
            f"VERSION ({selected_version}) must match Cargo.toml package version "
            f"({expected_version})"
        )
        raise SystemExit(message)

    binaries = tuple(binary or DEFAULT_BINARIES)
    build_release_binaries(
        ReleaseBuildSpec(
            repo=repo,
            target=target,
            binaries=binaries,
            cargo=cargo,
            build_jobs=build_jobs,
        )
    )
    archive_path = stage_archive(
        ReleaseArchiveSpec(
            repo=repo,
            target=target,
            version=selected_version,
            dist_dir=repo / dist_dir,
            binaries=binaries,
        )
    )
    print(archive_path)


if __name__ == "__main__":
    app()
