"""Properties the child-spawn scanner holds for trees no file writes down.

``test_cluster_tier_coverage_contract.py`` pins the module shapes this
repository actually uses. Those fixtures cannot notice a scanner that is
wrong about a shape nobody has written yet, so this file generates
bounded Rust module trees and compares the scan with the answer the
generator already knows. The generated layouts include a flat crate
root, a directory-form module, a ``#[path]`` override into ``support``,
a nested module, a plain sibling declared from a crate root, a
declaration whose file is missing, and a module cycle. They also include
the directory-backed ``test_cluster_fixture`` target reaching ``support``
as ``../support/...``.

Two traps keep the property honest. A ``support`` file carries both
markers but is compiled by no binary, so a scanner that matched any file
in the tree would report the wrong answer. Binaries that receive only one
of the two markers must stay out of the result, so a scanner that matched
either marker alone would fail here as well. The sibling layout adds a
third: the module file a crate root declares at the tree root is itself a
target, so the same text is compiled by two different binaries.
"""

import dataclasses
import typing as typ
from pathlib import Path

from cluster_sources import spawning_cluster_binaries
from hypothesis import given
from hypothesis import strategies as st

#: A name in the charset Cargo permits in a test target name.
_BINARY_NAME: typ.Final[st.SearchStrategy[str]] = st.from_regex(
    r"[a-z][a-z0-9_]{2,12}", fullmatch=True
)

#: The one target whose crate root lives in a directory of its own.
_FIXTURE: typ.Final[str] = "test_cluster_fixture"

#: A line the child-process marker matches.
_CHILD_MARKER: typ.Final[str] = 'fn spawn() { Command::new("child"); }\n'

#: A line the cluster-bootstrap marker matches.
_CLUSTER_MARKER: typ.Final[str] = "fn boot() -> TestCluster { todo!() }\n"

#: A line neither marker matches.
_UNMARKED: typ.Final[str] = "// nothing to see here\n"

#: The module shapes a binary can be built from.
_LAYOUTS: typ.Final[tuple[str, ...]] = (
    "flat",
    "support_override",
    "directory_module",
    "nested",
    "crate_root_sibling",
    "missing_candidate",
    "cycle",
)


def _units(
    root: Path, name: str, layout: str, *, is_fixture: bool
) -> list[tuple[Path, str]]:
    """Return each compiled file of a binary, with its module declarations.

    Parameters
    ----------
    root : Path
        The synthetic ``tests`` directory the tree is rooted at.
    name : str
        The target's name, which namespaces its module paths so two
        binaries in one tree never share a file.
    layout : str
        One of :data:`_LAYOUTS`.
    is_fixture : bool
        Whether the crate root is ``test_cluster_fixture/mod.rs`` rather
        than a file at the tree root, which changes how a relative
        ``#[path]`` and a plain ``mod`` are resolved.

    Returns
    -------
    list[tuple[Path, str]]
        The crate root first, then its modules, each with the
        declarations its text must carry. A sibling declared from a
        crate root that sits at the tree root also appears here as a
        root-level file, which the scanner must treat as a binary of its
        own; the capitalised module name keeps it clear of the generated
        lowercase target names.
    """
    crate_root = root / _FIXTURE / "mod.rs" if is_fixture else root / f"{name}.rs"
    if layout == "flat":
        return [(crate_root, "")]
    if layout == "support_override":
        prefix = "../support" if is_fixture else "support"
        helper = root / "support" / f"{name}_helper.rs"
        declaration = f'#[path = "{prefix}/{name}_helper.rs"]\nmod helper;\n'
        return [(crate_root, declaration), (helper, "")]
    if layout == "directory_module":
        # A plain `mod` searches the declaring file's own directory, so a
        # `mod.rs` crate root looks inside the fixture directory rather
        # than beside it.
        helper = crate_root.parent / f"{name}_helper" / "mod.rs"
        return [(crate_root, f"mod {name}_helper;\n"), (helper, "")]
    if layout == "crate_root_sibling":
        sibling = crate_root.parent / f"Child_{name}.rs"
        return [(crate_root, f"mod Child_{name};\n"), (sibling, "")]
    if layout == "missing_candidate":
        declaration = f'#[path = "support/{name}_absent.rs"]\nmod absent;\n'
        return [(crate_root, declaration)]
    if layout == "cycle":
        module_dir = crate_root.parent / name
        first, second = module_dir / "a.rs", module_dir / "b.rs"
        return [
            (crate_root, f'#[path = "{name}/a.rs"]\nmod a;\n'),
            (first, '#[path = "b.rs"]\nmod b;\n'),
            (second, '#[path = "a.rs"]\nmod a;\n'),
        ]
    # `#[path]` is relative to the declaring file, so the fixture's
    # private module directory sits inside the fixture directory too.
    helper = crate_root.parent / name / "helper.rs"
    inner = helper.parent / "helper" / "inner.rs"
    return [
        (crate_root, f'#[path = "{name}/helper.rs"]\nmod helper;\n'),
        (helper, "mod inner;\n"),
        (inner, ""),
    ]


