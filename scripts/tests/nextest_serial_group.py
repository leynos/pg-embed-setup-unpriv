"""Read the groups nextest's own resolver reports.

``test_nextest_serial_group_cli.py`` runs
``cargo nextest show-config test-groups`` and compares the result with the
sources. This module owns the reading half of that: nextest prints each
group's heading with its thread limit and then the binaries it schedules
beneath it, and the parser turns that listing into one entry per group.

The parser lives here rather than in the test module because ``make
test-scripts`` collects the doctests of the modules it lists, and collecting
a test module also runs its tests. The resolver test needs a built
workspace, so it belongs to ``make test-serial-group`` alone; a helper
module carries no tests of its own.

See "Test timeouts: four tiers, outermost last" in
``docs/developers-guide.md``.
"""

from __future__ import annotations

import re
from dataclasses import dataclass

#: A group's heading, carrying the limit that makes nextest queue a test
#: before its per-test ``slow-timeout`` clock starts.
_GROUP_HEADING = re.compile(
    r"^group: (?P<name>\S+) \(max threads = (?P<threads>\d+)\)$"
)

#: A binary's heading inside a group, printed as ``<package>::<binary>:``.
_BINARY_HEADING = re.compile(r"^ {6}(?P<package>[^:\s]+)::(?P<binary>[^:\s]+):$")


@dataclass(frozen=True)
class Group:
    """One test group as nextest resolved it."""

    max_threads: int
    binaries: frozenset[str]


def parse_groups(listing: str) -> dict[str, Group]:
    """Return each group nextest reports, with the binaries it schedules.

    A binary heading names its package and binary; only the binary name is
    kept, because the filter terms the contract reads are binary names.

    Parameters
    ----------
    listing : str
        ``cargo nextest show-config test-groups`` output, without colour.

    Returns
    -------
    dict[str, Group]
        Each group's thread limit and the binaries listed beneath it, keyed
        by the group's name. A group that schedules nothing is present with
        an empty set rather than absent.

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
        name: Group(limits[name], frozenset(found))
        for name, found in binaries.items()
    }