"""Discover the integration-test binaries that compile the serial module.

A binary that compiles ``tests/support/serial.rs`` takes the cross-process
scenario lock (``pg-embed-setup-unpriv.serial.lock``), so the nextest
``serial`` group must hold it (#254). Discovery is deliberately narrow: a
binary counts when its root file names the module directly, as every
current root does, whether Cargo finds that root as ``tests/<name>.rs`` or
``tests/<name>/main.rs``.

A support module that compiled the module on a root's behalf would hide the
dependency, so the inventory reports such a file instead of guessing which
binary reaches it; move the attribute into the root to make the binary
discoverable. Following arbitrary local module imports would need a Rust
module resolver, and the layout this crate uses does not need one.

The TOML accessors come from :mod:`timeout_budgets`, which both this module
and the group contract use to tolerate a key that is absent or of the wrong
type without raising.
"""

from __future__ import annotations

import fnmatch
import os
import re
import tomllib
import typing as typ
from pathlib import Path

from timeout_budgets import mapping_or_empty, sequence_or_empty

#: The module a root file names to take the cross-process scenario lock.
#:
#: A directory module's root, such as ``tests/test_cluster_fixture/mod.rs``,
#: reaches the shared file as ``../support/serial.rs``, so the attribute
#: pattern accepts any relative prefix instead of pinning the top-level
#: spelling only.
SERIAL_MODULE_PATH: typ.Final[str] = "support/serial.rs"
_SERIAL_MODULE_PATTERN: typ.Final[re.Pattern[str]] = re.compile(
    r'#\[path\s*=\s*"[^"]*' + re.escape(SERIAL_MODULE_PATH) + r'"\]'
)


def _declared_targets(manifest: dict[str, object], root: Path) -> dict[str, Path]:
    """Return integration-test names mapped to their declared root files.

    Parameters
    ----------
    manifest : dict of str to object
        The parsed ``Cargo.toml``.
    root : Path
        The directory the manifest's relative paths are resolved against.

    Returns
    -------
    dict of str to Path
        One entry per ``[[test]]`` declaration.
    """
    targets: dict[str, Path] = {}
    for entry in sequence_or_empty(manifest.get("test")):
        table = mapping_or_empty(entry)
        match table.get("path"), table.get("name"):
            case (str() as path, str() as name):
                targets[name] = root / path
            case (str() as path, _):
                targets[Path(path).stem] = root / path
            case (_, str() as name):
                targets[name] = root / "tests" / f"{name}.rs"
    return targets


def _read_text(path: Path, label: str) -> tuple[str | None, str | None]:
    """Read a source file, turning an I/O failure into a fault message.

    Parameters
    ----------
    path : Path
        The file to read.
    label : str
        How the fault message should name the file.

    Returns
    -------
    tuple
        The text, or ``None`` after a failure, and the fault or ``None``.
    """
    try:
        return path.read_text(encoding="utf-8"), None
    except OSError as error:
        return None, f"cannot read {label}: {error}"


def _scanned_paths(
    directory: Path, pattern: str, *, recursive: bool
) -> tuple[list[Path], list[str]]:
    """List matching files under ``directory``, reporting a scan failure.

    ``Path.glob`` and ``Path.rglob`` suppress ``OSError`` raised while
    enumerating on Python 3.13, so an unreadable directory would contribute
    no matches without any failure being visible. Walking with
    :func:`os.walk` and its ``onerror`` callback keeps that failure, which
    matters because a lock-taking binary inside such a directory would
    otherwise be omitted from the inventory silently.

    The walk descends only as far as the pattern's own separator count when
    ``recursive`` is false, so ``*.rs`` names the roots directly inside
    ``tests`` while ``*/main.rs`` names those one level below it.

    Parameters
    ----------
    directory : Path
        The directory to search.
    pattern : str
        The glob pattern, matched against a POSIX relative path.
    recursive : bool
        Whether to search subdirectories as well.

    Returns
    -------
    tuple
        The matching paths in a stable order, and any scan faults.
    """
    faults: list[str] = []

    def report(error: OSError) -> None:
        """Record a directory the walk could not list."""
        faults.append(f"cannot list {pattern} under {directory}: {error}")

    max_depth = None if recursive else pattern.count("/")
    matches: list[Path] = []
    for root, directories, files in os.walk(directory, onerror=report):
        root_path = Path(root)
        relative_root = root_path.relative_to(directory)
        if max_depth is not None and len(relative_root.parts) >= max_depth:
            directories[:] = []
        for name in files:
            if fnmatch.fnmatchcase((relative_root / name).as_posix(), pattern):
                matches.append(root_path / name)
    return sorted(matches), faults


