"""Hold the ``serial`` group to the binaries that take the scenario lock.

A check run only over the real files passes whether or not it discriminates,
so each way of leaving a lock-taking binary out, or letting a non-member in,
is driven through the checker first and must fail. The real configuration is
judged last against the inventory ``test_nextest_tier_contract`` pins, so the
two contracts cannot drift apart.

See "Test timeouts: four tiers, outermost last" in
``docs/developers-guide.md``.
"""

from __future__ import annotations

import os
from pathlib import Path

import pytest

from cluster_sources import TESTS_DIR
from serial_group import serial_group_faults
from serial_inventory import scan_lock_consuming_binaries
from test_nextest_tier_contract import SERIAL_BINARIES
from timeout_budgets import NEXTEST_CONFIG

#: The reference tree's sources, keyed by path relative to ``tests``.
#:
#: ``cluster_alpha`` reaches the lock only through a support module, so the
#: attribution the reader performs is exercised rather than assumed.
#: ``isolated_gamma`` takes only the in-process guard. ``late_delta`` compiles
#: the lock module, which brings in ``support/serial/inner.rs``, but keeps its
#: own cases isolated: both files are named under ``serial::``, so the
#: embedded term covers them and the binary stays out. ``lock_dir_theta`` also
#: compiles a file from that directory, but declares it at its own crate root,
#: so the file's tests are named ``cases::`` rather than ``serial::cases::``
#: and the binary is a member. The directory does not decide the exemption;
#: the namespace does. ``support/serial/uncompiled.rs`` is a lock use in that
#: directory that no binary compiles, so it is neither joined nor reported.
_SOURCES: dict[str, str] = {
    "cluster_alpha.rs": '#[path = "support/locks.rs"]\nmod locks;\n',
    "cluster_beta.rs": "fn f(_guard: serial::ScenarioSerialGuard) {}\n",
    "isolated_gamma.rs": (
        "use serial::{ScenarioLocalGuard, local_serial_guard};\n"
    ),
    "late_delta.rs": (
        '#[path = "support/serial.rs"]\n'
        "mod serial;\n"
        "fn f(_guard: serial::ScenarioLocalGuard) {}\n"
    ),
    "lock_dir_theta.rs": (
        '#[path = "support/serial/cases.rs"]\n'
        "mod cases;\n"
        "fn f(_g: serial::ScenarioLocalGuard) {}\n"
    ),
    "support/locks.rs": "fn held() { let _guard = serial_guard(); }\n",
    "support/serial.rs": "pub fn serial_guard() {}\nmod inner;\n",
    "support/serial/inner.rs": "fn held() { let _guard = serial_guard(); }\n",
    "support/serial/cases.rs": "fn held() { let _guard = serial_guard(); }\n",
    "support/serial/uncompiled.rs": (
        "fn f(_g: serial::ScenarioSerialGuard) {}\n"
    ),
}

#: The binaries the reference tree and configuration agree on.
_REFERENCE_INVENTORY: frozenset[str] = frozenset(
    {"cluster_alpha", "cluster_beta", "lock_dir_theta"}
)

_GOOD_CONFIG = """[profile.default]
slow-timeout = { period = "180s", terminate-after = 1, grace-period = "5s" }

# A tier ahead of the group, which assigns a setting but no group of its own.
[[profile.default.overrides]]
filter = "binary(=cluster_beta)"
slow-timeout = { period = "300s", terminate-after = 1, grace-period = "5s" }

[test-groups]
serial = { max-threads = 1 }

[[profile.default.overrides]]
filter = \"\"\"
  binary(=cluster_alpha)
  | binary(=cluster_beta)
  | binary(=lock_dir_theta)
  | kind(test) & test(/^serial::/)
\"\"\"
test-group = "serial"
"""


def _write_repository(tmp_path: Path) -> Path:
    """Write the reference test tree and return its ``tests`` directory.

    Parameters
    ----------
    tmp_path : Path
        The test's temporary directory.

    Returns
    -------
    Path
        The directory holding the written sources.
    """
    tests_directory = tmp_path / "tests"
    for relative, text in _SOURCES.items():
        path = tests_directory / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
    return tests_directory


