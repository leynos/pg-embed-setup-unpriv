"""Discover the compiled Loom test binary and the models it reports.

The Loom tests are ordinary ignored library tests, so `--ignored` alone would
select every ignored test in the crate. The runner therefore builds the library
test binary once, reads the names it prints for `--list --ignored`, and checks
them against the inventory in `.config/loom-models.toml`, which
`loom_manifest` parses. This module owns the Cargo-message and listing parsing
that makes that possible.

Examples
--------
>>> parse_listing("env::loom_tests::one: test\\n1 test, 0 benchmarks\\n")
['env::loom_tests::one']
>>> parse_cargo_messages('{"reason": "build-finished"}\\n')
[{'reason': 'build-finished'}]
"""

from __future__ import annotations

import json
import typing as typ
from pathlib import Path

from loom_manifest import (
    PROFILES,
    DiscoveryError,
    Manifest,
    Model,
    is_loom_path,
    load_manifest,
    parse_manifest,
    validate_inventory,
)

#: The libtest target the runner builds and executes.
TEST_TARGET: typ.Final = "pg_embedded_setup_unpriv"

#: The feature that compiles the Loom models.
LOOM_FEATURE: typ.Final = "loom-tests"


def parse_listing(text: str) -> list[str]:
    """Return the test names in `--list --ignored` output.

    libtest prints one `name: test` line per test and a trailing summary. A
    benchmark line ends in `: bench` and is not a model.

    Examples
    --------
    >>> parse_listing("a::loom_tests::one: test\\na::loom_tests::two: test\\n")
    ['a::loom_tests::one', 'a::loom_tests::two']
    >>> parse_listing("a::bench: bench\\n0 tests, 1 benchmark\\n")
    []
    """
    suffix = ": test"
    return [
        line.strip()[: -len(suffix)]
        for line in text.splitlines()
        if line.strip().endswith(suffix)
    ]


def parse_cargo_messages(text: str) -> list[dict[str, typ.Any]]:
    """Return the JSON messages in Cargo's `--message-format=json` output.

    Cargo writes one JSON object per line to stdout and human-readable
    progress to stderr. A line that is not a JSON object is skipped.

    Examples
    --------
    >>> parse_cargo_messages('{"reason": "build-finished"}\\nnot json\\n')
    [{'reason': 'build-finished'}]
    """
    messages: list[dict[str, typ.Any]] = []
    for line in text.splitlines():
        stripped = line.strip()
        if not stripped.startswith("{"):
            continue
        try:
            message = json.loads(stripped)
        except json.JSONDecodeError:
            continue
        if isinstance(message, dict):
            messages.append(message)
    return messages


def select_test_executable(
    messages: typ.Iterable[dict[str, typ.Any]], target: str = TEST_TARGET
) -> Path:
    """Return the one executable lib test artefact for `target`.

    Parameters
    ----------
    messages : Iterable[dict[str, Any]]
        Cargo's JSON messages.
    target : str
        The lib target name.

    Returns
    -------
    Path
        The executable test binary.

    Raises
    ------
    DiscoveryError
        If no artefact, more than one artefact, or an artefact built without
        the `loom-tests` feature is found.

    Examples
    --------
    >>> message = {
    ...     "reason": "compiler-artifact",
    ...     "target": {"name": "pg_embedded_setup_unpriv", "kind": ["lib"], "test": True},
    ...     "features": ["loom-tests"],
    ...     "executable": "/tmp/loom-tests",
    ... }
    >>> select_test_executable([message])
    PosixPath('/tmp/loom-tests')
    """
    candidates: list[tuple[Path, list[str]]] = []
    for message in messages:
        if message.get("reason") != "compiler-artifact":
            continue
        artefact = message.get("target")
        if not isinstance(artefact, dict):
            continue
        if artefact.get("name") != target:
            continue
        if not artefact.get("test") or "lib" not in (artefact.get("kind") or []):
            continue
        executable = message.get("executable")
        if not executable:
            continue
        features = message.get("features")
        candidates.append(
            (Path(executable), features if isinstance(features, list) else [])
        )
    if not candidates:
        raise DiscoveryError(
            f"no executable lib test artefact for {target}; "
            "was the loom-tests feature enabled?"
        )
    if len(candidates) > 1:
        raise DiscoveryError(
            f"expected one executable lib test artefact for {target}, "
            f"found {len(candidates)}"
        )
    executable, features = candidates[0]
    if LOOM_FEATURE not in features:
        raise DiscoveryError(
            f"the {target} test artefact was built without the {LOOM_FEATURE} feature"
        )
    return executable