@dataclasses.dataclass(frozen=True)
class _Placement:
    """Which compiled file carries each marker, when it is present."""

    child: int | None
    cluster: int | None


def _placed_texts(
    units: list[tuple[Path, str]], placement: _Placement
) -> dict[Path, str]:
    """Write each unit's declaration and the markers it was chosen to hold.

    Returns
    -------
    dict[Path, str]
        Every compiled file's final text, keyed by path.
    """
    texts: dict[Path, str] = {}
    for index, (path, declaration) in enumerate(units):
        markers = declaration
        if index == placement.child:
            markers += _CHILD_MARKER
        if index == placement.cluster:
            markers += _CLUSTER_MARKER
        texts[path] = markers or _UNMARKED
    return texts


def _extra_root_targets(
    root: Path, crate_root: Path, texts: dict[Path, str]
) -> frozenset[str]:
    """Return the targets a module file at the tree root creates for itself.

    Cargo compiles every ``tests/*.rs`` as a target, so a file a crate
    root declares at the tree root is also a binary, with only its own
    text compiled in.

    Returns
    -------
    frozenset[str]
        Names of those root-level files that carry both markers.
    """
    return frozenset(
        path.stem
        for path, text in texts.items()
        if path.parent == root
        and path != crate_root
        and _CHILD_MARKER in text
        and _CLUSTER_MARKER in text
    )


@st.composite
def _binary(
    draw: st.DrawFn, root: Path, name: str
) -> tuple[dict[Path, str], frozenset[str]]:
    """Generate one binary's files and the targets they prove.

    Returns
    -------
    tuple[dict[Path, str], frozenset[str]]
        The binary's compiled files with their final text, and every
        target name the generated text must make the scanner report.
    """
    layout = draw(st.sampled_from(_LAYOUTS))
    units = _units(root, name, layout, is_fixture=name == _FIXTURE)
    units_range = st.integers(min_value=0, max_value=len(units) - 1)
    placement = _Placement(
        child=draw(units_range) if draw(st.booleans()) else None,
        cluster=draw(units_range) if draw(st.booleans()) else None,
    )
    texts = _placed_texts(units, placement)
    is_both = placement.child is not None and placement.cluster is not None
    targets = {name} if is_both else set()
    targets |= _extra_root_targets(root, units[0][0], texts)
    return texts, frozenset(targets)


@st.composite
def _module_tree(
    draw: st.DrawFn,
) -> tuple[dict[Path, str], Path, frozenset[str]]:
    """Generate a bounded test tree and the answer its scanner must give.

    Returns
    -------
    tuple[dict[Path, str], Path, frozenset[str]]
        The loaded sources keyed by path, the tree root, and the target
        names that compile in both markers.
    """
    root = Path("/generated/tests")
    sources: dict[Path, str] = {
        root / "support" / "orphan.rs": _CHILD_MARKER + _CLUSTER_MARKER
    }
    names = draw(st.lists(_BINARY_NAME, min_size=1, max_size=3, unique=True))
    if draw(st.booleans()):
        names = [*names, _FIXTURE]

    expected: set[str] = set()
    for name in names:
        texts, targets = draw(_binary(root, name))
        sources.update(texts)
        expected.update(targets)
    return sources, root, frozenset(expected)


@given(tree=_module_tree())
def test_the_scan_reports_exactly_the_binaries_compiling_both_markers(
    tree: tuple[dict[Path, str], Path, frozenset[str]],
) -> None:
    """The generated answer, not the scanner, decides the expected set.

    Every generated tree varies the layout, which file carries each
    marker, and whether a marker is missing at all, so the cases the
    repository's own fixtures cannot reach are covered here.
    """
    sources, root, expected = tree
    assert spawning_cluster_binaries(sources, root=root) == expected
