"""Reading the test sources that start child processes.

``test_cluster_tier_coverage_contract.py`` needs the set of integration
binaries that spawn children and reference a cluster-bootstrap API. The
answer has to come from the sources rather than from another list,
because a value pinned in a manually maintained list cannot notice a
binary nobody listed.

This module owns both halves of that reading. ``load_test_sources`` does
every filesystem call and reports a failure as ``SourceLoadError``;
``spawning_cluster_binaries`` is then a pure classification of the text
it is given; ``scan_spawning_cluster_binaries`` is the entry point that
does both. Keeping the split explicit means the scanner can be tested
against a constructed tree without touching the disk.

The scan is lexical, so a reference in a comment counts. A false
positive is a prompt to review the binary rather than a defect in the
reader.

See "Test timeouts: four tiers, outermost last" in
``docs/developers-guide.md``.
"""

import collections.abc as cabc
import os
import re
import typing as typ
from pathlib import Path

TESTS_DIR: typ.Final[Path] = Path(__file__).resolve().parents[2] / "tests"

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


class SourceLoadError(RuntimeError):
    """Raised when a test source cannot be read from disk.

    Reading is the loader's job; classifying text is the scanner's. The
    loader reports a filesystem or decoding failure as this one domain
    error rather than letting ``OSError`` or ``UnicodeError`` escape from
    a query that promises to return a set of names.
    """

    def __init__(self, path: Path, cause: OSError | UnicodeError) -> None:
        """Report the failing path and the underlying error."""
        super().__init__(f"cannot read test source {path}: {cause}")
        self.path = path
        self.cause = cause


def load_test_sources(tests_dir: Path = TESTS_DIR) -> dict[Path, str]:
    """Read every Rust source under the test tree, keyed by resolved path.

    Loading the whole tree once lets module resolution be a mapping
    lookup rather than a sequence of ``is_file`` probes, and keeps every
    filesystem call in one place. The text is decoded as UTF-8 because
    Rust sources are, and a decode failure is as much a reason to stop as
    a read failure.

    Parameters
    ----------
    tests_dir : Path
        The directory holding the integration-test sources.

    Returns
    -------
    dict[Path, str]
        Each source file's resolved path mapped to its UTF-8 text.

    Raises
    ------
    SourceLoadError
        If the tree cannot be listed, or a file cannot be read or
        decoded as UTF-8.
    """
    sources: dict[Path, str] = {}
    try:
        paths = sorted(tests_dir.rglob("*.rs"))
    except OSError as exc:
        raise SourceLoadError(tests_dir, exc) from exc
    for path in paths:
        try:
            text = path.read_text(encoding="utf-8")
        except (OSError, UnicodeError) as exc:
            raise SourceLoadError(path, exc) from exc
        sources[path.resolve()] = text
    return sources


def _source_files(
    sources: cabc.Mapping[Path, str], root: Path
) -> typ.Iterator[tuple[str, Path]]:
    """Yield each test binary's name and its crate-root source path.

    Parameters
    ----------
    sources : Mapping[Path, str]
        The loaded test sources, keyed by resolved path.
    root : Path
        The resolved directory holding the integration-test sources.

    Returns
    -------
    Iterator[tuple[str, Path]]
        Top-level ``tests/*.rs`` binaries, plus the directory-backed
        ``test_cluster_fixture`` target when it is present.
    """
    for path in sorted(sources):
        if path.parent == root:
            yield path.stem, path
    fixture = root / "test_cluster_fixture" / "mod.rs"
    if fixture in sources:
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


def _lexically_normalized(path: Path) -> Path:
    """Collapse ``.`` and ``..`` components without touching the filesystem.

    The loader keys sources by resolved path. A ``#[path]`` override is
    written relative to the declaring file, and a directory-backed target
    such as ``test_cluster_fixture`` reaches ``tests/support`` as
    ``../support/name.rs``, so the joined candidate carries ``..`` and
    never compares equal to a resolved key. Normalising here keeps the
    classifier pure: it matches path strings, it does not probe the disk.
    """
    return Path(os.path.normpath(path))


