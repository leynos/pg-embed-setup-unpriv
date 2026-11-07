"""Discover the integration-test binaries that take the scenario lock.

``tests/support/serial.rs`` guards the shared PostgreSQL cache, the
installation directories and process-wide state with a cross-process file
lock. A binary whose own cases take ``serial_guard``, or hold a
``ScenarioSerialGuard``, contends for that lock, so the nextest ``serial``
group must hold it (#254). Discovery reads the sources rather than a list,
because a value pinned in a manually maintained list cannot notice a binary
nobody listed.

The module's own unit tests take the same lock, but the group covers them
with its embedded ``serial::tests::*`` term instead of whole-binary
membership. The module's files are therefore excluded here: counting their
``serial_guard`` uses would make every binary that compiles the module a
member, including the ones whose scenarios are isolated behind
``local_serial_guard``. That over-approximation is what this contract
replaces.

Reading deliberately reuses :mod:`cluster_sources`, whose ``binary_sources``
resolves a crate root and the modules it declares, including a ``#[path]``
override into ``tests/support``. A lock use in a support module is thereby
attributed to the binary that compiles it, and a lock use no binary compiles
is reported rather than silently ignored.

The scan is lexical, so a reference in a comment counts. A false positive is
a prompt to review the binary rather than a defect in the reader.
"""

from __future__ import annotations

import collections.abc as cabc
import re
import typing as typ
from pathlib import Path

from cluster_sources import (
    TESTS_DIR,
    SourceLoadError,
    binary_sources,
    load_test_sources,
)

#: The API that takes the cross-process lock. ``local_serial_guard`` and
#: ``ScenarioLocalGuard`` are separate identifiers, so a word-boundary match
#: does not confuse the in-process guard with the shared one.
_LOCK_API: typ.Final[re.Pattern[str]] = re.compile(
    r"\b(?:serial_guard|ScenarioSerialGuard)\b"
)

#: The scenario-lock module, whose own uses the group's embedded term covers.
_LOCK_MODULE: typ.Final[Path] = Path("support") / "serial.rs"
_LOCK_MODULE_DIRECTORY: typ.Final[Path] = Path("support") / "serial"


def _is_lock_module(path: Path, root: Path) -> bool:
    """Return whether ``path`` belongs to the scenario-lock module itself.

    Parameters
    ----------
    path : Path
        A resolved source path.
    root : Path
        The resolved directory holding the integration-test sources.

    Returns
    -------
    bool
        True for the module file and every file in its directory.
    """
    relative = path.relative_to(root)
    return (
        relative == _LOCK_MODULE
        or _LOCK_MODULE_DIRECTORY in relative.parents
    )


def lock_consuming_binaries(
    sources: cabc.Mapping[Path, str], *, root: Path
) -> tuple[frozenset[str], list[str]]:
    """Classify loaded test sources by whether they take the lock.

    Parameters
    ----------
    sources : Mapping[Path, str]
        The loaded test sources, keyed by resolved path.
    root : Path
        The resolved directory holding the integration-test sources.

    Returns
    -------
    tuple
        The binaries whose own code takes the lock, and any faults. A
        source outside every binary that takes the lock is a fault,
        because no filter term can schedule it.
    """
    binaries = binary_sources(sources, root=root)
    members: set[str] = set()
    attributed: set[Path] = set()
    for name, compiled in binaries.items():
        attributed.update(compiled)
        own_sources = [
            source
            for source in sorted(compiled)
            if not _is_lock_module(source, root)
        ]
        if any(_LOCK_API.search(sources[source]) for source in own_sources):
            members.add(name)
    orphans = sorted(
        source
        for source in sources
        if source not in attributed
        and not _is_lock_module(source, root)
        and _LOCK_API.search(sources[source])
    )
    faults: list[str] = []
    if orphans:
        faults.append(
            "the scenario lock is taken outside any test binary, so no "
            "filter term can schedule it: "
            + ", ".join(str(source) for source in orphans)
        )
    return frozenset(members), faults


def scan_lock_consuming_binaries(
    tests_dir: Path = TESTS_DIR,
) -> tuple[frozenset[str], list[str]]:
    """Load the test tree and return the binaries that take the lock.

    Parameters
    ----------
    tests_dir : Path
        The directory holding the integration-test sources.

    Returns
    -------
    tuple
        The discovered binary names, and any faults. A tree that cannot
        be read is reported as a fault rather than raised, so the caller
        receives every way of drifting through one channel.

    Examples
    --------
    >>> members, faults = scan_lock_consuming_binaries()
    >>> isinstance(members, frozenset), faults
    (True, [])
    """
    root = tests_dir.resolve()
    try:
        sources = load_test_sources(root)
    except SourceLoadError as error:
        return frozenset(), [str(error)]
    return lock_consuming_binaries(sources, root=root)