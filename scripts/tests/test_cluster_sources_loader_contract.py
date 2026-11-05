"""Contract tests for the loader's failure reporting.

``load_test_sources`` promises to turn a filesystem or decoding failure
into ``SourceLoadError`` carrying the failing path and the underlying
cause. The scanner's callers rely on that promise to report which file
stopped the scan, so both failure modes are pinned here rather than left
to the happy-path tests.
"""

from pathlib import Path

import pytest

from cluster_sources import SourceLoadError, load_test_sources


def test_an_unreadable_source_reports_its_path_and_cause(tmp_path: Path) -> None:
    """A directory named like a source is the portable unreadable case.

    Permission bits are not portable: a suite running as root can read a
    mode-0 file. A directory matched by ``rglob("*.rs")`` fails to read
    for every user, so the error path is exercised deterministically.
    """
    unreadable = tmp_path / "unreadable.rs"
    unreadable.mkdir()

    with pytest.raises(SourceLoadError) as raised:
        load_test_sources(tmp_path)

    assert raised.value.path.resolve() == unreadable.resolve()
    assert isinstance(raised.value.cause, OSError)
    assert str(unreadable) in str(raised.value)


def test_an_invalid_utf8_source_reports_its_path_and_cause(tmp_path: Path) -> None:
    """Rust sources are UTF-8, so a decode failure is a load failure."""
    invalid = tmp_path / "invalid.rs"
    invalid.write_bytes(b"\xff\xfe not utf-8")

    with pytest.raises(SourceLoadError) as raised:
        load_test_sources(tmp_path)

    assert raised.value.path.resolve() == invalid.resolve()
    assert isinstance(raised.value.cause, UnicodeError)
    assert str(invalid) in str(raised.value)
