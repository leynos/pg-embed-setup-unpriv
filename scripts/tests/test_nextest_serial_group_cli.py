"""Ask nextest itself which binaries the ``serial`` group schedules.

``test_serial_group_contract.py`` reads ``.config/nextest.toml`` as text and
compares the filter with the sources. This runs nextest's own resolver, so a
configuration that reads correctly but schedules differently cannot pass
both: nextest applies the whole override chain, the embedded ``serial::``
test term and the group's thread limit, then prints the result.

A binary reaches the group by one of two routes. It is named in the filter,
because its own scenarios take the cross-process lock, or it compiles
``tests/support/serial.rs`` and its embedded ``serial::`` cases join by test
name. The expected set is therefore the pinned direct consumers plus every
binary that compiles the lock module, which is what the assertions below
compare against.

``show-config`` builds every test target before it can resolve the filters,
so this module is deliberately not part of ``make test-scripts``, which runs
before CI fetches dependencies. ``make test-serial-group`` assigns the same
``RUSTFLAGS`` as ``make test`` so the build is reused rather than repeated.

See "Test timeouts: four tiers, outermost last" in
``docs/developers-guide.md``.
"""

from __future__ import annotations

import re
import shutil
import subprocess
import typing as typ
from dataclasses import dataclass
from pathlib import Path

import pytest

from cluster_sources import TESTS_DIR, binary_sources, load_test_sources
from test_nextest_tier_contract import SERIAL_BINARIES

#: The repository whose configuration nextest resolves.
REPOSITORY_ROOT: typ.Final[Path] = Path(__file__).resolve().parents[2]

#: A group's heading, carrying the limit that makes nextest queue a test
#: before its per-test ``slow-timeout`` clock starts.
_GROUP_HEADING = re.compile(
    r"^group: (?P<name>\S+) \(max threads = (?P<threads>\d+)\)$"
)

#: A binary's heading inside a group, printed as ``<package>::<binary>:``.
_BINARY_HEADING = re.compile(r"^ {6}(?P<package>[^:\s]+)::(?P<binary>[^:\s]+):$")

#: Resolving the groups requires a built workspace. A cold build on a shared
#: host outlasts the default, and a queued build is not a failed one.
_BUILD_TIMEOUT = 2_400

#: The resolver command. Colours and the pager would corrupt the listing,
#: which is parsed line by line.
_SHOW_GROUPS = (
    "cargo",
    "nextest",
    "show-config",
    "test-groups",
    "--all-targets",
    "--all-features",
    "--no-pager",
    "--color",
    "never",
)


@dataclass(frozen=True)
class _Group:
    """One test group as nextest resolved it."""

    max_threads: int
    binaries: frozenset[str]


def parse_groups(listing: str) -> dict[str, _Group]:
    """Return each group nextest reports, with the binaries it schedules.

    Parameters
    ----------
    listing : str
        ``cargo nextest show-config test-groups`` output, without colour.

    Returns
    -------
    dict[str, _Group]
        Each group's thread limit and the binaries listed beneath it.

    Examples
    --------
    >>> parse_groups(
    ...     "group: serial (max threads = 1)\\n"
    ...     "      probe::alpha:\\n"
    ...     "          alpha\\n"
    ... )["serial"].binaries
    frozenset({'alpha'})
    """
    limits: dict[str, int] = {}
    binaries: dict[str, set[str]] = {}
    current: str | None = None
    for line in listing.splitlines():
        heading = _GROUP_HEADING.match(line)
        if heading:
            current = heading.group("name")
            limits[current] = int(heading.group("threads"))
            binaries.setdefault(current, set())
            continue
        entry = _BINARY_HEADING.match(line)
        if entry is not None and current is not None:
            binaries[current].add(entry.group("binary"))
    return {
        name: _Group(limits[name], frozenset(found))
        for name, found in binaries.items()
    }


@pytest.fixture(scope="module")
def groups() -> dict[str, _Group]:
    """Resolve the groups once by running nextest, or skip without it.

    Returns
    -------
    dict[str, _Group]
        The groups nextest schedules from the repository's configuration.

    Raises
    ------
    pytest.skip.Exception
        If ``cargo-nextest`` is not installed, because no run of the
        workspace can answer the question without nextest's resolver. The
        skip names that reason rather than reporting a vacuous pass.
    """
    if shutil.which("cargo-nextest") is None:
        pytest.skip(
            "cargo-nextest is not installed, so nextest cannot resolve the "
            "serial group; install it or run this target where CI installs it"
        )
    completed = subprocess.run(
        _SHOW_GROUPS,
        cwd=REPOSITORY_ROOT,
        capture_output=True,
        text=True,
        check=False,
        timeout=_BUILD_TIMEOUT,
    )
    assert completed.returncode == 0, (
        "nextest must resolve the groups: " + completed.stderr.strip()[-2000:]
    )
    return parse_groups(completed.stdout)


def _binaries_compiling_the_lock_module() -> frozenset[str]:
    """Return every binary whose crate root compiles the lock module."""
    root = TESTS_DIR.resolve()
    lock_module = root / "support" / "serial.rs"
    return frozenset(
        name
        for name, compiled in binary_sources(
            load_test_sources(root), root=root
        ).items()
        if lock_module in compiled
    )


def test_nextest_schedules_every_lock_consumer_in_the_serial_group(
    groups: dict[str, _Group],
) -> None:
    """nextest's resolver agrees with the sources the contract reads.

    A lock consumer that nextest leaves out starts concurrently with the
    rest, blocks in ``flock`` and can exceed the 180 s default, which is the
    defect #254 reports. A binary that joins without a lock use is a smaller
    cost, but it means the trigger is not what the configuration says, so it
    is reported for review rather than accepted silently.
    """
    serial = groups.get("serial")
    assert serial is not None, (
        f"nextest must report the serial group: {sorted(groups)}"
    )
    assert serial.max_threads == 1, (
        "the serial group must schedule one test at a time, got "
        f"{serial.max_threads}"
    )
    missing = SERIAL_BINARIES - serial.binaries
    assert not missing, (
        "every pinned direct lock consumer must be scheduled in the group: "
        f"{sorted(missing)}"
    )
    compilers = _binaries_compiling_the_lock_module()
    unscheduled = compilers - serial.binaries
    assert not unscheduled, (
        "a binary compiling tests/support/serial.rs must join through the "
        f"embedded serial:: term: {sorted(unscheduled)}"
    )
    unexpected = serial.binaries - SERIAL_BINARIES - compilers
    assert not unexpected, (
        "only the pinned consumers and the binaries compiling the lock "
        f"module may join the group: {sorted(unexpected)}"
    )