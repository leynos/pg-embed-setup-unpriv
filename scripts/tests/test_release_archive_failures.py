"""Failure-path and property tests for the release archive packager."""

from __future__ import annotations

import importlib.util
import re
import shlex
import sys
import tarfile
from pathlib import Path

import pytest
from cmd_mox import CmdMox
from hypothesis import given, settings
from hypothesis import strategies as st

SCRIPT_PATH = Path(__file__).resolve().parents[1] / "release_archive.py"
sys.path.insert(0, str(SCRIPT_PATH.parent))
SPEC = importlib.util.spec_from_file_location("release_archive", SCRIPT_PATH)
assert SPEC is not None
release_archive = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = release_archive
SPEC.loader.exec_module(release_archive)
import release_archive_cargo

SAFE_COMPONENT = st.text(
    alphabet=st.characters(
        whitelist_categories=("Ll", "Lu", "Nd"),
        whitelist_characters="-_",
    ),
    min_size=1,
    max_size=32,
)
SAFE_WORD = st.text(
    alphabet=st.characters(
        whitelist_categories=("Ll", "Lu", "Nd"),
        whitelist_characters="-_",
    ),
    min_size=1,
    max_size=16,
)
PATH_LIKE_COMPONENT = st.one_of(
    st.just(""),
    st.just("."),
    st.just(".."),
    SAFE_COMPONENT.map(lambda value: f"{value}..tail"),
    SAFE_COMPONENT.map(lambda value: f"{value}/child"),
    SAFE_COMPONENT.map(lambda value: f"{value}\\child"),
)
SAFE_SHELL_WORD = st.text(
    alphabet=st.characters(
        whitelist_categories=("Ll", "Lu", "Nd"),
        whitelist_characters="-_./:",
    ),
    min_size=1,
    max_size=16,
)
PATH_SEGMENT = st.text(
    alphabet=st.characters(
        whitelist_categories=("Ll", "Lu", "Nd"),
        whitelist_characters="-_",
    ),
    min_size=1,
    max_size=12,
)
WHITESPACE = st.text(alphabet=" \t\n\r", min_size=1, max_size=16)


@st.composite
def generated_cargo_wrapper_command(draw: st.DrawFn) -> tuple[str, str, list[str]]:
    """Generate Cargo wrapper commands and their expected argv split."""
    args = draw(st.lists(SAFE_WORD, min_size=1, max_size=4))
    if draw(st.booleans()):
        program = draw(SAFE_WORD)
    else:
        wrapper = draw(SAFE_WORD)
        program = rf"C:\Tools\{wrapper}.exe"
    cargo = " ".join([program, *args])
    return cargo, program, args


@st.composite
def safe_release_component(draw: st.DrawFn) -> str:
    """Generate non-empty release components without path syntax."""
    segments = draw(st.lists(PATH_SEGMENT, min_size=1, max_size=4))
    return ".".join(segments)


def write_manifest(repo: Path, version: str = "0.5.1") -> Path:
    """Write the minimal manifest needed by the packager."""
    manifest = repo / "Cargo.toml"
    manifest.write_text(f'[package]\nname = "pg-embed-setup-unpriv"\nversion = "{version}"\n')
    return manifest


def write_release_binary(
    repo: Path,
    target: str,
    name: str,
    content: str = "binary",
) -> Path:
    """Write a fake Cargo release binary for archive staging tests."""
    filename = f"{name}{release_archive.binary_extension(target)}"
    output = repo / "target" / target / "release" / filename
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(content)
    return output


def archive_members(archive: Path) -> list[str]:
    """Return archive member names in stored order."""
    with tarfile.open(archive, "r:gz") as tar:
        return tar.getnames()


def assert_cargo_program_and_args_preserved(
    cargo: str,
    expected_program: str,
    expected_args: list[str],
) -> None:
    """Assert that Cargo command parsing preserves the expected argv split."""
    parsed_program, parsed_args = release_archive._cargo_program_and_args(cargo)
    assert parsed_program == expected_program
    assert parsed_args == expected_args


