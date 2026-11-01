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

import pytest

from test_nextest_tier_contract import CLUSTER_BINARIES, SERIAL_BINARIES

TESTS_DIR = Path(__file__).resolve().parents[2] / "tests"

#: A reference to an API that bootstraps a real PostgreSQL cluster.
_CLUSTER_API = re.compile(
    r"shared_cluster_handle|bootstrap_for_tests|TestCluster|test_cluster::"
)

#: A reference to spawning another process from a test.
_CHILD_PROCESS = re.compile(r"current_exe\(|spawn_child\(|Command::new\(")

#: A module declaration a test binary compiles in, with or without a
#: `#[path]` attribute that overrides where its file lives.
_MODULE_DECLARATION = re.compile(
    r'(?:#\[path\s*=\s*"(?P<path>[^"]+)"\]\s*)?'
    r"(?:pub(?:\([^)]*\))?\s+)?mod\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)\s*;"
)

#: Binaries that spawn children and reference a cluster API, but are not
#: normal-run parents whose allowance covers real cluster startup.
CHILD_CLUSTER_EXCEPTIONS: typ.Final[dict[str, str]] = {
    "bootstrap_worker_binary": (
        "the child it spawns is the `pg_worker` helper, and the tests cover "
        "worker-path validation that fails before any cluster starts; its "
        "`bootstrap_for_tests` calls run in-process to produce that failure"
    ),
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


def _source_files(tests_dir: Path = TESTS_DIR) -> typ.Iterator[tuple[str, Path]]:
    """Yield each test binary's name and its crate-root source path.

    Parameters
    ----------
    tests_dir : Path
        The directory holding the integration-test sources.

    Returns
    -------
    Iterator[tuple[str, Path]]
        Top-level ``tests/*.rs`` binaries, plus the directory-backed
        ``test_cluster_fixture`` target when it exists.
    """
    for path in sorted(tests_dir.glob("*.rs")):
        yield path.stem, path
    fixture = tests_dir / "test_cluster_fixture" / "mod.rs"
    if fixture.is_file():
        yield "test_cluster_fixture", fixture


def _module_directory(declaring_file: Path, *, is_crate_root: bool) -> Path:
    """Return where a plain ``mod name;`` declared by this file lives.

    A crate root and a ``mod.rs`` both search their own directory: for
    ``tests/parent.rs``, ``mod child;`` is ``tests/child.rs``, not
    ``tests/parent/child.rs``. Only an ordinary module file, such as
    ``tests/parent.rs``'s ``tests/parent/child.rs``, searches a
    directory named after itself.
    """
    if is_crate_root or declaring_file.name == "mod.rs":
        return declaring_file.parent
    return declaring_file.parent / declaring_file.stem


def _module_file(
    declaring_file: Path, declared: re.Match[str], *, is_crate_root: bool
) -> Path | None:
    """Return the file a module declaration compiles in, if it exists.

    A ``#[path]`` attribute overrides the search and is taken relative
    to the declaring file, which is how the test crates reach
    ``tests/support``. A plain declaration searches the sibling file
    and then the directory form. Declarations nested inside inline
    ``mod`` blocks are not modelled; no test crate uses that shape.
    """
    override = declared.group("path")
    name = declared.group("name")
    if override is not None:
        candidates = [declaring_file.parent / override]
    else:
        base = _module_directory(declaring_file, is_crate_root=is_crate_root)
        candidates = [base / f"{name}.rs", base / name / "mod.rs"]
    return next((path for path in candidates if path.is_file()), None)


def _compiled_sources(
    path: Path, seen: dict[Path, str], *, is_crate_root: bool
) -> dict[Path, str]:
    """Return every source file a test binary compiles in, by path.

    Parameters
    ----------
    path : Path
        A crate root or an included module file.
    seen : dict[Path, str]
        The files already read, mapping each to its text, which also
        stops a module cycle from recursing.
    is_crate_root : bool
        Whether ``path`` is the binary's root, which changes where its
        plain module declarations resolve.

    Returns
    -------
    dict[Path, str]
        The text of ``path`` and, transitively, of every module it
        declares.
    """
    resolved = path.resolve()
    if resolved in seen:
        return seen
    source = resolved.read_text(encoding="utf-8")
    seen[resolved] = source
    for declared in _MODULE_DECLARATION.finditer(source):
        module = _module_file(resolved, declared, is_crate_root=is_crate_root)
        if module is not None:
            _compiled_sources(module, seen, is_crate_root=False)
    return seen


def spawning_cluster_binaries(
    tests_dir: Path = TESTS_DIR,
) -> frozenset[str]:
    """Return binaries that spawn children and reference a cluster API.

    The markers are searched across every file the binary compiles in,
    not only its crate root: a parent can put the child-process spawn in
    one module and the cluster bootstrap in another.

    Parameters
    ----------
    tests_dir : Path
        The directory holding the integration-test sources.

    Returns
    -------
    frozenset[str]
        Test binary names matching both lexical markers.
    """
    found = set()
    for name, path in _source_files(tests_dir):
        sources = _compiled_sources(path, {}, is_crate_root=True)
        text = "\n".join(sources[source] for source in sorted(sources))
        if _CHILD_PROCESS.search(text) and _CLUSTER_API.search(text):
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
            "bootstrap_worker_binary",
            "durability_defaults",
            "fresh_cluster_per_process",
            "per_cluster_directories",
            "shared_cluster_retry",
            "shutdown_hook_lifecycle",
            "start_ramp",
        }
    )


_SPAWN = "fn spawn() { spawn_child(); }\n"
_BOOTSTRAP = "fn bootstrap() -> TestCluster { todo!() }\n"

#: Module layouts a test binary can use, each placing the spawn marker
#: and the cluster marker in different files. The crate-root case is the
#: one most easily got wrong: ``mod cluster_api;`` beside a crate root
#: resolves next to it, not into a directory named after the root.
_SEPARATE_MODULE_LAYOUTS: dict[str, dict[str, str]] = {
    "crate-root-siblings": {
        "parent.rs": (
            '#[path = "spawned_child.rs"]\nmod spawned_child;\nmod cluster_api;\n'
        ),
        "spawned_child.rs": _SPAWN,
        "cluster_api.rs": _BOOTSTRAP,
    },
    "ordinary-module-file": {
        "parent.rs": "mod child;\n",
        "child.rs": f"mod cluster_api;\n{_SPAWN}",
        "child/cluster_api.rs": _BOOTSTRAP,
    },
    "directory-form": {
        "parent.rs": "mod child;\n",
        "child/mod.rs": f"mod cluster_api;\n{_SPAWN}",
        "child/cluster_api.rs": _BOOTSTRAP,
    },
}


@pytest.mark.parametrize(
    "layout",
    _SEPARATE_MODULE_LAYOUTS.values(),
    ids=_SEPARATE_MODULE_LAYOUTS.keys(),
)
def test_markers_in_separate_included_modules_are_found(
    layout: dict[str, str], tmp_path: Path
) -> None:
    """A parent may spawn in one module and bootstrap in another.

    Both marker searches run over every file the binary compiles in, so
    a parent that keeps the child-process spawn and the cluster
    bootstrap in different modules is still detected. Each layout is a
    module-resolution rule: a ``#[path]`` override beside the crate
    root, an ordinary module file's own directory, and the directory
    form of a plain declaration.
    """
    for relative, source in layout.items():
        path = tmp_path / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(source, encoding="utf-8")

    assert spawning_cluster_binaries(tmp_path) == frozenset({"parent"}), (
        "markers split across included modules must still name the parent"
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
