"""Derive the child-spawning cluster binaries from the test sources.

``test_nextest_tier_contract.py`` pins the cluster registry by value, but
a value pinned in one manually maintained list cannot notice a binary
that was never listed. This contract reads the test sources and finds
the normal-run binaries that spawn child processes and reference a
cluster-bootstrap API, then requires every one to be in the cluster
tier. A parent test's allowance covers its children's startup and
waiting, so a new such binary cannot silently stay on the 180 s default.

The scan is lexical, so a reference in a comment counts. A false
positive is a prompt to review the binary and, if it is genuinely
outside the tier, add it to ``CHILD_CLUSTER_EXCEPTIONS`` with a reason.

Timeout-tier membership is deliberately separate from membership in the
``serial`` test group: an isolated root removes the need for the shared
scenario lock, not the cost of a cold cluster start.

See "Test timeouts: four tiers, outermost last" in
``docs/developers-guide.md``.
"""

import re
import typing as typ
from pathlib import Path

from test_nextest_tier_contract import CLUSTER_BINARIES, SERIAL_BINARIES

TESTS_DIR = Path(__file__).resolve().parents[2] / "tests"

#: A reference to an API that bootstraps a real PostgreSQL cluster.
_CLUSTER_API = re.compile(
    r"shared_cluster_handle|bootstrap_for_tests|TestCluster|test_cluster::"
)

#: A reference to spawning another process from a test.
_CHILD_PROCESS = re.compile(r"current_exe\(|spawn_child\(|Command::new\(")

#: Binaries that spawn children and reference a cluster API, but are not
#: normal-run parents whose allowance covers real cluster startup.
CHILD_CLUSTER_EXCEPTIONS: typ.Final[dict[str, str]] = {
    "shared_cluster_retry": (
        "each child points every proxy at a closed port, so its bootstrap "
        "fails without starting a cluster; the parent is bounded by the "
        "retry's attempt count"
    ),
    "start_ramp": (
        "every test carries `#[ignore]`; it is a manual ramp measurement, "
        "not part of a normal run"
    ),
}

#: Parent tests that spawn children bootstrapping real clusters, but whose
#: roots are isolated. They take no scenario lock.
ISOLATED_CHILD_CLUSTER_BINARIES: typ.Final[frozenset[str]] = frozenset(
    {
        "durability_defaults",
        "fresh_cluster_per_process",
        "per_cluster_directories",
    }
)

#: Binaries whose scenarios use the local guard with isolated sandboxes.
LOCAL_GUARD_BINARIES: typ.Final[frozenset[str]] = frozenset(
    {"data_dir_recovery", "observability", "test_cluster_async"}
)


def _source_files() -> typ.Iterator[tuple[str, Path]]:
    """Yield each test binary's name and its source path.

    Returns
    -------
    Iterator[tuple[str, Path]]
        Top-level ``tests/*.rs`` binaries, plus the directory-backed
        ``test_cluster_fixture`` target.
    """
    for path in sorted(TESTS_DIR.glob("*.rs")):
        yield path.stem, path
    yield "test_cluster_fixture", TESTS_DIR / "test_cluster_fixture" / "mod.rs"


def spawning_cluster_binaries() -> frozenset[str]:
    """Return binaries that spawn children and reference a cluster API.

    Returns
    -------
    frozenset[str]
        Test binary names matching both lexical markers.
    """
    found = set()
    for name, path in _source_files():
        source = path.read_text(encoding="utf-8")
        if _CHILD_PROCESS.search(source) and _CLUSTER_API.search(source):
            found.add(name)
    return frozenset(found)


def test_the_scan_names_the_known_parent_child_cluster_binaries() -> None:
    """The derived set is pinned, so a scanner regression is visible.

    The exceptions and the derived set are checked together: a new
    parent/child cluster binary changes this result, which makes the
    author decide between the cluster tier and a documented exception
    instead of inheriting the default budget.
    """
    assert spawning_cluster_binaries() == frozenset(
        {
            "durability_defaults",
            "fresh_cluster_per_process",
            "per_cluster_directories",
            "shared_cluster_retry",
            "shutdown_hook_lifecycle",
            "start_ramp",
        }
    )


def test_every_normal_run_child_cluster_binary_is_in_the_tier() -> None:
    """A normal-run parent that starts a real cluster is in the cluster tier.

    The three isolated binaries in ``ISOLATED_CHILD_CLUSTER_BINARIES``
    are the cases this rule was written for: they were missing from the
    tier while their children bootstrapped real clusters against the
    parent's 180 s default. An exception may not also be in the tier,
    because its reason says why it is outside it.
    """
    derived = spawning_cluster_binaries() - CHILD_CLUSTER_EXCEPTIONS.keys()
    missing = derived - CLUSTER_BINARIES
    assert not missing, (
        f"{sorted(missing)} spawn cluster-bootstrapping children but are "
        f"not in the cluster tier; add each to the tier or record a reason "
        f"in CHILD_CLUSTER_EXCEPTIONS"
    )
    conflicting = CHILD_CLUSTER_EXCEPTIONS.keys() & CLUSTER_BINARIES
    assert not conflicting, (
        f"{sorted(conflicting)} are both in the cluster tier and documented "
        f"as exceptions; remove one"
    )


def test_isolated_child_cluster_binaries_take_no_scenario_lock() -> None:
    """Tier membership does not imply serial-group membership.

    The isolated roots of these binaries remove the need for the shared
    scenario lock. They still pay cold-cluster startup, which is what
    the tier covers, so they belong in exactly one of the two lists.
    """
    assert ISOLATED_CHILD_CLUSTER_BINARIES <= CLUSTER_BINARIES, (
        "every isolated child-cluster binary must be in the cluster tier"
    )
    assert ISOLATED_CHILD_CLUSTER_BINARIES.isdisjoint(SERIAL_BINARIES), (
        "an isolated child-cluster binary must not be a whole-binary serial "
        "member; only its embedded `serial::tests::*` cases take the lock"
    )


def test_local_guard_binaries_are_not_whole_binary_serial_members() -> None:
    """The local guard names its own isolation, so no whole-binary entry.

    Reintroducing one would serialize isolated sandboxes for no gain
    and make the registry's "direct consumers" description inaccurate.
    """
    assert LOCAL_GUARD_BINARIES.isdisjoint(SERIAL_BINARIES), (
        "local-guard binaries must not be whole-binary serial members; the "
        "embedded term covers the lock's own unit tests"
    )
    assert LOCAL_GUARD_BINARIES <= CLUSTER_BINARIES, (
        "every local-guard binary here still starts a real cluster and must "
        "keep the cluster tier"
    )
