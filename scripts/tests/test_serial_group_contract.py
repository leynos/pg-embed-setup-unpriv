"""Hold the ``serial`` group contract to its reference shape.

A check run only over the real files passes whether or not it discriminates,
so each way of leaving a lock-taking binary out, or letting a non-member in,
is driven through the checker first and must fail. The real configuration is
judged last.
"""

from __future__ import annotations

import typing as typ
from pathlib import Path

import pytest

from serial_group import (
    CARGO_MANIFEST,
    LOCK_TAKING_BINARIES,
    NEXTEST_CONFIG,
    TESTS_DIRECTORY,
    serial_group_faults,
)

_GOOD_FILTER = "\n  | ".join(
    f"binary(={name})" for name in sorted(LOCK_TAKING_BINARIES)
)

_GOOD_CONFIG = f'''[profile.default]
slow-timeout = {{ period = "180s", terminate-after = 1, grace-period = "5s" }}

[test-groups]
serial = {{ max-threads = 1 }}

[[profile.default.overrides]]
filter = """
  {_GOOD_FILTER}
"""
test-group = "serial"
'''

#: Root files whose declared path is a directory module rather than a file.
_ROOT_PATHS: typ.Final[dict[str, str]] = {
    "test_cluster_fixture": "tests/test_cluster_fixture/mod.rs",
}


def _root_path(name: str) -> str:
    """Return the manifest path of one binary's root file.

    Parameters
    ----------
    name : str
        The integration-test target name.

    Returns
    -------
    str
        The path relative to the repository root.
    """
    return _ROOT_PATHS.get(name, f"tests/{name}.rs")


def _serial_attribute(relative: str) -> str:
    """Return the serial-module attribute a root file at ``relative`` uses.

    A root directly under ``tests`` reaches the shared file as
    ``support/serial.rs``; a directory module one level deeper reaches it
    as ``../support/serial.rs``.

    Parameters
    ----------
    relative : str
        The root file path relative to the repository root.

    Returns
    -------
    str
        The ``#[path = ...]`` attribute naming the shared module.
    """
    depth = len(Path(relative).parts) - 1
    prefix = "../" * (depth - 1)
    return f'#[path = "{prefix}support/serial.rs"]'


@pytest.fixture
def repository(tmp_path: Path) -> tuple[Path, str]:
    """Write a reference manifest and tests tree.

    Parameters
    ----------
    tmp_path : Path
        The test's temporary directory.

    Returns
    -------
    tuple
        The ``tests`` directory and the manifest text.
    """
    tests_directory = tmp_path / "tests"
    tests_directory.mkdir(parents=True, exist_ok=True)
    declarations: list[str] = []
    for name in sorted(LOCK_TAKING_BINARIES):
        relative = _root_path(name)
        root_file = tmp_path / relative
        root_file.parent.mkdir(parents=True, exist_ok=True)
        root_file.write_text(
            f"{_serial_attribute(relative)}\nmod serial;\n", encoding="utf-8"
        )
        declarations.append(
            f'[[test]]\nname = "{name}"\npath = "{relative}"\n'
        )
    # A declared binary that does not compile the module must stay excluded.
    (tests_directory / "settings.rs").write_text(
        "// no serial module\n", encoding="utf-8"
    )
    declarations.append('[[test]]\nname = "settings"\npath = "tests/settings.rs"\n')
    # An undeclared top-level file without the attribute is not a target.
    (tests_directory / "untracked_helper.rs").write_text(
        "// helper\n", encoding="utf-8"
    )
    return tests_directory, "\n".join(declarations)


def test_the_reference_shape_has_no_fault(
    repository: tuple[Path, str],
) -> None:
    """The reference configuration, manifest and tree agree with the pin."""
    tests_directory, manifest = repository
    faults = serial_group_faults(_GOOD_CONFIG, manifest, tests_directory)
    assert faults == [], f"the reference repository must be fault-free, got {faults}"