def _serial_includers(
    tests_directory: Path,
) -> tuple[frozenset[Path], list[str]]:
    """Return every file under ``tests`` that compiles the serial module.

    Parameters
    ----------
    tests_directory : Path
        The ``tests`` directory to scan.

    Returns
    -------
    tuple
        The resolved paths that name the module, and any read faults.
    """
    includers: set[Path] = set()
    paths, faults = _scanned_paths(tests_directory, "*.rs", recursive=True)
    for path in paths:
        text, fault = _read_text(path, str(path))
        if fault is not None:
            faults.append(fault)
        elif text is not None and _SERIAL_MODULE_PATTERN.search(text):
            includers.add(path.resolve())
    return frozenset(includers), faults


def _lock_taking_names(
    targets: dict[str, Path],
) -> tuple[set[str], list[str]]:
    """Return the targets whose root file compiles the serial module.

    Parameters
    ----------
    targets : dict of str to Path
        Integration-test names mapped to their root files.

    Returns
    -------
    tuple
        The names that compile the module, and any read faults.
    """
    lock_taking: set[str] = set()
    faults: list[str] = []
    for name, root_file in sorted(targets.items()):
        label = f"the root file for binary {name!r} at {root_file}"
        text, fault = _read_text(root_file, label)
        if fault is not None:
            faults.append(fault)
        elif text is not None and _SERIAL_MODULE_PATTERN.search(text):
            lock_taking.add(name)
    return lock_taking, faults


def _hidden_includer_faults(
    targets: dict[str, Path], includers: frozenset[Path]
) -> list[str]:
    """Report serial-module inclusions that no declared root explains.

    Parameters
    ----------
    targets : dict of str to Path
        Integration-test names mapped to their root files.
    includers : frozenset of Path
        Every file under ``tests`` that compiles the module.

    Returns
    -------
    list of str
        One fault naming the unattributable files, or empty.
    """
    hidden = sorted(includers - {target.resolve() for target in targets.values()})
    if not hidden:
        return []
    return [
        "the serial module is compiled outside a declared test root, so no "
        "binary can be attributed: " + ", ".join(str(path) for path in hidden)
    ]


def lock_taking_binaries(
    manifest_text: str, tests_directory: Path
) -> tuple[frozenset[str], list[str]]:
    """Discover the binaries whose root file compiles the serial module.

    Declared ``[[test]]`` targets and undeclared roots are all considered,
    whether a root is ``tests/<name>.rs`` or ``tests/<name>/main.rs``, so a
    new binary that includes the module is found even before its manifest
    entry or filter term is added. Only a root file counts, so any other file
    that compiles the module is reported rather than silently left
    unattributable.

    Parameters
    ----------
    manifest_text : str
        The ``Cargo.toml`` text.
    tests_directory : Path
        The ``tests`` directory to scan.

    Returns
    -------
    tuple
        The discovered lock-taking names, and any discovery faults.
    """
    try:
        manifest = tomllib.loads(manifest_text)
    except tomllib.TOMLDecodeError as error:
        return frozenset(), [f"Cargo.toml is not valid TOML: {error}"]
    targets = _declared_targets(manifest, tests_directory.parent)
    flat_roots, flat_faults = _scanned_paths(
        tests_directory, "*.rs", recursive=False
    )
    directory_roots, directory_faults = _scanned_paths(
        tests_directory, "*/main.rs", recursive=False
    )
    for path in flat_roots:
        targets.setdefault(path.stem, path)
    for path in directory_roots:
        targets.setdefault(path.parent.name, path)
    lock_taking, faults = _lock_taking_names(targets)
    includers, read_faults = _serial_includers(tests_directory)
    faults.extend(flat_faults)
    faults.extend(directory_faults)
    faults.extend(read_faults)
    faults.extend(_hidden_includer_faults(targets, includers))
    return frozenset(lock_taking), faults