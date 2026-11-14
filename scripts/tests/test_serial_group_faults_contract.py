"""Drive every fault the serial-group checker can report.

Each way the checker can fail without raising is driven through it here, so a
fault that escapes as an exception, or arrives unnamed, is caught before it
can hide a real drift. The tree and the configuration come from
:mod:`serial_group_reference`, so this module judges the fault channel rather
than restating the fixtures.

See "Test timeouts: four tiers, outermost last" in
``docs/developers-guide.md``.
"""

from __future__ import annotations

import os
from pathlib import Path

import pytest

from serial_group import serial_group_faults
from serial_group_reference import (
    GOOD_CONFIG,
    REFERENCE_INVENTORY,
    write_repository,
)
from serial_inventory import scan_lock_consuming_binaries


def test_a_lock_use_no_binary_compiles_is_reported(tmp_path: Path) -> None:
    """A support module outside every binary is a fault, not a silent omission."""
    tests_directory = write_repository(tmp_path)
    (tests_directory / "support" / "orphan.rs").write_text(
        "fn f() { let _guard = serial_guard(); }\n", encoding="utf-8"
    )
    faults = serial_group_faults(
        GOOD_CONFIG, REFERENCE_INVENTORY, tests_directory
    )
    assert any("orphan" in fault for fault in faults), (
        f"a lock use no binary compiles must be reported: {faults}"
    )


def test_an_unreadable_tree_is_a_fault_not_an_exception(tmp_path: Path) -> None:
    """A tree that cannot be read is reported through the fault channel."""
    faults = serial_group_faults(
        GOOD_CONFIG,
        REFERENCE_INVENTORY,
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
    tests_directory = write_repository(tmp_path)
    blocked = tests_directory / "blocked"
    blocked.mkdir()
    (blocked / "hidden.rs").write_text(
        "fn f(_g: serial::ScenarioSerialGuard) {}\n", encoding="utf-8"
    )
    blocked.chmod(0)
    try:
        discovered, faults = scan_lock_consuming_binaries(tests_directory)
        group_faults = serial_group_faults(
            GOOD_CONFIG,
            REFERENCE_INVENTORY,
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
        GOOD_CONFIG, REFERENCE_INVENTORY, tmp_path
    )
    assert any("cannot resolve" in fault for fault in faults), (
        f"a resolution failure must be reported: {faults}"
    )


def test_a_source_that_cannot_be_resolved_is_a_named_fault(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A source-path failure after root resolution names the source."""
    tests_directory = write_repository(tmp_path)
    root = tests_directory.resolve()
    real_resolve = Path.resolve

    def unresolvable(self: Path, *args: object, **kwargs: object) -> Path:
        """Fail only for one source, so the root still resolves."""
        if self.name == "cluster_beta.rs":
            raise RuntimeError("symlink loop")
        return real_resolve(self, *args, **kwargs)

    monkeypatch.setattr(Path, "resolve", unresolvable)
    faults = serial_group_faults(
        GOOD_CONFIG, REFERENCE_INVENTORY, root
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
        config, REFERENCE_INVENTORY, write_repository(tmp_path)
    )
    assert any("string filter" in fault for fault in faults), (
        f"a missing filter must be named as such: {faults}"
    )


@pytest.mark.skipif(
    os.name == "nt", reason="creating a symlink needs a privilege on Windows"
)
def test_a_source_outside_the_tree_does_not_stop_the_scan(tmp_path: Path) -> None:
    """A symlink leaving the test tree is a fault, not a crash."""
    tests_directory = write_repository(tmp_path)
    outside = tmp_path / "outside.rs"
    outside.write_text("fn f(_g: serial::ScenarioSerialGuard) {}\n", encoding="utf-8")
    (tests_directory / "linked.rs").symlink_to(outside)
    discovered, faults = scan_lock_consuming_binaries(tests_directory)
    assert isinstance(discovered, frozenset)
    assert any("outside" in fault for fault in faults), (
        f"an unattributable lock use must be reported: {faults}"
    )
