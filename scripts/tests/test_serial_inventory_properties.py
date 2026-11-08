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

#: Each example writes a tree and scans it, which the default 200 ms deadline
#: does not allow for on a loaded runner. A slow example is not a defect.
_GENERATED = settings(max_examples=25, deadline=None)

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


def _source_text(takes_lock: bool) -> str:
    """Return the body of a source that does or does not take the lock."""
    return _LOCKING if takes_lock else _ISOLATED


def _write_support(tests_directory: Path, name: str, takes_lock: bool) -> str:
    """Write one binary's support module and return its declaration.

    Parameters
    ----------
    tests_directory : Path
        The directory to write the sources into.
    name : str
        The binary the support module belongs to.
    takes_lock : bool
        Whether the support module takes the lock.

    Returns
    -------
    str
        The lines that include the module, as ``#[path]`` is what makes the
        attribution interesting.
    """
    support = tests_directory / "support" / f"{name}_support.rs"
    support.parent.mkdir(parents=True, exist_ok=True)
    support.write_text(_source_text(takes_lock), encoding="utf-8")
    return f'#[path = "support/{name}_support.rs"]\nmod {name}_support;\n'


def _write_binary(tests_directory: Path, plan: _Plan) -> None:
    """Write the crate root one plan describes.

    Parameters
    ----------
    tests_directory : Path
        The directory to write the sources into.
    plan : tuple
        The plan for this binary.
    """
    name, takes_lock, includes_support, support_takes_lock = plan
    declaration = (
        _write_support(tests_directory, name, support_takes_lock)
        if includes_support
        else ""
    )
    (tests_directory / f"{name}.rs").write_text(
        declaration + _source_text(takes_lock), encoding="utf-8"
    )


def _takes_the_lock(plan: _Plan) -> bool:
    """Return whether a plan makes its binary a lock consumer."""
    _, takes_lock, includes_support, support_takes_lock = plan
    return takes_lock or (includes_support and support_takes_lock)


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
    for plan in plans:
        _write_binary(tests_directory, plan)
    return frozenset(plan[0] for plan in plans if _takes_the_lock(plan))


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