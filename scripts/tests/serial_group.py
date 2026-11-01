"""Hold the nextest ``serial`` group to the binaries that take the lock.

``tests/support/serial.rs`` acquires a cross-process file lock
(``pg-embed-setup-unpriv.serial.lock``) around every scenario that touches
the shared PostgreSQL cache, the installation directories or process-wide
state. A binary that compiles that module contends for the lock even when
its own cases use only ``local_serial_guard``: the module's unit tests take
``serial_guard`` as well.

nextest starts a test only when a slot in its test group is free, so group
membership keeps the per-test slow-timeout clock from running while a test
waits for the lock (#254). This contract pins membership to the binaries
that compile the module and discovers new ones, so an omission fails
``make test-scripts`` rather than a cold full run.

The filter grammar is deliberately narrow: exact ``binary(=name)`` terms
joined by ``|``. A broader filter, a negation or a conjunction would make
the group mean something this contract cannot compare with the inventory.
The override must also apply on every platform: a ``platform`` restriction
would leave the binaries out of the group everywhere else. It must come
before any other override that assigns a test group, because nextest applies
the first matching override and a later serial assignment would never run.

The inventory itself comes from :mod:`serial_inventory`, which discovers
the binaries and reports a serial-module inclusion that cannot be
attributed to a root file.
"""

from __future__ import annotations

import re
import typing as typ
from pathlib import Path

from nextest_budgets import _parsed
from serial_inventory import lock_taking_binaries
from timeout_budgets import mapping_or_empty, sequence_or_empty

REPOSITORY_ROOT: typ.Final[Path] = Path(__file__).resolve().parents[2]
NEXTEST_CONFIG: typ.Final[Path] = REPOSITORY_ROOT / ".config" / "nextest.toml"
CARGO_MANIFEST: typ.Final[Path] = REPOSITORY_ROOT / "Cargo.toml"
TESTS_DIRECTORY: typ.Final[Path] = REPOSITORY_ROOT / "tests"

#: The group whose ``max-threads = 1`` queues the lock-taking binaries.
SERIAL_GROUP: typ.Final[str] = "serial"

#: Every integration-test binary that compiles ``tests/support/serial.rs``.
#:
#: Changing this set is a deliberate scheduling change. The pin exists so
#: that a change to the nextest filter's membership cannot pass unnoticed.
LOCK_TAKING_BINARIES: typ.Final[frozenset[str]] = frozenset(
    {
        "bootstrap_privileges",
        "cluster_split_constructors",
        "data_dir_recovery",
        "database_lifecycle",
        "observability",
        "password_reuse_e2e",
        "settings_logging",
        "shutdown_hook",
        "shutdown_hook_lifecycle",
        "test_cluster_async",
        "test_cluster_behaviour",
        "test_cluster_connection",
        "test_cluster_drop",
        "test_cluster_extensions",
        "test_cluster_fixture",
        "timezone_regression",
    }
)

#: One exact ``binary(=name)`` term, the only syntax the filter may use.
_BINARY_TERM: typ.Final[re.Pattern[str]] = re.compile(r"binary\(=([A-Za-z0-9_]+)\)")


def _serial_limit_faults(config: dict[str, object]) -> list[str]:
    """Report a ``test-groups.serial.max-threads`` other than one.

    Parameters
    ----------
    config : dict of str to object
        The parsed nextest configuration.

    Returns
    -------
    list of str
        One fault when the limit is absent or not exactly one, else empty.
    """
    groups = mapping_or_empty(config.get("test-groups"))
    serial = mapping_or_empty(groups.get(SERIAL_GROUP))
    limit = serial.get("max-threads")
    if type(limit) is int and limit == 1:
        return []
    return [
        f"test-groups.serial.max-threads must be 1, found {limit!r}"
    ]


def _default_overrides(config: dict[str, object]) -> list[dict[str, object]]:
    """Return the ``profile.default`` overrides in the order nextest applies.

    Parameters
    ----------
    config : dict of str to object
        The parsed nextest configuration.

    Returns
    -------
    list of dict
        Every override under ``profile.default``, in file order.
    """
    profile = mapping_or_empty(
        mapping_or_empty(config.get("profile")).get("default")
    )
    return [
        mapping_or_empty(override)
        for override in sequence_or_empty(profile.get("overrides"))
    ]


def _serial_overrides(config: dict[str, object]) -> list[dict[str, object]]:
    """Return the ``profile.default`` overrides that assign the serial group.

    Parameters
    ----------
    config : dict of str to object
        The parsed nextest configuration.

    Returns
    -------
    list of dict
        Every override whose ``test-group`` names ``serial``, in file order.
    """
    return [
        override
        for override in _default_overrides(config)
        if override.get("test-group") == SERIAL_GROUP
    ]


