"""The constructed tree and configuration the serial contracts judge.

``test_serial_group_contract.py`` and
``test_serial_group_faults_contract.py`` both need a tree that reaches the
lock in several ways and a configuration that schedules it. Keeping the
fixtures here means the two contracts cannot disagree about the shape they
judge, and neither file grows past a readable size.

See "Test timeouts: four tiers, outermost last" in
``docs/developers-guide.md``.
"""

from __future__ import annotations

from pathlib import Path

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
SOURCES: dict[str, str] = {
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
REFERENCE_INVENTORY: frozenset[str] = frozenset(
    {"cluster_alpha", "cluster_beta", "lock_dir_theta"}
)

#: The nextest configuration that schedules the reference tree.
GOOD_CONFIG = """[profile.default]
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


def write_repository(tmp_path: Path) -> Path:
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
    for relative, text in SOURCES.items():
        path = tests_directory / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
    return tests_directory


def add_support_only_binary(tests_directory: Path) -> None:
    """Add a binary whose only lock use is in an included support module."""
    (tests_directory / "support" / "only_lock.rs").write_text(
        "fn f(_g: serial::ScenarioSerialGuard) {}\n", encoding="utf-8"
    )
    (tests_directory / "solo.rs").write_text(
        '#[path = "support/only_lock.rs"]\nmod only_lock;\n', encoding="utf-8"
    )

