"""Reading the test sources and the modules each binary compiles in.

Two coverage contracts need an answer from the sources rather than from
another list, because a value pinned in a manually maintained list cannot
notice a binary nobody listed. ``test_cluster_tier_coverage_contract.py``
needs the binaries that spawn children and reference a cluster-bootstrap
API, and ``test_serial_group_contract.py`` needs the binaries that take
the cross-process scenario lock.

This module owns the reading both contracts share. ``load_test_sources``
does every filesystem call and reports a failure as ``SourceLoadError``;
``binary_sources`` resolves each binary's crate root and the modules it
compiles in; ``module_sources`` resolves one module file's own namespace;
``spawning_cluster_binaries`` is a pure classification of the text that
resolution yields. Keeping the split explicit means a scanner
can be tested against a constructed tree without touching the disk.

The classification is lexical, so a reference in a comment counts. A
false positive is a prompt to review the binary rather than a defect in
the reader.

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
    loader reports a filesystem, resolution or decoding failure as this
    one domain error rather than letting ``OSError``, ``RuntimeError`` or
    ``UnicodeError`` escape from a query that promises to return a set of
    names.
    """

    def __init__(
        self, path: Path, cause: OSError | UnicodeError | RuntimeError
    ) -> None:
        """Report the failing path and the underlying error."""
        super().__init__(f"cannot read test source {path}: {cause}")
        self.path = path
        self.cause = cause


def _rust_sources(tests_dir: Path) -> list[Path]:
    """List every Rust source under ``tests_dir``, reporting a scan failure.

    ``Path.rglob`` suppresses ``OSError`` raised while enumerating on
    Python 3.13, so an unreadable directory would contribute no sources
    without any failure being visible. Walking with :func:`os.walk` and
    its ``onerror`` callback keeps that failure: a binary that takes the
    lock inside such a directory would otherwise be omitted from every
    coverage contract silently.
    """
    faults: list[OSError] = []

    def report(error: OSError) -> None:
        """Record a directory the walk could not list."""
        faults.append(error)

    paths: list[Path] = []
    for root, _directories, names in os.walk(tests_dir, onerror=report):
        paths.extend(
            Path(root) / name for name in sorted(names) if name.endswith(".rs")
        )
    if faults:
        error = faults[0]
        # The walk reports the directory it could not list, which is the
        # path a reader needs; the root is only a fallback.
        failed = Path(error.filename) if error.filename else tests_dir
        raise SourceLoadError(failed, error)
    return sorted(paths)


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
        If the tree cannot be listed, a file cannot be read or decoded as
        UTF-8, or a source path cannot be resolved.
    """
    sources: dict[Path, str] = {}
    for path in _rust_sources(tests_dir):
        try:
            text = path.read_text(encoding="utf-8")
            # Resolution belongs inside the same guard: a symlink loop or a
            # vanished path raises here, after the directory walk succeeded,
            # and a caller promised a fault must not receive a raw error.
            resolved = path.resolve()
        except (OSError, UnicodeError, RuntimeError) as exc:
            raise SourceLoadError(path, exc) from exc
        sources[resolved] = text
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
        Top-level ``tests/*.rs`` binaries, the ``tests/<name>/main.rs``
        directories Cargo finds on its own, and the directory-backed
        ``test_cluster_fixture`` target the manifest declares with a
        ``mod.rs`` root.
    """
    for path in sorted(sources):
        if path.parent == root:
            yield path.stem, path
        elif path.parent.parent == root and path.name == "main.rs":
            yield path.parent.name, path
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

    The collapse is lexical, so it must not use ``Path.resolve``: that
    would consult the filesystem and follow symlinks. A leading ``..``
    survives on a relative path, and the anchor is never popped, so
    ``/..`` stays ``/``.
    """
    parts: list[str] = []
    for part in path.parts:
        if part == "..":
            if parts and parts[-1] not in {"..", path.anchor}:
                parts.pop()
            elif not path.is_absolute():
                parts.append(part)
        elif part != ".":
            parts.append(part)
    return Path(*parts)


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


def binary_sources(
    sources: cabc.Mapping[Path, str], *, root: Path
) -> dict[str, frozenset[Path]]:
    """Return each test binary's name mapped to the files it compiles in.

    A binary's own crate root and every module it declares, transitively,
    form one unit of classification. A source outside that relation is
    compiled by no binary, which is how a coverage contract notices a
    declaration it cannot attribute.

    Parameters
    ----------
    sources : Mapping[Path, str]
        The loaded test sources, keyed by resolved path.
    root : Path
        The resolved directory holding the integration-test sources.

    Returns
    -------
    dict[str, frozenset[Path]]
        One entry per binary, in a stable order, each holding the
        resolved paths of its crate root and included modules.
    """
    return {
        name: frozenset(
            _compiled_sources(path, {}, is_crate_root=True, sources=sources)
        )
        for name, path in sorted(_source_files(sources, root))
    }


def module_sources(
    sources: cabc.Mapping[Path, str], *, path: Path
) -> frozenset[Path]:
    """Return the files a module file compiles in, including itself.

    A module file resolves a plain ``mod`` declaration in a directory
    named after itself, which is what separates ``support/serial.rs``
    from a crate root: its children are the files Cargo names under the
    ``serial`` module rather than under the binary. Reachability is the
    question here, because the files a module compiles are the namespace
    its tests are named under.

    Parameters
    ----------
    sources : Mapping[Path, str]
        The loaded test sources, keyed by resolved path.
    path : Path
        A module file the loader has already read, such as
        ``tests/support/serial.rs``.

    Returns
    -------
    frozenset[Path]
        ``path`` and, transitively, every file it declares. A ``path``
        the loader did not read compiles nothing, so the result is empty
        rather than a ``KeyError``: the caller is asking which namespace
        exists, and a missing module has none.

    Examples
    --------
    >>> sources = {
    ...     Path("support/serial.rs"): "mod cases;\\n",
    ...     Path("support/serial/cases.rs"): "",
    ... }
    >>> sorted(p.as_posix() for p in module_sources(
    ...     sources, path=Path("support/serial.rs")
    ... ))
    ['support/serial.rs', 'support/serial/cases.rs']
    """
    if path not in sources:
        return frozenset()
    return frozenset(
        _compiled_sources(path, {}, is_crate_root=False, sources=sources)
    )


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
    for name, compiled in binary_sources(sources, root=root).items():
        text = "\n".join(sources[source] for source in sorted(compiled))
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
