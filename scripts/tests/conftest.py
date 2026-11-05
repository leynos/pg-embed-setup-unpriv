"""Fixtures shared by the release-tooling contract suites.

Each module that compares `.config/nextest.toml` with a contract asks
for the module-scoped `nextest_config` fixture, so the file is read once
per test module rather than once per test. Reading it here keeps the
path, the encoding and the module scope in one place: a suite receives
the same UTF-8 text every other suite compares, and a change to where
the configuration lives is made once.
"""

import pytest
from timeout_budgets import NEXTEST_CONFIG


@pytest.fixture(scope="module")
def nextest_config() -> str:
    """Return the nextest configuration file's text.

    Returns
    -------
    str
        The file's contents.
    """
    return NEXTEST_CONFIG.read_text(encoding="utf-8")