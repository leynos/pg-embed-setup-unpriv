"""Manifest inspection and cargo-binstall archive staging."""

from __future__ import annotations

import shutil
import tarfile
import tomllib
from collections.abc import Mapping
from dataclasses import dataclass
from pathlib import Path
from tempfile import TemporaryDirectory
from typing import Protocol

PACKAGE_NAME = "pg-embed-setup-unpriv"
PATH_SEPARATORS = frozenset({"/", "\\"})


class ReleaseArchiveSpecLike(Protocol):
    """Archive inputs consumed by the staging flow."""

    repo: Path
    target: str
    version: str
    dist_dir: Path
    binaries: tuple[str, ...]


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


def stage_archive(spec: ReleaseArchiveSpecLike) -> Path:
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