def _competing_group_faults(config: dict[str, object]) -> list[str]:
    """Report an earlier override that assigns a test group of its own.

    nextest applies the first matching override, so an override ahead of the
    serial one that assigns any ``test-group`` can capture a lock-taking
    binary and leave the serial override unused. Requiring the serial group
    to be the first group assignment is simpler to check than deciding
    whether an earlier filter matches a binary.

    Parameters
    ----------
    config : dict of str to object
        The parsed nextest configuration.

    Returns
    -------
    list of str
        One fault naming the earlier filters, or empty when no override
        before the serial one assigns a group.
    """
    overrides = _default_overrides(config)
    for index, override in enumerate(overrides):
        if override.get("test-group") != SERIAL_GROUP:
            continue
        earlier = [
            entry for entry in overrides[:index] if "test-group" in entry
        ]
        if not earlier:
            return []
        filters = ", ".join(repr(entry.get("filter")) for entry in earlier)
        return [
            "no override may assign a test group before the serial one, "
            f"found {len(earlier)}: {filters}"
        ]
    return []


def _one_serial_override(
    config: dict[str, object],
) -> tuple[dict[str, object] | None, list[str]]:
    """Return the single serial override, or a fault naming the count.

    Parameters
    ----------
    config : dict of str to object
        The parsed nextest configuration.

    Returns
    -------
    tuple
        The override when exactly one assigns the group, and the faults.
    """
    overrides = _serial_overrides(config)
    if len(overrides) == 1:
        return overrides[0], []
    return None, [
        "profile.default must have exactly one override assigning "
        f'test-group = "{SERIAL_GROUP}", found {len(overrides)}'
    ]


def _platform_faults(override: dict[str, object]) -> list[str]:
    """Report a serial override nextest applies on one platform only.

    A platform-restricted override would leave the lock-taking binaries out
    of the serial group everywhere else, which is the omission this contract
    exists to catch, so the override must apply on every platform.

    Parameters
    ----------
    override : dict of str to object
        The override assigning the serial group.

    Returns
    -------
    list of str
        One fault when the override names a platform, else empty.
    """
    platform = override.get("platform")
    if platform is None:
        return []
    return [
        "the serial override must not restrict itself to a platform, "
        f"found platform = {platform!r}"
    ]


def _filter_names(
    override: dict[str, object],
) -> tuple[frozenset[str], list[str]]:
    """Parse the serial filter and report any syntax outside the grammar.

    Parameters
    ----------
    override : dict of str to object
        The override assigning the serial group.

    Returns
    -------
    tuple
        The binary names named by the filter, and the grammar faults.
    """
    raw = override.get("filter")
    match raw:
        case str():
            terms = raw.split("|")
        case _:
            return frozenset(), ["the serial override names no filter"]
    names: set[str] = set()
    faults: list[str] = []
    for part in terms:
        term = part.strip()
        match = _BINARY_TERM.fullmatch(term)
        if match is None:
            faults.append(
                "the serial filter must be exact binary(=name) terms joined "
                f"by '|', found {term!r}"
            )
            continue
        names.add(match.group(1))
    return frozenset(names), faults


def _difference_faults(
    label: str, actual: frozenset[str], expected: frozenset[str]
) -> list[str]:
    """Report names missing from, or absent from, a pinned inventory.

    Parameters
    ----------
    label : str
        What the compared set describes, for the message.
    actual : frozenset of str
        The names read from the repository.
    expected : frozenset of str
        The pinned inventory.

    Returns
    -------
    list of str
        One fault per direction that differs.
    """
    faults: list[str] = []
    missing = sorted(expected - actual)
    extra = sorted(actual - expected)
    if missing:
        faults.append(f"{label} is missing {', '.join(missing)}")
    if extra:
        faults.append(f"{label} names unexpected binaries {', '.join(extra)}")
    return faults


def serial_group_faults(
    config_text: str, manifest_text: str, tests_directory: Path
) -> list[str]:
    """Return every way the serial group drifts from the lock-taking set.

    Parameters
    ----------
    config_text : str
        The nextest configuration file's text.
    manifest_text : str
        The ``Cargo.toml`` text.
    tests_directory : Path
        The ``tests`` directory to scan for root files.

    Returns
    -------
    list of str
        The faults, empty when the filter, its limit and the discovered
        binaries all agree with the pinned inventory.
    """
    config = _parsed(config_text)
    faults = _serial_limit_faults(config)
    faults.extend(_competing_group_faults(config))
    override, override_faults = _one_serial_override(config)
    faults.extend(override_faults)
    filter_names: frozenset[str] = frozenset()
    if override is not None:
        faults.extend(_platform_faults(override))
        filter_names, grammar_faults = _filter_names(override)
        faults.extend(grammar_faults)
    discovered, discovery_faults = lock_taking_binaries(
        manifest_text, tests_directory
    )
    faults.extend(discovery_faults)
    faults.extend(
        _difference_faults(
            "the serial filter", filter_names, LOCK_TAKING_BINARIES
        )
    )
    faults.extend(
        _difference_faults(
            "the discovered lock-taking set",
            discovered,
            LOCK_TAKING_BINARIES,
        )
    )
    return faults