"""Hold the ``serial`` group contract to its reference shape.

A check run only over the real files passes whether or not it discriminates,
so each way of leaving a lock-taking binary out, or letting a non-member in,
is driven through the checker first and must fail. The real configuration is
judged last.
"""

from __future__ import annotations

import os
import re
import subprocess
import tempfile
import typing as typ
from pathlib import Path

import pytest
from hypothesis import given, settings, strategies as st

from serial_group import (
    CARGO_MANIFEST,
    LOCK_TAKING_BINARIES,
    NEXTEST_CONFIG,
    TESTS_DIRECTORY,
    _filter_names,
    serial_group_faults,
)
from serial_inventory import lock_taking_binaries

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


def test_a_directory_based_root_is_discovered(
    repository: tuple[Path, str],
) -> None:
    """A ``tests/<name>/main.rs`` root is attributed, not called hidden."""
    tests_directory, manifest = repository
    suite = tests_directory / "dir_suite" / "main.rs"
    suite.parent.mkdir()
    suite.write_text(
        f'{_serial_attribute("tests/dir_suite/main.rs")}\nmod serial;\n',
        encoding="utf-8",
    )
    faults = serial_group_faults(_GOOD_CONFIG, manifest, tests_directory)
    assert any(
        "dir_suite" in fault and "unexpected" in fault for fault in faults
    ), f"a directory-based root must count as a binary: {faults}"
    assert not any("outside a declared test root" in fault for fault in faults), (
        f"a directory-based root must not be called hidden: {faults}"
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


def test_a_manifest_that_is_not_toml_yields_a_fault_and_no_inventory(
    repository: tuple[Path, str],
) -> None:
    """Unreadable manifest text is reported instead of raising."""
    tests_directory, _ = repository
    inventory, faults = lock_taking_binaries("[test\nname = ", tests_directory)
    assert inventory == frozenset(), (
        "a manifest that cannot be parsed must not yield an inventory"
    )
    assert any("not valid TOML" in fault for fault in faults), (
        f"the parse failure must be reported: {faults}"
    )


def test_a_declared_root_that_cannot_be_read_is_reported(
    repository: tuple[Path, str],
) -> None:
    """A manifest entry with no root file is named, and is not a member."""
    tests_directory, manifest = repository
    (tests_directory / "data_dir_recovery.rs").unlink()
    inventory, faults = lock_taking_binaries(manifest, tests_directory)
    assert "data_dir_recovery" not in inventory, (
        "a binary whose root cannot be read must not be inventoried"
    )
    assert any(
        "cannot read" in fault and "data_dir_recovery" in fault for fault in faults
    ), f"the missing root must be reported by name: {faults}"


@pytest.mark.skipif(
    os.name == "nt" or os.geteuid() == 0,
    reason="file permissions do not deny access to this user",
)
def test_a_scanned_file_that_cannot_be_read_is_reported(
    repository: tuple[Path, str],
) -> None:
    """An unreadable ``.rs`` entry under ``tests`` is reported, not skipped."""
    tests_directory, manifest = repository
    unreadable = tests_directory / "unreadable.rs"
    unreadable.write_text('#[path = "support/serial.rs"]\n', encoding="utf-8")
    unreadable.chmod(0)
    try:
        _, faults = lock_taking_binaries(manifest, tests_directory)
    finally:
        unreadable.chmod(0o644)
    assert any(
        "cannot read" in fault and "unreadable.rs" in fault for fault in faults
    ), f"an unreadable scanned file must be reported: {faults}"


@pytest.mark.skipif(
    os.name == "nt" or os.geteuid() == 0,
    reason="directory permissions do not deny access to this user",
)
def test_a_directory_that_cannot_be_listed_is_reported(
    repository: tuple[Path, str],
) -> None:
    """An unreadable directory is a scan fault, not a silent empty result."""
    tests_directory, manifest = repository
    blocked = tests_directory / "blocked"
    blocked.mkdir()
    (blocked / "hidden_root.rs").write_text(
        '#[path = "../support/serial.rs"]\n', encoding="utf-8"
    )
    blocked.chmod(0)
    try:
        _, faults = lock_taking_binaries(manifest, tests_directory)
    finally:
        blocked.chmod(0o755)
    assert any("cannot list" in fault and "blocked" in fault for fault in faults), (
        f"an unreadable directory must be reported as a scan fault: {faults}"
    )


def test_the_public_contract_reports_a_missing_root(
    repository: tuple[Path, str],
) -> None:
    """The discovery fault reaches the caller through the public boundary."""
    tests_directory, manifest = repository
    (tests_directory / "timezone_regression.rs").unlink()
    faults = serial_group_faults(_GOOD_CONFIG, manifest, tests_directory)
    assert any(
        "cannot read" in fault and "timezone_regression" in fault for fault in faults
    ), f"the public contract must surface the discovery fault: {faults}"


_ANSI_ESCAPE = re.compile(r"\x1b\[[0-9;]*m")
_GROUP_HEADER = re.compile(
    r"^group: (?P<name>\S+) \(max threads = (?P<limit>\d+)\)$", re.MULTILINE
)
_BINARY_ENTRY = re.compile(r"^\s+\w+::(?P<name>\S+):$", re.MULTILINE)


def _group_members(output: str) -> dict[str, tuple[int, set[str]]]:
    """Map each printed group to its thread limit and member binaries.

    Parameters
    ----------
    output : str
        Standard output of ``cargo nextest show-config test-groups``.

    Returns
    -------
    dict of str to tuple
        One entry per group, holding ``max threads`` and the binary names
        nextest reported under that group.
    """
    text = _ANSI_ESCAPE.sub("", output)
    headers = list(_GROUP_HEADER.finditer(text))
    groups: dict[str, tuple[int, set[str]]] = {}
    for index, header in enumerate(headers):
        end = headers[index + 1].start() if index + 1 < len(headers) else len(text)
        block = text[header.end() : end]
        groups[header["name"]] = (
            int(header["limit"]),
            set(_BINARY_ENTRY.findall(block)),
        )
    return groups


_FILTER_BINARY = re.compile(r"binary\(=([A-Za-z0-9_-]+)\)")

#: Where the probe's generated crate lives. Cargo keys a path package's
#: freshness on its manifest's location, so a stable path — not just a stable
#: ``CARGO_TARGET_DIR`` — is what lets the dependency-free crate stay warm:
#: the behavioural check then costs a compile only when nextest, the toolchain
#: or the checked-in configuration changes.
_PROBE_CRATE = Path(__file__).resolve().parents[2] / "target" / "serial-group-probe-crate"
_PROBE_TARGET = _PROBE_CRATE / "target"


def _configured_binaries() -> frozenset[str]:
    """Return every binary name the real configuration mentions.

    nextest rejects a ``binary(=name)`` term that matches nothing, so the
    probe must define a binary for each name the checked-in overrides use,
    including the ones that only narrow a timeout.

    Returns
    -------
    frozenset of str
        The lock-taking binaries and the timeout-only names.
    """
    mentioned = _FILTER_BINARY.findall(NEXTEST_CONFIG.read_text(encoding="utf-8"))
    return frozenset(mentioned) | LOCK_TAKING_BINARIES


def _write_if_changed(path: Path, text: str) -> None:
    """Write ``text`` unless ``path`` already holds it.

    Leaving an unchanged file untouched preserves its modification time, which
    is what lets Cargo treat the probe as fresh on the next run.
    """
    if path.exists() and path.read_text(encoding="utf-8") == text:
        return
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


@pytest.fixture
def nextest_probe() -> Path:
    """Write a dependency-free crate that carries the real nextest config.

    nextest answers a question about test binaries, so the probe needs one
    test case per lock-taking binary. Copying the checked-in configuration
    verbatim is the point: the grammar under test is nextest's own, not a
    restatement of it.

    Returns
    -------
    Path
        The probe crate's root directory.
    """
    crate = _PROBE_CRATE
    # `[workspace]` stops the probe from searching upwards and finding the
    # repository's workspace, which would reject it as a non-member.
    _write_if_changed(
        crate / "Cargo.toml",
        '[package]\nname = "probe"\nversion = "0.0.0"\nedition = "2021"\n'
        "\n[workspace]\n",
    )
    _write_if_changed(crate / "src" / "lib.rs", "")
    _write_if_changed(
        crate / ".config" / "nextest.toml",
        NEXTEST_CONFIG.read_text(encoding="utf-8"),
    )
    configured = _configured_binaries()
    tests_directory = crate / "tests"
    tests_directory.mkdir(exist_ok=True)
    for stale in tests_directory.glob("*.rs"):
        if stale.stem not in configured:
            stale.unlink()
    for name in sorted(configured):
        _write_if_changed(
            tests_directory / f"{name}.rs", "#[test]\nfn ok() {}\n"
        )
    return crate


def _cargo_executable() -> str:
    """Return the Cargo executable the surrounding build is using.

    Cargo exports ``CARGO`` to the processes it runs, so honouring it keeps
    the probe on the same toolchain as the gate that invokes the suite.
    """
    return os.environ.get("CARGO", "cargo")


def _nextest_is_available() -> bool:
    """Report whether ``cargo nextest`` can be run at all.

    The probe is an optional cross-check, so a host without cargo-nextest —
    or without a Cargo executable on ``PATH`` — skips it rather than failing
    the contract, which does not depend on nextest being installed.
    """
    try:
        version = subprocess.run(
            [_cargo_executable(), "nextest", "--version"],
            capture_output=True,
            text=True,
            check=False,
        )
    except OSError:
        return False
    return version.returncode == 0


def test_nextest_places_every_lock_taking_binary_in_the_serial_group(
    nextest_probe: Path,
) -> None:
    """nextest's own parser agrees with the pinned inventory and the limit."""
    if not _nextest_is_available():
        pytest.skip("cargo-nextest is not installed, so the parser cannot be asked")
    result = subprocess.run(
        [
            _cargo_executable(),
            "nextest",
            "show-config",
            "test-groups",
            "--all-targets",
            "--offline",
        ],
        cwd=nextest_probe,
        capture_output=True,
        text=True,
        timeout=600,
        check=False,
        env={**os.environ, "CARGO_TARGET_DIR": str(_PROBE_TARGET)},
    )
    assert result.returncode == 0, f"cargo nextest failed: {result.stderr}"
    groups = _group_members(result.stdout)
    assert groups.get("serial") == (1, set(LOCK_TAKING_BINARIES)), (
        "nextest must place every lock-taking binary in one serial group with "
        f"max threads 1, got {groups}"
    )


def test_a_nextest_configuration_that_is_not_toml_is_a_fault(
    repository: tuple[Path, str],
) -> None:
    """A malformed nextest file is reported, not raised, at the boundary."""
    tests_directory, manifest = repository
    faults = serial_group_faults("[profile.default\n", manifest, tests_directory)
    assert any("not valid TOML" in fault for fault in faults), (
        "a configuration that cannot be parsed must surface as a fault: "
        f"{faults}"
    )


#: Names the generated filters may use: nextest's own ``[A-Za-z0-9_]`` set.
_BINARY_NAMES = st.from_regex(r"[a-z][a-z0-9_]{0,15}", fullmatch=True)

#: Whitespace and separators an author might place between two terms.
_SEPARATORS = st.sampled_from(["|", " | ", "\n| ", "\n  |  "])

#: Terms that are not the single exact form the contract accepts.
_BAD_TERMS = st.sampled_from(
    [
        "all()",
        "binary(=)",
        "binary(=a b)",
        "binary(=a-b)",
        "not(binary(=a))",
        "binary(=a) & binary(=b)",
    ]
)


@settings(max_examples=50, deadline=None)
@given(names=st.lists(_BINARY_NAMES, unique=True, max_size=8), separator=_SEPARATORS)
def test_an_exact_filter_names_exactly_its_terms(
    names: list[str], separator: str
) -> None:
    """Any sequence of exact terms parses to that set, whatever separates them."""
    filter_text = separator.join(f"binary(={name})" for name in names)
    parsed, faults = _filter_names({"filter": filter_text})
    if not names:
        assert faults, "an empty filter names no binary and must be a fault"
        return
    assert faults == [], f"{filter_text!r} must be accepted, got {faults}"
    assert parsed == frozenset(names), (
        f"{filter_text!r} must name {frozenset(names)}, got {parsed}"
    )


@settings(max_examples=50, deadline=None)
@given(names=st.lists(_BINARY_NAMES, unique=True, max_size=5), bad=_BAD_TERMS)
def test_a_term_outside_the_grammar_is_a_fault(
    names: list[str], bad: str
) -> None:
    """A foreign term is reported while the exact terms beside it survive."""
    terms = [f"binary(={name})" for name in names]
    terms.append(bad)
    parsed, faults = _filter_names({"filter": " | ".join(terms)})
    assert len(faults) == 1, f"{bad!r} must be the only fault, got {faults}"
    assert parsed == frozenset(names), (
        "the exact terms must still be read beside a foreign one"
    )


@settings(max_examples=25, deadline=None)
@given(
    names=st.lists(_BINARY_NAMES, unique=True, max_size=6),
    layout=st.lists(
        st.tuples(st.booleans(), st.booleans()), min_size=6, max_size=6
    ),
)
def test_discovery_follows_which_roots_compile_the_module(
    names: list[str], layout: list[tuple[bool, bool]]
) -> None:
    """Every root layout is discovered, and only includers join the inventory.

    Hypothesis reuses one pytest fixture value across examples, so the tree
    is built in its own temporary directory rather than under ``tmp_path``.
    """
    with tempfile.TemporaryDirectory() as directory:
        tests_directory = Path(directory) / "tests"
        tests_directory.mkdir()
        expected: set[str] = set()
        for name, (includes, is_directory_module) in zip(names, layout):
            if is_directory_module:
                root = tests_directory / name / "main.rs"
                root.parent.mkdir()
                attribute = '#[path = "../support/serial.rs"]'
            else:
                root = tests_directory / f"{name}.rs"
                attribute = '#[path = "support/serial.rs"]'
            body = f"{attribute}\nmod serial;\n" if includes else "// unrelated\n"
            root.write_text(body, encoding="utf-8")
            if includes:
                expected.add(name)
        inventory, faults = lock_taking_binaries("", tests_directory)
    assert faults == [], f"the generated tree must be clean, got {faults}"
    assert inventory == frozenset(expected), (
        f"the inventory must be {frozenset(expected)}, got {inventory}"
    )


def test_an_inclusion_outside_a_root_is_always_reported(tmp_path: Path) -> None:
    """A support file that compiles the module is a fault, never a silent skip."""
    tests_directory = tmp_path / "tests"
    (tests_directory / "support").mkdir(parents=True)
    (tests_directory / "support" / "extra.rs").write_text(
        '#[path = "support/serial.rs"]\n', encoding="utf-8"
    )
    inventory, faults = lock_taking_binaries("", tests_directory)
    assert inventory == frozenset(), (
        f"no root compiles the module, so the inventory must be empty, got {inventory}"
    )
    assert any("outside a declared test root" in fault for fault in faults), (
        f"the unattributable inclusion must be reported: {faults}"
    )