def test_stage_archive_reports_missing_release_binary(tmp_path: Path) -> None:
    """Archive staging reports an expected binary missing from target output."""
    spec = release_archive.ReleaseArchiveSpec(
        repo=tmp_path,
        target="x86_64-unknown-linux-gnu",
        version="0.5.1",
        dist_dir=tmp_path / "dist",
        binaries=("pg_worker",),
    )
    expected_path = tmp_path / "target/x86_64-unknown-linux-gnu/release/pg_worker"

    with pytest.raises(
        FileNotFoundError,
        match=re.escape(f"release binary missing: {expected_path}"),
    ):
        release_archive.stage_archive(spec)


def test_archive_stem_rejects_path_like_version() -> None:
    """Archive stems reject versions that would change the archive path."""
    with pytest.raises(SystemExit, match=re.escape("version cannot contain path separators")):
        release_archive.archive_stem("x86_64-unknown-linux-gnu", "0.5/1")


def test_build_release_binaries_raises_system_exit_on_cargo_failure(
    tmp_path: Path,
) -> None:
    """Release builds surface non-zero Cargo exits through SystemExit."""
    spec = release_archive.ReleaseBuildSpec(
        repo=tmp_path,
        target="x86_64-unknown-linux-gnu",
        binaries=("pg_worker",),
        cargo="cargo",
        build_jobs=None,
    )

    with CmdMox() as mox:
        mox.mock("cargo").with_args(
            "build",
            "--release",
            "--target",
            "x86_64-unknown-linux-gnu",
            "--bin",
            "pg_worker",
        ).returns(exit_code=42)
        mox.replay()

        with pytest.raises(SystemExit) as exc_info:
            release_archive.build_release_binaries(spec)

    assert exc_info.value.code == 42


def test_main_uses_default_release_binaries(
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
) -> None:
    """The CLI builds and stages the default production binary set."""
    target = "x86_64-unknown-linux-gnu"
    manifest = write_manifest(tmp_path, version="0.5.1")
    for binary in release_archive.DEFAULT_BINARIES:
        write_release_binary(tmp_path, target, binary)

    with CmdMox() as mox:
        mox.mock("cargo").with_args(
            "build",
            "--release",
            "--target",
            target,
            "--bin",
            "pg_embedded_setup_unpriv",
            "--bin",
            "pg_worker",
        ).returns()
        mox.replay()

        release_archive.main(
            target,
            release_version="0.5.1",
            manifest_path=manifest,
            dist_dir=Path("dist"),
            cargo="cargo",
        )

    archive = Path(capsys.readouterr().out.strip())
    root = "pg-embed-setup-unpriv-x86_64-unknown-linux-gnu-v0.5.1"
    assert archive == tmp_path / "dist" / f"{root}.tgz"
    assert archive_members(archive) == [
        root,
        f"{root}/pg_embedded_setup_unpriv",
        f"{root}/pg_worker",
    ]


@settings(max_examples=150)
@given(
    target=safe_release_component(),
    binaries=st.lists(safe_release_component(), min_size=1, max_size=4).map(tuple),
)
def test_validate_release_spec_components_accepts_generated_safe_values(
    target: str,
    binaries: tuple[str, ...],
) -> None:
    """Release spec validation accepts generated non-path components."""
    release_archive.validate_release_spec_components(target, binaries)


@settings(max_examples=100)
@given(prefix=PATH_SEGMENT, suffix=PATH_SEGMENT)
def test_validate_release_spec_components_rejects_parent_dir_segments(
    prefix: str,
    suffix: str,
) -> None:
    """Release spec validation rejects generated parent-dir segments."""
    target = f"{prefix}/../{suffix}"

    with pytest.raises(SystemExit, match=re.escape("target cannot contain '..'")):
        release_archive.validate_release_spec_components(target, ("pg_worker",))


