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

#: A binary's heading inside a group. nextest prints an integration test as
#: ``<package>::<binary>:`` and gives a library or proc-macro target the
#: package name alone, because one of each exists per package. Both forms are
#: headings, so a group that schedules library tests does not lose them.
_BINARY_HEADING = re.compile(r"^ {6}(?P<id>[^:\s]+(?:::[^:\s]+)?):$")


@dataclass(frozen=True)
class Group:
    """One test group as nextest resolved it."""

    max_threads: int
    binaries: frozenset[str]


def _binary_name(identifier: str) -> str:
    """Return the name a filter term uses for a binary identifier.

    Parameters
    ----------
    identifier : str
        A heading's identifier, as nextest prints it.

    Returns
    -------
    str
        The part after ``::`` for an integration test, which is the target
        name ``binary(=...)`` matches. A library or proc-macro heading has no
        separator and the package name is already that name.

    Examples
    --------
    >>> _binary_name("probe::alpha"), _binary_name("probe")
    ('alpha', 'probe')
    """
    _, separator, name = identifier.partition("::")
    return name if separator else identifier


def parse_groups(listing: str) -> dict[str, Group]:
    """Return each group nextest reports, with the binaries it schedules.

    A binary heading names its package and binary, or its package alone for a
    library or proc-macro target; only the binary name is kept, because the
    filter terms the contract reads are binary names.

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
    An integration test keeps the target name, and a library target keeps the
    package name nextest gives it; neither heading is discarded.
    >>> sorted(
    ...     parse_groups(
    ...         "group: serial (max threads = 1)\\n"
    ...         "      probe::alpha:\\n"
    ...         "          alpha\\n"
    ...         "      probe:\\n"
    ...         "          serial::tests::takes_lock\\n"
    ...     )["serial"].binaries
    ... )
    ['alpha', 'probe']
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
            binaries[current].add(_binary_name(entry.group("id")))
    return {
        name: Group(limits[name], frozenset(found))
        for name, found in binaries.items()
    }