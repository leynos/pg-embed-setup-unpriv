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
from collections.abc import Mapping
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


@dataclass(frozen=True)
class ManifestVersionError(Exception):
    """Typed failure raised when Cargo manifest version discovery fails."""

    manifest_path: Path
    reason: str

    def __str__(self) -> str:
        return f"failed to read package version from {self.manifest_path}: {self.reason}"


def binary_extension(target: str) -> str:
    """Return the executable suffix used by `target`.

    Windows Rust targets emit `.exe` files; all current Unix targets used by
    this project emit extensionless binaries.

    Parameters
    ----------
    target : str
        Rust target triple to inspect.

    Returns
    -------
    str
        `.exe` for Windows targets, otherwise an empty string.

    Examples
    --------
    >>> binary_extension("x86_64-pc-windows-msvc")
    '.exe'
    """
    return ".exe" if "windows" in target else ""


def manifest_version(manifest_path: Path) -> str:
    """Read the package version from a Cargo manifest.

    Parameters
    ----------
    manifest_path : Path
        Path to the `Cargo.toml` manifest to inspect.

    Returns
    -------
    str
        The string value from `package.version`.

    Raises
    ------
    ManifestVersionError
        Raised when the manifest cannot be read, cannot be parsed as TOML, is
        missing `package.version`, or stores `package.version` as a non-string
        value.

    Examples
    --------
    >>> manifest_version(Path("Cargo.toml"))  # doctest: +SKIP
    '0.5.1'
    """
    data = _load_manifest_data(manifest_path)
    return _package_version_from_manifest_data(manifest_path, data)


def _load_manifest_data(manifest_path: Path) -> object:
    """Load TOML data from `manifest_path`."""
    try:
        with manifest_path.open("rb") as manifest:
            return tomllib.load(manifest)
    except OSError as err:
        raise ManifestVersionError(manifest_path, str(err)) from err
    except tomllib.TOMLDecodeError as err:
        raise ManifestVersionError(manifest_path, f"invalid TOML: {err}") from err


def _package_version_from_manifest_data(manifest_path: Path, data: object) -> str:
    """Extract the package version from parsed manifest data."""
    if not isinstance(data, Mapping):
        raise ManifestVersionError(manifest_path, "manifest must be a table")
    try:
        package = data["package"]
        if not isinstance(package, Mapping):
            raise ManifestVersionError(manifest_path, "package must be a table")
        version = package["version"]
    except KeyError as err:
        raise ManifestVersionError(manifest_path, f"missing key: {err}") from err
    if not isinstance(version, str):
        raise ManifestVersionError(manifest_path, "package.version must be a string")
    return version


def archive_stem(target: str, version: str) -> str:
    """Return the cargo-binstall archive root directory name.

    Parameters
    ----------
    target : str
        Rust target triple to include in the archive name.
    version : str
        Package version without the leading `v`.

    Returns
    -------
    str
        Archive root directory stem.

    Raises
    ------
    SystemExit
        Raised when `target` is path-like or empty.

    Examples
    --------
    >>> archive_stem("x86_64-unknown-linux-gnu", "0.5.1")
    'pg-embed-setup-unpriv-x86_64-unknown-linux-gnu-v0.5.1'
    """
    _validate_path_component(target, "target")
    return f"{PACKAGE_NAME}-{target}-v{version}"


def release_binary_path(repo: Path, target: str, binary: str) -> Path:
    """Return Cargo's release output path for `binary` and `target`.

    Parameters
    ----------
    repo : Path
        Repository root containing the Cargo `target` directory.
    target : str
        Rust target triple to locate.
    binary : str
        Binary target name to locate.

    Returns
    -------
    Path
        Expected release binary path.

    Raises
    ------
    SystemExit
        Raised when `target` or `binary` is path-like or empty.

    Examples
    --------
    >>> release_binary_path(Path("."), "x86_64-unknown-linux-gnu", "pg_worker")
    PosixPath('target/x86_64-unknown-linux-gnu/release/pg_worker')
    """
    _validate_path_component(target, "target")
    _validate_path_component(binary, "binary")
    return repo / "target" / target / "release" / f"{binary}{binary_extension(target)}"


def build_release_binaries(spec: ReleaseBuildSpec) -> None:
    """Build the selected release binaries for `spec.target`.

    Parameters
    ----------
    spec : ReleaseBuildSpec
        Build configuration for Cargo invocation.

    Raises
    ------
    SystemExit
        Raised when validation fails or Cargo exits unsuccessfully.

    Examples
    --------
    >>> build_release_binaries(  # doctest: +SKIP
    ...     ReleaseBuildSpec(Path("."), "x86_64-unknown-linux-gnu", ("pg_worker",), "cargo")
    ... )
    """
    validate_release_spec_components(spec.target, spec.binaries)
    program, program_args = _cargo_program_and_args(spec.cargo)
    args = [*program_args, "build", "--release", "--target", spec.target]
    args.extend(_cargo_build_job_args(spec.build_jobs))
    for binary in spec.binaries:
        args.extend(["--bin", binary])

    command = cuprum_sh.make(Program(program), catalogue=_catalogue_for(program))
    result = command(*args).run_sync(
        capture=False,
        echo=True,
        context=ExecutionContext(cwd=spec.repo),
    )
    if result.exit_code != 0:
        raise SystemExit(result.exit_code)