@settings(max_examples=100)
@given(
    prefix=PATH_SEGMENT,
    suffix=PATH_SEGMENT,
    separator=st.sampled_from(("/", "\\")),
)
def test_validate_release_spec_components_rejects_path_separators(
    prefix: str,
    suffix: str,
    separator: str,
) -> None:
    """Release spec validation rejects generated path separators."""
    binary = f"{prefix}{separator}{suffix}"

    with pytest.raises(
        SystemExit,
        match=re.escape("binary cannot contain path separators"),
    ):
        release_archive.validate_release_spec_components(
            "x86_64-unknown-linux-gnu",
            (binary,),
        )


def test_validate_release_spec_components_rejects_empty_strings() -> None:
    """Release spec validation always rejects empty components."""
    with pytest.raises(SystemExit, match=re.escape("target cannot be empty")):
        release_archive.validate_release_spec_components("", ("pg_worker",))
    with pytest.raises(SystemExit, match=re.escape("binary cannot be empty")):
        release_archive.validate_release_spec_components(
            "x86_64-unknown-linux-gnu",
            ("",),
        )


@settings(max_examples=150)
@given(value=PATH_LIKE_COMPONENT)
def test_validate_release_spec_components_rejects_generated_path_like_targets(
    value: str,
) -> None:
    """Release spec validation rejects generated path-like targets."""
    with pytest.raises(SystemExit):
        release_archive.validate_release_spec_components(value, ("pg_worker",))


@settings(max_examples=150)
@given(value=PATH_LIKE_COMPONENT)
def test_validate_release_spec_components_rejects_generated_path_like_binaries(
    value: str,
) -> None:
    """Release spec validation rejects generated path-like binary names."""
    with pytest.raises(SystemExit):
        release_archive.validate_release_spec_components(
            "x86_64-unknown-linux-gnu",
            (value,),
        )


@settings(max_examples=150)
@given(directory=SAFE_WORD, executable=SAFE_WORD)
def test_cargo_program_and_args_treats_generated_paths_as_executables(
    directory: str,
    executable: str,
) -> None:
    """Cargo command parsing keeps generated path-like executables intact."""
    cargo = f"/opt/{directory}/{executable}"

    parsed_program, parsed_args = release_archive._cargo_program_and_args(cargo)

    assert parsed_program == cargo
    assert parsed_args == []


@settings(max_examples=150)
@given(command=generated_cargo_wrapper_command())
def test_cargo_program_and_args_preserves_generated_wrapper_argv(
    command: tuple[str, str, list[str]],
) -> None:
    """Cargo command parsing preserves generated wrapper argv."""
    cargo, program, args = command

    assert_cargo_program_and_args_preserved(cargo, program, args)


@settings(max_examples=100)
@given(words=st.lists(SAFE_SHELL_WORD, min_size=1, max_size=5))
def test_split_cargo_command_roundtrips_shell_quoted_strings(words: list[str]) -> None:
    """Cargo command splitting follows shell quoting for generated argv."""
    command = " ".join(shlex.quote(word) for word in words)

    assert release_archive_cargo._split_cargo_command(command) == shlex.split(command)


@settings(max_examples=50)
@given(prefix=SAFE_SHELL_WORD)
def test_split_cargo_command_rejects_generated_malformed_quoting(prefix: str) -> None:
    """Cargo command splitting reports generated unbalanced quotes."""
    command = f"{prefix} 'unterminated"

    with pytest.raises(
        SystemExit,
        match=re.escape("invalid cargo executable command"),
    ):
        release_archive_cargo._split_cargo_command(command)


@settings(max_examples=50)
@given(command=WHITESPACE)
def test_cargo_program_and_args_rejects_generated_blank_input(command: str) -> None:
    """Cargo command parsing rejects generated blank input."""
    with pytest.raises(SystemExit, match=re.escape("cargo executable cannot be empty")):
        release_archive._cargo_program_and_args(command)
    with pytest.raises(SystemExit, match=re.escape("cargo executable cannot be empty")):
        release_archive_cargo._split_cargo_command(command)