@pytest.mark.parametrize(
    ("old", "new", "keyword"),
    [
        (
            "\n  | binary(=test_cluster_fixture)",
            "",
            "test_cluster_fixture",
        ),
        ("\n  | binary(=data_dir_recovery)", "", "data_dir_recovery"),
        ("\n  | binary(=test_cluster_async)", "", "test_cluster_async"),
        (
            "\n  | binary(=timezone_regression)",
            "\n  | binary(=timezone_regression)\n  | binary(=mystery_binary)",
            "mystery_binary",
        ),
        (
            "serial = { max-threads = 1 }",
            "serial = { max-threads = 2 }",
            "max-threads",
        ),
        ('test-group = "serial"', 'test-group = "other"', "exactly one"),
        ("\ntest-group = \"serial\"\n", "\n", "exactly one"),
        (
            'test-group = "serial"',
            'platform = "x86_64-unknown-linux-gnu"\ntest-group = "serial"',
            "platform",
        ),
        (
            "[[profile.default.overrides]]",
            '[[profile.default.overrides]]\nfilter = "all()"\n'
            'test-group = "other"\n\n[[profile.default.overrides]]',
            "before the serial one",
        ),
        ("binary(=bootstrap_privileges)", "all()", "binary(=name)"),
        (
            "binary(=test_cluster_fixture)",
            "not binary(=test_cluster_fixture)",
            "binary(=name)",
        ),
        (
            "\n  | binary(=test_cluster_fixture)",
            "\n  and binary(=test_cluster_fixture)",
            "binary(=name)",
        ),
    ],
    ids=[
        "missing-fixture-binary",
        "missing-recovery-binary",
        "missing-async-binary",
        "unexpected-binary",
        "max-threads-above-one",
        "renamed-group",
        "removed-group",
        "platform-restricted",
        "competing-earlier-group",
        "all-operator",
        "negation",
        "conjunction",
    ],
)
def test_a_mutation_is_a_fault(
    repository: tuple[Path, str], old: str, new: str, keyword: str
) -> None:
    """Each mutation is reported, naming the binary or setting at fault."""
    assert old in _GOOD_CONFIG, (
        f"the mutation target {old!r} must exist in the reference configuration"
    )
    tests_directory, manifest = repository
    faults = serial_group_faults(
        _GOOD_CONFIG.replace(old, new), manifest, tests_directory
    )
    assert faults, f"replacing {old!r} with {new!r} must be reported"
    assert any(keyword in fault for fault in faults), (
        f"faults must mention {keyword!r}: {faults}"
    )


def test_an_undeclared_lock_taking_binary_is_named(
    repository: tuple[Path, str],
) -> None:
    """A new top-level file that includes the module cannot join unseen."""
    tests_directory, manifest = repository
    stray = tests_directory / "stray_suite.rs"
    stray.write_text(
        f"{_serial_attribute('tests/stray_suite.rs')}\nmod serial;\n",
        encoding="utf-8",
    )
    faults = serial_group_faults(_GOOD_CONFIG, manifest, tests_directory)
    assert any("stray_suite" in fault for fault in faults), (
        f"an undeclared lock-taking binary must be reported: {faults}"
    )


def test_a_support_module_compiling_the_serial_module_is_named(
    repository: tuple[Path, str],
) -> None:
    """A root that hides the module behind a support module cannot pass."""
    tests_directory, manifest = repository
    support = tests_directory / "support"
    support.mkdir(exist_ok=True)
    hidden = support / "shared.rs"
    hidden.write_text(
        f'{_serial_attribute("tests/support/shared.rs")}\n', encoding="utf-8"
    )
    faults = serial_group_faults(_GOOD_CONFIG, manifest, tests_directory)
    assert any("shared.rs" in fault for fault in faults), (
        f"an indirect serial inclusion must be reported: {faults}"
    )


def test_the_real_repository_serial_group_holds_every_lock_taking_binary() -> None:
    """The repository's own configuration and tests tree agree with the pin."""
    faults = serial_group_faults(
        NEXTEST_CONFIG.read_text(encoding="utf-8"),
        CARGO_MANIFEST.read_text(encoding="utf-8"),
        TESTS_DIRECTORY,
    )
    assert faults == [], (
        "the real serial group must hold every lock-taking binary, "
        f"got {faults}"
    )