def _cargo_program_and_args(cargo: str) -> tuple[str, list[str]]:
    """Return the executable and wrapper arguments represented by `cargo`."""
    stripped_cargo = cargo.strip()
    if not stripped_cargo:
        raise SystemExit("cargo executable cannot be empty")
    cargo_command = shlex.split(stripped_cargo)
    if not cargo_command:
        raise SystemExit("cargo executable cannot be empty")
    if windows_wrapper := _windows_wrapper_program_and_args(stripped_cargo):
        return windows_wrapper
    if path_wrapper := _path_wrapper_program_and_args(cargo_command):
        return path_wrapper
    if _looks_like_executable_path(stripped_cargo):
        return _strip_matching_quotes(stripped_cargo), []
    program, *program_args = cargo_command
    return program, program_args


def _windows_wrapper_program_and_args(cargo: str) -> tuple[str, list[str]] | None:
    """Return a Windows `.exe` wrapper command when `cargo` includes arguments."""
    windows_command = shlex.split(cargo, posix=False)
    windows_program = _strip_matching_quotes(windows_command[0])
    if len(windows_command) > 1 and windows_program.lower().endswith(".exe"):
        return windows_program, windows_command[1:]
    return None


def _path_wrapper_program_and_args(cargo_command: list[str]) -> tuple[str, list[str]] | None:
    """Return a path-like wrapper command when `cargo` was split as argv."""
    if len(cargo_command) > 1 and _looks_like_executable_path(cargo_command[0]):
        program, *program_args = cargo_command
        return _strip_matching_quotes(program), program_args
    return None


def validate_release_spec_components(target: str, binaries: tuple[str, ...]) -> None:
    """Reject release identifiers that could escape Cargo's output tree.

    Parameters
    ----------
    target : str
        Rust target triple used to locate Cargo build outputs.
    binaries : tuple[str, ...]
        Binary target names used to locate Cargo build outputs.

    Raises
    ------
    SystemExit
        Raised when `target` or any binary name is path-like or empty.

    Examples
    --------
    >>> validate_release_spec_components("x86_64-unknown-linux-gnu", ("pg_worker",))
    """
    _validate_path_component(target, "target")
    for binary in binaries:
        _validate_path_component(binary, "binary")


def _validate_path_component(value: str, kind: str) -> None:
    """Reject path-like values accepted only as release identifiers."""
    if message := _path_component_violation(value, kind):
        raise SystemExit(message)


def _path_component_violation(value: str, kind: str) -> str | None:
    """Return the first validation error for a release path component."""
    return (
        _empty_path_component_violation(value, kind)
        or _parent_dir_path_component_violation(value, kind)
        or _separator_path_component_violation(value, kind)
    )


def _empty_path_component_violation(value: str, kind: str) -> str | None:
    """Return the validation error for an empty release path component."""
    if not value:
        return f"{kind} cannot be empty"
    return None


def _parent_dir_path_component_violation(value: str, kind: str) -> str | None:
    """Return the validation error for a parent-directory path component."""
    if value in {".", ".."} or ".." in value:
        return f"{kind} cannot contain '..': {value}"
    return None


def _separator_path_component_violation(value: str, kind: str) -> str | None:
    """Return the validation error for path separators in a release component."""
    if any(separator in value for separator in PATH_SEPARATORS):
        return f"{kind} cannot contain path separators: {value}"
    return None


def _looks_like_executable_path(cargo: str) -> bool:
    """Return whether `cargo` names a path instead of a wrapper argv string."""
    executable = _strip_matching_quotes(cargo)
    return "/" in executable or "\\" in executable or executable.lower().endswith(".exe")


def _strip_matching_quotes(value: str) -> str:
    """Strip one matching shell-quote pair around an executable path."""
    if _has_matching_outer_quotes(value):
        return value[1:-1]
    return value


def _has_matching_outer_quotes(value: str) -> bool:
    """Return whether `value` is enclosed in one matching shell quote pair."""
    if len(value) < 2:
        return False
    first = value[0]
    last = value[-1]
    return first == last and first in SHELL_QUOTES


def _cargo_build_job_args(build_jobs: str | None) -> list[str]:
    """Return Cargo arguments represented by the Makefile `BUILD_JOBS` value."""
    if not build_jobs:
        return []
    parts = shlex.split(build_jobs)
    if len(parts) == 1 and parts[0].isdecimal():
        return ["--jobs", parts[0]]
    return parts


def _catalogue_for(cargo: str) -> ProgramCatalogue:
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
    """Stage release binaries and return the produced `.tgz` path.

    Parameters
    ----------
    spec : ReleaseArchiveSpec
        Archive staging configuration.

    Returns
    -------
    Path
        Path to the produced archive.

    Raises
    ------
    FileNotFoundError
        Raised when an expected release binary is missing.
    SystemExit
        Raised when target or binary validation fails.

    Examples
    --------
    >>> stage_archive(  # doctest: +SKIP
    ...     ReleaseArchiveSpec(Path("."), "x86_64-unknown-linux-gnu", "0.5.1", Path("dist"), ("pg_worker",))
    ... )
    PosixPath('dist/pg-embed-setup-unpriv-x86_64-unknown-linux-gnu-v0.5.1.tgz')
    """
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
    """Copy Cargo release binaries into the archive staging directory.

    Parameters
    ----------
    repo : Path
        Repository root containing Cargo build outputs.
    target : str
        Rust target triple to copy from.
    binaries : tuple[str, ...]
        Binary target names to copy.
    staging_root : Path
        Existing archive root directory where binaries are copied.

    Raises
    ------
    FileNotFoundError
        Raised when an expected release binary is missing.
    SystemExit
        Raised when target or binary validation fails.

    Examples
    --------
    >>> copy_release_binaries(Path("."), "x86_64-unknown-linux-gnu", ("pg_worker",), Path("stage"))  # doctest: +SKIP
    """
    for binary in binaries:
        source = release_binary_path(repo, target, binary)
        if not source.is_file():
            raise FileNotFoundError(f"release binary missing: {source}")
        shutil.copy2(source, staging_root / source.name)


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
