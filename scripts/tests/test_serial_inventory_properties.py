"""Property test: discovery follows the module a binary actually compiles.

The contract's examples pin the shapes that were once wrong. These generate
trees instead, because a rule about attribution can hold for the three shapes
someone thought of and fail for the fourth: a lock use reached through a
shared ``#[path]`` module, a binary that includes the lock module but keeps
its own cases isolated, a binary that takes the lock itself, and a use no
binary compiles all have to keep their own answer.

See "Test timeouts: four tiers, outermost last" in
``docs/developers-guide.md``.
"""

from __future__ import annotations

import tempfile
from dataclasses import dataclass
from pathlib import Path

from hypothesis import given, settings
from hypothesis import strategies as st

from cluster_sources import load_test_sources
from serial_inventory import lock_consuming_binaries

#: Text that takes the cross-process lock.
_LOCKING = "fn f(_g: serial::ScenarioSerialGuard) {}\n"

#: Text that takes only the in-process guard.
_ISOLATED = "fn f(_g: serial::ScenarioLocalGuard) {}\n"

#: The real scenario-lock module's file name under ``support``.
_LOCK_MODULE = "serial"

#: A support module no binary compiles.
_ORPHAN = "orphan"

#: Each example writes a tree and scans it, which the default 200 ms deadline
#: does not allow for on a loaded runner. A slow example is not a defect.
_GENERATED = settings(max_examples=25, deadline=None)

_NAMES = st.lists(
    st.from_regex(r"[a-z][a-z0-9_]{0,8}", fullmatch=True),
    min_size=1,
    max_size=5,
    unique=True,
)

#: Generated modules are prefixed so they cannot shadow the lock module.
_MODULE_NAMES = st.lists(
    st.from_regex(r"mod_[a-z0-9_]{0,6}", fullmatch=True),
    max_size=3,
    unique=True,
)


@dataclass(frozen=True)
class _Binary:
    """One binary: its name, its own lock use, and the modules it includes."""

    name: str
    takes_lock: bool
    includes: frozenset[str]


@dataclass(frozen=True)
class _Tree:
    """The generated tree: its binaries, its modules and its orphan."""

    binaries: tuple[_Binary, ...]
    modules: tuple[tuple[str, bool], ...]
    orphan_takes_lock: bool


@st.composite
def _trees(draw: st.DrawFn) -> _Tree:
    """Generate one tree whose expected attribution is computable.

    Parameters
    ----------
    draw : st.DrawFn
        The Hypothesis draw function.

    Returns
    -------
    _Tree
        Binaries with unique names, support modules shared between them,
        and an orphan that no binary compiles.
    """
    module_names = draw(_MODULE_NAMES)
    modules = tuple((name, draw(st.booleans())) for name in module_names)
    binaries = []
    for index, name in enumerate(draw(_NAMES)):
        includes = (
            draw(st.frozensets(st.sampled_from(module_names)))
            if module_names
            else frozenset()
        )
        if index == 0:
            # The first binary always compiles the lock module, so every
            # example exercises its exclusion from whole-binary membership.
            includes = includes | {_LOCK_MODULE}
        binaries.append(_Binary(name, draw(st.booleans()), includes))
    return _Tree(tuple(binaries), modules, draw(st.booleans()))


def _source_text(takes_lock: bool) -> str:
    """Return the body of a source that does or does not take the lock."""
    return _LOCKING if takes_lock else _ISOLATED


def _write_module(tests_directory: Path, name: str, takes_lock: bool) -> None:
    """Write one support module, which several binaries may include."""
    module = tests_directory / "support" / f"{name}.rs"
    module.parent.mkdir(parents=True, exist_ok=True)
    module.write_text(_source_text(takes_lock), encoding="utf-8")


def _declaration(name: str) -> str:
    """Return the ``#[path]`` declaration that compiles a support module."""
    return f'#[path = "support/{name}.rs"]\nmod {name};\n'


def _write_binary(tests_directory: Path, binary: _Binary) -> None:
    """Write the crate root one binary describes."""
    declarations = "".join(
        _declaration(name) for name in sorted(binary.includes)
    )
    (tests_directory / f"{binary.name}.rs").write_text(
        declarations + _source_text(binary.takes_lock), encoding="utf-8"
    )


def _write_tree(tests_directory: Path, tree: _Tree) -> None:
    """Write every source a tree describes.

    The lock module always takes the lock, and the orphan takes it when the
    plan says so; no binary compiles the orphan.
    """
    _write_module(tests_directory, _LOCK_MODULE, True)
    for name, takes_lock in tree.modules:
        _write_module(tests_directory, name, takes_lock)
    _write_module(tests_directory, _ORPHAN, tree.orphan_takes_lock)
    for binary in tree.binaries:
        _write_binary(tests_directory, binary)


def _locking_modules(tree: _Tree) -> frozenset[str]:
    """Return the generated modules whose text takes the lock."""
    return frozenset(name for name, takes_lock in tree.modules if takes_lock)


def _expected_members(tree: _Tree) -> frozenset[str]:
    """Return the binaries whose own text, outside the lock module, locks.

    The lock module's own uses are covered by the group's embedded term, so
    a binary that only compiles it is not a member.
    """
    locking = _locking_modules(tree)
    return frozenset(
        binary.name
        for binary in tree.binaries
        if binary.takes_lock or binary.includes & locking
    )


def _expected_orphans(tree: _Tree) -> frozenset[str]:
    """Return the modules that take the lock and no binary compiles.

    A generated module may be included by no binary, which is an orphan for
    the same reason the designated one is; the expectation is computed from
    the plan rather than from the reader.
    """
    included = frozenset().union(
        *(binary.includes for binary in tree.binaries)
    )
    unused = {
        name
        for name, takes_lock in tree.modules
        if takes_lock and name not in included
    }
    if tree.orphan_takes_lock:
        unused.add(_ORPHAN)
    return frozenset(unused)


def _assert_faults(tree: _Tree, faults: list[str]) -> None:
    """Assert that the faults name exactly the orphaned lock uses."""
    expected = _expected_orphans(tree)
    if not expected:
        assert faults == [], (
            f"a generated tree without a locking orphan is fault-free: {faults}"
        )
        return
    assert len(faults) == 1, f"orphans are reported once: {faults}"
    assert faults[0].count("support/") == len(expected), (
        f"only the orphaned modules may be reported: {faults}"
    )
    for name in expected:
        assert f"support/{name}.rs" in faults[0], (
            f"the orphan {name} must be named: {faults}"
        )


@_GENERATED
@given(tree=_trees())
def test_discovery_matches_the_declared_attribution(tree: _Tree) -> None:
    """A binary is a member exactly when its compiled text takes the lock."""
    with tempfile.TemporaryDirectory() as directory:
        tests_directory = Path(directory) / "tests"
        tests_directory.mkdir()
        _write_tree(tests_directory, tree)
        root = tests_directory.resolve()
        discovered, faults = lock_consuming_binaries(
            load_test_sources(root), root=root
        )
    expected = _expected_members(tree)
    assert discovered == expected, (
        "discovery must match the declared attribution: "
        f"expected {sorted(expected)}, got {sorted(discovered)}"
    )
    _assert_faults(tree, faults)