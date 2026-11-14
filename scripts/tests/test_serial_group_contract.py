"""Hold the ``serial`` group to the binaries that take the scenario lock.

A check run only over the real files passes whether or not it discriminates,
so each way of leaving a lock-taking binary out, or letting a non-member in,
is driven through the checker and must fail. The fault channel itself is
driven in ``test_serial_group_faults_contract.py``, and the tree both modules
judge comes from ``serial_group_reference.py``. The real configuration is
judged last against the inventory ``test_nextest_tier_contract`` pins, so the
two contracts cannot drift apart.

See "Test timeouts: four tiers, outermost last" in
``docs/developers-guide.md``.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from cluster_sources import TESTS_DIR
from serial_group import serial_group_faults
from serial_group_reference import (
    GOOD_CONFIG,
    REFERENCE_INVENTORY,
    add_support_only_binary,
    write_repository,
)
from serial_inventory import scan_lock_consuming_binaries
from test_nextest_tier_contract import SERIAL_BINARIES
from timeout_budgets import NEXTEST_CONFIG


def test_the_reference_shape_has_no_fault(tmp_path: Path) -> None:
    """The reference configuration and tree agree with the pinned inventory."""
    tests_directory = write_repository(tmp_path)
    faults = serial_group_faults(
        GOOD_CONFIG, REFERENCE_INVENTORY, tests_directory
    )
    assert faults == [], f"the reference repository must be fault-free, got {faults}"


def test_the_discovered_set_matches_the_reference_tree(tmp_path: Path) -> None:
    """Each route to the lock attributes, and each non-route stays out.

    ``cluster_alpha`` takes the lock through a support module, ``cluster_beta``
    in its own crate root, and ``lock_dir_theta`` through a lock-directory file
    it declares itself, whose tests are named under the binary rather than
    under ``serial::``. ``isolated_gamma`` takes only the local guard and
    ``late_delta`` compiles only the lock module's own namespace, so neither
    is a member.
    """
    tests_directory = write_repository(tmp_path)
    discovered, faults = scan_lock_consuming_binaries(tests_directory)
    assert faults == [], f"the reference tree must be fault-free, got {faults}"
    assert discovered == REFERENCE_INVENTORY, (
        f"only the binaries that take the lock must be discovered, got {discovered}"
    )


def test_a_support_only_binary_is_discovered(tmp_path: Path) -> None:
    """A lock use reached only through a support module is attributed."""
    tests_directory = write_repository(tmp_path)
    add_support_only_binary(tests_directory)
    discovered, faults = scan_lock_consuming_binaries(tests_directory)
    assert faults == [], f"the support module belongs to solo: {faults}"
    assert "solo" in discovered, (
        f"a support-only lock use must attribute to its binary: {sorted(discovered)}"
    )


def test_a_support_only_binary_missing_from_the_filter_is_a_fault(
    tmp_path: Path,
) -> None:
    """The same binary is reported when the filter omits it."""
    tests_directory = write_repository(tmp_path)
    add_support_only_binary(tests_directory)
    faults = serial_group_faults(
        GOOD_CONFIG,
        REFERENCE_INVENTORY | {"solo"},
        tests_directory,
    )
    assert any("solo" in fault for fault in faults), (
        f"the omitted support-only binary must be named: {faults}"
    )


@pytest.mark.parametrize(
    ("old", "new", "keyword"),
    [
        (
            "  binary(=cluster_alpha)\n  | binary(=cluster_beta)",
            "  binary(=cluster_beta)",
            "cluster_alpha",
        ),
        (
            "\n  | kind(test) & test(/^serial::/)",
            "\n  | kind(test) & test(/^serial::/)\n  | binary(=mystery_binary)",
            "mystery_binary",
        ),
        ('\ntest-group = "serial"\n', "\n", "exactly one"),
        ('test-group = "serial"', 'test-group = "other"', "exactly one"),
        (
            (
                'slow-timeout = { period = "300s", terminate-after = 1, '
                'grace-period = "5s" }'
            ),
            (
                'slow-timeout = { period = "300s", terminate-after = 1, '
                'grace-period = "5s" }\ntest-group = "tier"'
            ),
            "before the serial one",
        ),
        (
            '[[profile.default.overrides]]\nfilter = """',
            (
                '[[profile.default.overrides]]\n'
                'platform = "x86_64-unknown-linux-gnu"\nfilter = """'
            ),
            "platform",
        ),
        (
            "\n  | kind(test) & test(/^serial::/)",
            "",
            "exactly once",
        ),
        ("binary(=cluster_alpha)", "all()", "binary(=name)"),
        (
            "binary(=cluster_alpha)",
            "not binary(=cluster_alpha)",
            "binary(=name)",
        ),
    ],
    ids=[
        "missing-binary",
        "unexpected-binary",
        "removed-group",
        "renamed-group",
        "group-assigned-ahead",
        "platform-restricted",
        "missing-embedded-term",
        "all-operator",
        "negation",
    ],
)
def test_a_mutation_is_a_fault(
    tmp_path: Path, old: str, new: str, keyword: str
) -> None:
    """Each mutation is reported, naming the binary, term or setting at fault."""
    assert old in GOOD_CONFIG, (
        f"the mutation target {old!r} must exist in the reference configuration"
    )
    tests_directory = write_repository(tmp_path)
    faults = serial_group_faults(
        GOOD_CONFIG.replace(old, new),
        REFERENCE_INVENTORY,
        tests_directory,
    )
    assert faults, f"replacing {old!r} with {new!r} must be reported"
    assert any(keyword in fault for fault in faults), (
        f"faults must mention {keyword!r}: {faults}"
    )