def _module_file(
    declaring_file: Path,
    declared: re.Match[str],
    *,
    is_crate_root: bool,
    sources: cabc.Mapping[Path, str],
) -> Path | None:
    """Return the loaded file a module declaration compiles in, if any.

    A ``#[path]`` attribute overrides the search and is taken relative
    to the declaring file, which is how the test crates reach
    ``tests/support``. A plain declaration searches the sibling file
    and then the directory form. Declarations nested inside inline
    ``mod`` blocks are not modelled; no test crate uses that shape.
    """
    override = declared.group("path")
    name = declared.group("name")
    if override is not None:
        candidates = [_lexically_normalized(declaring_file.parent / override)]
    else:
        base = _module_directory(declaring_file, is_crate_root=is_crate_root)
        candidates = [base / f"{name}.rs", base / name / "mod.rs"]
    return next((path for path in candidates if path in sources), None)


def _compiled_sources(
    path: Path,
    seen: dict[Path, str],
    *,
    is_crate_root: bool,
    sources: cabc.Mapping[Path, str],
) -> dict[Path, str]:
    """Return every source file a test binary compiles in, by path.

    Parameters
    ----------
    path : Path
        A crate root or an included module file, which the loader has
        already read.
    seen : dict[Path, str]
        The files already visited, mapping each to its text, which also
        stops a module cycle from recursing.
    is_crate_root : bool
        Whether ``path`` is the binary's root, which changes where its
        plain module declarations resolve.
    sources : Mapping[Path, str]
        The loaded test sources, keyed by resolved path.

    Returns
    -------
    dict[Path, str]
        The text of ``path`` and, transitively, of every module it
        declares.
    """
    if path in seen:
        return seen
    source = sources[path]
    seen[path] = source
    for declared in _MODULE_DECLARATION.finditer(source):
        module = _module_file(
            path, declared, is_crate_root=is_crate_root, sources=sources
        )
        if module is not None:
            _compiled_sources(
                module, seen, is_crate_root=False, sources=sources
            )
    return seen


def spawning_cluster_binaries(
    sources: cabc.Mapping[Path, str], *, root: Path
) -> frozenset[str]:
    """Return binaries that spawn children and reference a cluster API.

    A pure classification of text the loader has already read. The
    markers are searched across every file the binary compiles in, not
    only its crate root: a parent can put the child-process spawn in one
    module and the cluster bootstrap in another.

    Parameters
    ----------
    sources : Mapping[Path, str]
        The loaded test sources, keyed by resolved path.
    root : Path
        The resolved directory holding the integration-test sources.

    Returns
    -------
    frozenset[str]
        Test binary names matching both lexical markers.
    """
    found = set()
    for name, path in _source_files(sources, root):
        compiled = _compiled_sources(
            path, {}, is_crate_root=True, sources=sources
        )
        text = "\n".join(compiled[source] for source in sorted(compiled))
        if _CHILD_PROCESS.search(text) and _CLUSTER_API.search(text):
            found.add(name)
    return frozenset(found)


def scan_spawning_cluster_binaries(
    tests_dir: Path = TESTS_DIR,
) -> frozenset[str]:
    """Load the test tree and return the child-spawning cluster binaries.

    This is the entry point that performs both steps: it resolves the
    root, loads every source, and hands the result to the pure
    classifier.

    Parameters
    ----------
    tests_dir : Path
        The directory holding the integration-test sources.

    Returns
    -------
    frozenset[str]
        Test binary names matching both lexical markers.

    Raises
    ------
    SourceLoadError
        If the tree cannot be read.
    """
    root = tests_dir.resolve()
    return spawning_cluster_binaries(load_test_sources(root), root=root)