def test_the_reference_shape_has_no_fault(tmp_path: Path) -> None:
    """The reference configuration and tree agree with the pinned inventory."""
    tests_directory = _write_repository(tmp_path)
    faults = serial_group_faults(
        _GOOD_CONFIG, _REFERENCE_INVENTORY, tests_directory
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
    tests_directory = _write_repository(tmp_path)
    discovered, faults = scan_lock_consuming_binaries(tests_directory)
    assert faults == [], f"the reference tree must be fault-free, got {faults}"
    assert discovered == _REFERENCE_INVENTORY, (
        f"only the binaries that take the lock must be discovered, got {discovered}"
    )


def _add_support_only_binary(tests_directory: Path) -> None:
    """Add a binary whose only lock use is in an included support module."""
    (tests_directory / "support" / "only_lock.rs").write_text(
        "fn f(_g: serial::ScenarioSerialGuard) {}\n", encoding="utf-8"
    )
    (tests_directory / "solo.rs").write_text(
        '#[path = "support/only_lock.rs"]\nmod only_lock;\n', encoding="utf-8"
    )


def test_a_support_only_binary_is_discovered(tmp_path: Path) -> None:
    """A lock use reached only through a support module is attributed."""
    tests_directory = _write_repository(tmp_path)
    _add_support_only_binary(tests_directory)
    discovered, faults = scan_lock_consuming_binaries(tests_directory)
    assert faults == [], f"the support module belongs to solo: {faults}"
    assert "solo" in discovered, (
        f"a support-only lock use must attribute to its binary: {sorted(discovered)}"
    )


def test_a_support_only_binary_missing_from_the_filter_is_a_fault(
    tmp_path: Path,
) -> None:
    """The same binary is reported when the filter omits it."""
    tests_directory = _write_repository(tmp_path)
    _add_support_only_binary(tests_directory)
    faults = serial_group_faults(
        _GOOD_CONFIG,
        _REFERENCE_INVENTORY | {"solo"},
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
    assert old in _GOOD_CONFIG, (
        f"the mutation target {old!r} must exist in the reference configuration"
    )
    tests_directory = _write_repository(tmp_path)
    faults = serial_group_faults(
        _GOOD_CONFIG.replace(old, new),
        _REFERENCE_INVENTORY,
        tests_directory,
    )
    assert faults, f"replacing {old!r} with {new!r} must be reported"
    assert any(keyword in fault for fault in faults), (
        f"faults must mention {keyword!r}: {faults}"
    )


def test_a_missing_binary_is_a_fault(tmp_path: Path) -> None:
    """A valid filter that omits a lock consumer reports that binary."""
    old = "  binary(=cluster_alpha)\n  | binary(=cluster_beta)"
    assert old in _GOOD_CONFIG, "the mutation target must exist"
    faults = serial_group_faults(
        _GOOD_CONFIG.replace(old, "  binary(=cluster_beta)"),
        _REFERENCE_INVENTORY,
        _write_repository(tmp_path),
    )
    assert faults == ["the serial filter must name cluster_alpha"], (
        f"the missing binary, not a syntax error, must be reported: {faults}"
    )


def test_a_binary_that_starts_taking_the_lock_is_reported(tmp_path: Path) -> None:
    """A new lock consumer cannot time out unseen on a cold full run."""
    tests_directory = _write_repository(tmp_path)
    (tests_directory / "late_epsilon.rs").write_text(
        "use serial::serial_guard;\n", encoding="utf-8"
    )
    faults = serial_group_faults(
        _GOOD_CONFIG, _REFERENCE_INVENTORY, tests_directory
    )
    assert any("late_epsilon" in fault for fault in faults), (
        f"a new lock consumer must be reported: {faults}"
    )


def test_a_directory_target_is_discovered(tmp_path: Path) -> None:
    """A ``tests/<name>/main.rs`` target is a binary like any other root."""
    tests_directory = _write_repository(tmp_path)
    target = tests_directory / "late_zeta" / "main.rs"
    target.parent.mkdir()
    target.write_text("mod inner;\n", encoding="utf-8")
    (target.parent / "inner.rs").write_text(
        "fn f(_guard: serial::ScenarioSerialGuard) {}\n", encoding="utf-8"
    )
    faults = serial_group_faults(
        _GOOD_CONFIG, _REFERENCE_INVENTORY, tests_directory
    )
    assert any("late_zeta" in fault for fault in faults), (
        f"a directory-backed target must be discovered: {faults}"
    )


def test_a_lock_use_no_binary_compiles_is_reported(tmp_path: Path) -> None:
    """A support module outside every binary is a fault, not a silent omission."""
    tests_directory = _write_repository(tmp_path)
    (tests_directory / "support" / "orphan.rs").write_text(
        "fn f() { let _guard = serial_guard(); }\n", encoding="utf-8"
    )
    faults = serial_group_faults(
        _GOOD_CONFIG, _REFERENCE_INVENTORY, tests_directory
    )
    assert any("orphan" in fault for fault in faults), (
        f"a lock use no binary compiles must be reported: {faults}"
    )


def test_an_unreadable_tree_is_a_fault_not_an_exception(tmp_path: Path) -> None:
    """A tree that cannot be read is reported through the fault channel."""
    faults = serial_group_faults(
        _GOOD_CONFIG,
        _REFERENCE_INVENTORY,
        tmp_path / "missing",
    )
    assert any("missing" in fault for fault in faults), (
        f"an unreadable tree must be reported: {faults}"
    )


@pytest.mark.skipif(os.name == "nt", reason="mode denial is not enforced on Windows")
@pytest.mark.skipif(
    hasattr(os, "geteuid") and os.geteuid() == 0,
    reason="root can list a directory whose mode denies everyone",
)
def test_an_unlistable_directory_is_one_named_fault(tmp_path: Path) -> None:
    """A directory the walk cannot list is a fault, not an exception."""
    tests_directory = _write_repository(tmp_path)
    blocked = tests_directory / "blocked"
    blocked.mkdir()
    (blocked / "hidden.rs").write_text(
        "fn f(_g: serial::ScenarioSerialGuard) {}\n", encoding="utf-8"
    )
    blocked.chmod(0)
    try:
        discovered, faults = scan_lock_consuming_binaries(tests_directory)
        group_faults = serial_group_faults(
            _GOOD_CONFIG,
            _REFERENCE_INVENTORY,
            tests_directory,
        )
    finally:
        blocked.chmod(0o755)
    assert isinstance(discovered, frozenset)
    assert len(faults) == 1, f"one fault must name the directory: {faults}"
    assert "blocked" in faults[0], f"the directory must be named: {faults}"
    assert any("blocked" in fault for fault in group_faults), (
        f"the group contract must carry the same fault: {group_faults}"
    )


def test_a_directory_that_cannot_be_resolved_is_a_fault(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A resolution failure is a fault, not an exception from the scan."""

    def unresolvable(self: Path, *args: object, **kwargs: object) -> Path:
        """Stand in for a resolution the filesystem refuses."""
        raise RuntimeError("symlink loop")

    monkeypatch.setattr(Path, "resolve", unresolvable)
    faults = serial_group_faults(
        _GOOD_CONFIG, _REFERENCE_INVENTORY, tmp_path
    )
    assert any("cannot resolve" in fault for fault in faults), (
        f"a resolution failure must be reported: {faults}"
    )


def test_a_source_that_cannot_be_resolved_is_a_named_fault(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A source-path failure after root resolution names the source."""
    tests_directory = _write_repository(tmp_path)
    root = tests_directory.resolve()
    real_resolve = Path.resolve

    def unresolvable(self: Path, *args: object, **kwargs: object) -> Path:
        """Fail only for one source, so the root still resolves."""
        if self.name == "cluster_beta.rs":
            raise RuntimeError("symlink loop")
        return real_resolve(self, *args, **kwargs)

    monkeypatch.setattr(Path, "resolve", unresolvable)
    faults = serial_group_faults(
        _GOOD_CONFIG, _REFERENCE_INVENTORY, root
    )
    assert any(
        "cluster_beta.rs" in fault and "symlink loop" in fault for fault in faults
    ), f"a source-path failure must name the source and cause: {faults}"


def test_the_serial_override_must_carry_a_string_filter(tmp_path: Path) -> None:
    """A missing filter is reported as such, not as a syntax fault."""
    config = """[test-groups]
serial = { max-threads = 1 }

[[profile.default.overrides]]
test-group = "serial"
"""
    faults = serial_group_faults(
        config, _REFERENCE_INVENTORY, _write_repository(tmp_path)
    )
    assert any("string filter" in fault for fault in faults), (
        f"a missing filter must be named as such: {faults}"
    )


@pytest.mark.skipif(
    os.name == "nt", reason="creating a symlink needs a privilege on Windows"
)
def test_a_source_outside_the_tree_does_not_stop_the_scan(tmp_path: Path) -> None:
    """A symlink leaving the test tree is a fault, not a crash."""
    tests_directory = _write_repository(tmp_path)
    outside = tmp_path / "outside.rs"
    outside.write_text("fn f(_g: serial::ScenarioSerialGuard) {}\n", encoding="utf-8")
    (tests_directory / "linked.rs").symlink_to(outside)
    discovered, faults = scan_lock_consuming_binaries(tests_directory)
    assert isinstance(discovered, frozenset)
    assert any("outside" in fault for fault in faults), (
        f"an unattributable lock use must be reported: {faults}"
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