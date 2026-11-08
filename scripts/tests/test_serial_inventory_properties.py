"""Property test: discovery follows the module a binary actually compiles.

The contract's examples pin the shapes that were once wrong. These generate
trees instead, because a rule about attribution can hold for the three shapes
someone thought of and fail for the fourth: a lock use reached through a
``#[path]`` module, a module that only names the in-process guard, and a
binary that takes the lock itself all have to keep their own answer.

See "Test timeouts: four tiers, outermost last" in
``docs/developers-guide.md``.
"""

from __future__ import annotations

import tempfile
from pathlib import Path

from hypothesis import given, settings
from hypothesis import strategies as st

from cluster_sources import load_test_sources
from serial_inventory import lock_consuming_binaries

#: Text that takes the cross-process lock.
_LOCKING = "fn f(_g: serial::ScenarioSerialGuard) {}\n"

#: Text that takes only the in-process guard.
_ISOLATED = "fn f(_g: serial::ScenarioLocalGuard) {}\n"

#: One binary: its name, its own lock use, whether it includes a support
#: module, and whether that module takes the lock.
_Plan = tuple[str, bool, bool, bool]

_GENERATED = settings(max_examples=25)

_NAMES = st.lists(
    st.from_regex(r"[a-z][a-z0-9_]{0,8}", fullmatch=True),
    min_size=1,
    max_size=5,
    unique=True,
)


@st.composite
def _plans(draw: st.DrawFn) -> list[_Plan]:
    """Generate the plans for one tree.

    Parameters
    ----------
    draw : st.DrawFn
        The Hypothesis draw function.

    Returns
    -------
    list of tuple
        One plan per binary, with unique names.
    """
    return [
        (
            name,
            draw(st.booleans()),
            draw(st.booleans()),
            draw(st.booleans()),
        )
        for name in draw(_NAMES)
    ]


def _write_tree(tests_directory: Path, plans: list[_Plan]) -> frozenset[str]:
    """Write the planned sources and return the expected members.

    The expectation is computed from the plan rather than from the reader,
    so a wrong reader cannot agree with it by repeating the same mistake.

    Parameters
    ----------
    tests_directory : Path
        The directory to write the sources into.
    plans : list of tuple
        The plans to write.

    Returns
    -------
    frozenset of str
        The binaries that take the lock under the plan.
    """
    expected: set[str] = set()
    for name, takes_lock, includes_support, support_takes_lock in plans:
        declaration = ""
        if includes_support:
            support = tests_directory / "support" / f"{name}_support.rs"
            support.parent.mkdir(parents=True, exist_ok=True)
            support.write_text(
                _LOCKING if support_takes_lock else _ISOLATED,
                encoding="utf-8",
            )
            declaration = (
                f'#[path = "support/{name}_support.rs"]\n'
                f"mod {name}_support;\n"
            )
        (tests_directory / f"{name}.rs").write_text(
            declaration + (_LOCKING if takes_lock else _ISOLATED),
            encoding="utf-8",
        )
        if takes_lock or (includes_support and support_takes_lock):
            expected.add(name)
    return frozenset(expected)


@_GENERATED
@given(plans=_plans())
def test_discovery_matches_the_declared_attribution(plans: list[_Plan]) -> None:
    """A binary is a member exactly when its compiled text takes the lock."""
    with tempfile.TemporaryDirectory() as directory:
        tests_directory = Path(directory) / "tests"
        tests_directory.mkdir()
        expected = _write_tree(tests_directory, plans)
        root = tests_directory.resolve()
        discovered, faults = lock_consuming_binaries(
            load_test_sources(root), root=root
        )
    assert faults == [], f"a generated tree must be fault-free, got {faults}"
    assert discovered == expected, (
        "discovery must match the declared attribution: "
        f"expected {sorted(expected)}, got {sorted(discovered)}"
    )