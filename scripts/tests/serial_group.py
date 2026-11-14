"""Hold the nextest ``serial`` group to the binaries that take the lock.

``tests/support/serial.rs`` takes a cross-process file lock
(``pg-embed-setup-unpriv.serial.lock``) around every scenario that touches
the shared PostgreSQL cache, the installation directories or process-wide
state. nextest starts a test only when a slot in its test group is free, so
group membership keeps the per-test slow-timeout clock from running while a
test waits for that lock (#254). Time spent blocked in ``flock`` after a
test starts is still counted, which is why a lock-taking binary must be in
the group rather than merely given a longer allowance.

Membership is checked in both directions. The filter must name the pinned
inventory, and the sources must still contain exactly those binaries, so a
binary that starts taking the lock fails here instead of timing out on a
cold full run. The inventory itself comes from :mod:`serial_inventory`.

Two shapes are rejected because nextest reads them in a way that would
silently drop a binary from the group. A ``platform`` restriction applies
the override on one platform only. An override *ahead* of the serial one
that assigns any ``test-group`` wins for that setting, because nextest
resolves each setting against the first matching override that configures
it, so the serial assignment would never run for a matching binary.
"""

from __future__ import annotations

import typing as typ
from pathlib import Path

from cluster_sources import TESTS_DIR
from nextest_budgets import _parsed
from nextest_filters import serial_group_binaries
from serial_inventory import scan_lock_consuming_binaries
from timeout_budgets import (
    NextestConfigurationError,
    mapping_or_empty,
    sequence_or_empty,
)

#: The group whose ``max-threads = 1`` queues the binaries that take the lock.
SERIAL_GROUP: typ.Final[str] = "serial"


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


def _one_serial_override(
    config: dict[str, object],
) -> tuple[dict[str, object] | None, list[str]]:
    """Return the single override that assigns the serial group.

    Parameters
    ----------
    config : dict of str to object
        The parsed nextest configuration.

    Returns
    -------
    tuple
        The override when exactly one assigns the group, and the faults.
    """
    overrides = [
        override
        for override in _default_overrides(config)
        if override.get("test-group") == SERIAL_GROUP
    ]
    if len(overrides) == 1:
        return overrides[0], []
    return None, [
        (
            "profile.default must have exactly one override assigning "
            f'test-group = "{SERIAL_GROUP}", found {len(overrides)}'
        )
    ]


def _competing_group_faults(config: dict[str, object]) -> list[str]:
    """Report an override ahead of the serial one that assigns a group.

    nextest resolves each setting against the first matching override that
    configures it, so an override ahead of the serial one that assigns any
    ``test-group`` captures a binary it matches and leaves the serial
    assignment unused for that binary. Requiring the serial group to be the
    first group assignment avoids deciding whether an earlier filter matches
    a lock-taking binary, which this contract does not model.

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
            (
                "no override may assign a test group before the serial one, "
                f"found {len(earlier)}: {filters}"
            )
        ]
    return []


def _platform_faults(override: dict[str, object]) -> list[str]:
    """Report a serial override nextest applies on one platform only.

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
        (
            "the serial override must not restrict itself to a platform, "
            f"found platform = {platform!r}"
        )
    ]


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
        faults.append(f"{label} must name {', '.join(missing)}")
    if extra:
        faults.append(f"{label} must not name {', '.join(extra)}")
    return faults


def _filter_names(
    override: dict[str, object],
) -> tuple[frozenset[str], list[str]]:
    """Return the binaries the serial override's filter names.

    A missing or non-string ``filter`` is reported in its own terms
    rather than coerced, because ``str(None)`` would reach the parser as
    the text ``None`` and report a syntax fault that hides the real
    problem.

    Parameters
    ----------
    override : dict of str to object
        The override assigning the serial group.

    Returns
    -------
    tuple
        The binary names the filter selects, and the faults.
    """
    filter_text = override.get("filter")
    if not isinstance(filter_text, str):
        return frozenset(), [
            (
                "the serial override must carry a string filter, "
                f"found {filter_text!r}"
            )
        ]
    try:
        return serial_group_binaries(filter_text), []
    except NextestConfigurationError as error:
        return frozenset(), [str(error)]


def serial_group_faults(
    config_text: str,
    expected: frozenset[str],
    tests_directory: Path = TESTS_DIR,
) -> list[str]:
    """Return every way the serial group drifts from the lock-taking set.

    Parameters
    ----------
    config_text : str
        The nextest configuration file's text.
    expected : frozenset of str
        The pinned inventory the filter and the sources must both name.
    tests_directory : Path
        The ``tests`` directory to scan for lock-taking sources.

    Returns
    -------
    list of str
        The faults, empty when the filter, the sources and the pin agree.
        A configuration that cannot be parsed is reported as a fault rather
        than raised, so every way of drifting reaches the caller through one
        channel.
    """
    try:
        config = _parsed(config_text)
    except NextestConfigurationError as error:
        return [str(error)]
    faults = _competing_group_faults(config)
    override, override_faults = _one_serial_override(config)
    faults.extend(override_faults)
    filter_names: frozenset[str] = frozenset()
    if override is not None:
        faults.extend(_platform_faults(override))
        filter_names, filter_faults = _filter_names(override)
        faults.extend(filter_faults)
    discovered, discovery_faults = scan_lock_consuming_binaries(tests_directory)
    faults.extend(discovery_faults)
    faults.extend(_difference_faults("the serial filter", filter_names, expected))
    faults.extend(
        _difference_faults("the discovered lock consumers", discovered, expected)
    )
    return faults