def test_a_missing_binary_is_a_fault(tmp_path: Path) -> None:
    """A valid filter that omits a lock consumer reports that binary."""
    old = "  binary(=cluster_alpha)\n  | binary(=cluster_beta)"
    assert old in GOOD_CONFIG, "the mutation target must exist"
    faults = serial_group_faults(
        GOOD_CONFIG.replace(old, "  binary(=cluster_beta)"),
        REFERENCE_INVENTORY,
        write_repository(tmp_path),
    )
    assert faults == ["the serial filter must name cluster_alpha"], (
        f"the missing binary, not a syntax error, must be reported: {faults}"
    )


def test_a_binary_that_starts_taking_the_lock_is_reported(tmp_path: Path) -> None:
    """A new lock consumer cannot time out unseen on a cold full run."""
    tests_directory = write_repository(tmp_path)
    (tests_directory / "late_epsilon.rs").write_text(
        "use serial::serial_guard;\n", encoding="utf-8"
    )
    faults = serial_group_faults(
        GOOD_CONFIG, REFERENCE_INVENTORY, tests_directory
    )
    assert any("late_epsilon" in fault for fault in faults), (
        f"a new lock consumer must be reported: {faults}"
    )


def test_a_directory_target_is_discovered(tmp_path: Path) -> None:
    """A ``tests/<name>/main.rs`` target is a binary like any other root."""
    tests_directory = write_repository(tmp_path)
    target = tests_directory / "late_zeta" / "main.rs"
    target.parent.mkdir()
    target.write_text("mod inner;\n", encoding="utf-8")
    (target.parent / "inner.rs").write_text(
        "fn f(_guard: serial::ScenarioSerialGuard) {}\n", encoding="utf-8"
    )
    faults = serial_group_faults(
        GOOD_CONFIG, REFERENCE_INVENTORY, tests_directory
    )
    assert any("late_zeta" in fault for fault in faults), (
        f"a directory-backed target must be discovered: {faults}"
    )


def test_the_repository_serial_group_holds_every_lock_consumer() -> None:
    """The repository's own configuration and sources agree with the pin."""
    faults = serial_group_faults(
        NEXTEST_CONFIG.read_text(encoding="utf-8"), SERIAL_BINARIES, TESTS_DIR
    )
    assert faults == [], (
        "the serial group must hold every binary that takes the lock, "
        f"got {faults}"
    )
