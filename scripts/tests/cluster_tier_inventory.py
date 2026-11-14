"""Classify the test binaries that spawn a real PostgreSQL cluster.

The cluster timeout tier exists because those binaries pay a cold cluster
bootstrap inside a test, whether or not they take the shared scenario lock.
The classification reads the sources rather than a list, because a value
pinned in a manually maintained list cannot notice a binary nobody listed.

``spawning_cluster_binaries`` is a pure classification of text the loader has
already read; ``scan_spawning_cluster_binaries`` resolves the tree, loads it
and hands the result to that classifier. Both live here rather than in
:mod:`cluster_sources` so each module reads as one unit: that module answers
which files a binary compiles, and this one asks what those files mean.

The classification is lexical, so a reference in a comment counts. A false
positive is a prompt to review the binary rather than a defect in the
reader.

See "Test timeouts: four tiers, outermost last" in
``docs/developers-guide.md``.
"""

import collections.abc as cabc
import re
from pathlib import Path

from cluster_sources import TESTS_DIR, binary_sources, load_test_sources

#: A reference to an API that bootstraps a real PostgreSQL cluster.
_CLUSTER_API = re.compile(
    r"shared_cluster_handle|bootstrap_for_tests|TestCluster|test_cluster::"
)

#: A reference to spawning another process from a test.
_CHILD_PROCESS = re.compile(r"current_exe\(|spawn_child\(|Command::new\(")


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