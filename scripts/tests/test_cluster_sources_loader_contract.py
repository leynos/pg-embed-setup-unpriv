"""Contract tests for the loader's failure reporting.

``load_test_sources`` promises to turn a filesystem or decoding failure
into ``SourceLoadError`` carrying the failing path and the underlying
cause. The scanner's callers rely on that promise to report which file
stopped the scan, so both failure modes are pinned here rather than left
to the happy-path tests.
"""

import os
from pathlib import Path

import pytest

from cluster_sources import SourceLoadError, load_test_sources


@pytest.mark.skipif(
    os.name == "nt", reason="creating a symlink needs a privilege on Windows"
)
def test_an_unreadable_source_reports_its_path_and_cause(tmp_path: Path) -> None:
    """A dangling symlink is a source the loader cannot read.

    Permission bits are not portable: a suite running as root can read a
    mode-0 file. A symlink to a path that does not exist fails for every
    user, so the read path is exercised deterministically.
    """
    unreadable = tmp_path / "unreadable.rs"
    unreadable.symlink_to(tmp_path / "missing.rs")

    with pytest.raises(SourceLoadError) as raised:
        load_test_sources(tmp_path)

    assert raised.value.path.resolve() == unreadable.resolve()
    assert isinstance(raised.value.cause, OSError)
    assert str(unreadable) in str(raised.value)


@pytest.mark.skipif(
    os.name == "nt" or os.geteuid() == 0,
    reason="directory permissions do not deny access to this user",
)
def test_a_directory_that_cannot_be_listed_is_reported(tmp_path: Path) -> None:
    """An unlistable directory stops the load instead of being skipped.

    ``Path.rglob`` would suppress the scan failure and return the sources
    it managed to read, hiding a binary that takes the lock inside the
    skipped directory.
    """
    blocked = tmp_path / "blocked"
    blocked.mkdir()
    (blocked / "hidden.rs").write_text("// hidden\n", encoding="utf-8")
    blocked.chmod(0)
    try:
        with pytest.raises(SourceLoadError) as raised:
            load_test_sources(tmp_path)
    finally:
        blocked.chmod(0o755)

    assert raised.value.path.resolve() == blocked.resolve()
    assert isinstance(raised.value.cause, OSError)


def test_an_invalid_utf8_source_reports_its_path_and_cause(tmp_path: Path) -> None:
    """Rust sources are UTF-8, so a decode failure is a load failure."""
    invalid = tmp_path / "invalid.rs"
    invalid.write_bytes(b"\xff\xfe not utf-8")

    with pytest.raises(SourceLoadError) as raised:
        load_test_sources(tmp_path)

    assert raised.value.path.resolve() == invalid.resolve()
    assert isinstance(raised.value.cause, UnicodeError)
    assert str(invalid) in str(raised.value)
