"""How the timeout contract reads a nextest filter's binary names.

A budget's scope is pinned by naming the binaries its filter selects, so
the reader that turns a filter into a set of names is what makes
``test_timeout_ordering_contract.py``'s cluster and serial assertions
mean anything. The reader can be wrong while every filter in this
repository is right, because both filters here happen to be flat
disjunctions of whole-binary terms: a reader that split on whitespace,
ignored duplicates, or matched a ``test(...)`` term as a binary would
give the same answer on today's file.

So the readings are driven with controlled filters: the shapes the
configuration writes, the shapes it must refuse rather than partly
match, and the embedded lock-test term the serial group carries beside
its binaries.

The readings themselves live in ``nextest_filters.py``.
"""

import pytest
from nextest_filters import (
    EMBEDDED_SERIAL_TERM,
    binary_names_in_filter,
    serial_group_binaries,
)
from timeout_budgets import NextestConfigurationError


@pytest.mark.parametrize(
    ("filter_text", "expected"),
    [
        pytest.param("binary(=ui)", frozenset({"ui"}), id="a-single-term"),
        pytest.param(
            "\n  binary(=ui)\n  | binary(=settings)\n",
            frozenset({"ui", "settings"}),
            id="multi-line-whitespace",
        ),
        pytest.param(
            "binary(=ui) | binary(=ui) | binary(=settings)",
            frozenset({"ui", "settings"}),
            id="duplicate-names-collapse",
        ),
        pytest.param(
            "binary(=test_cluster_fixture) | binary(=timezone_regression)",
            frozenset({"test_cluster_fixture", "timezone_regression"}),
            id="empty-and-underscore-names",
        ),
    ],
)
def test_a_binary_disjunction_reads_as_its_names(
    filter_text: str, expected: frozenset[str]
) -> None:
    """A flat disjunction is read whole, with duplicates collapsed.

    The multi-line case is the shape the configuration writes: a term per
    line, indented, with the ``|`` leading each continuation. Splitting on
    whitespace or on newlines instead would leave a leading ``binary(=``
    attached to the previous term and produce names nextest never sees.
    """
    assert binary_names_in_filter(filter_text) == expected


@pytest.mark.parametrize(
    "filter_text",
    [
        pytest.param("all()", id="all"),
        pytest.param("test(/^serial::/)", id="a-test-term"),
        pytest.param("binary(=ui) | test(/^serial::/)", id="a-mixed-disjunction"),
        pytest.param("binary(=ui) & binary(=settings)", id="an-intersection"),
        pytest.param("not binary(=ui)", id="a-negation"),
        pytest.param("binary(=ui) |", id="a-trailing-empty-term"),
        pytest.param("", id="an-empty-filter"),
        pytest.param("binary(~ui)", id="a-substring-term"),
    ],
)
def test_a_filter_that_is_not_a_pure_binary_disjunction_is_refused(
    filter_text: str,
) -> None:
    """Refusing unknown shapes is what keeps the scope pin meaningful.

    ``all()`` would widen a budget to every binary, and a mixed
    disjunction would hide a ``test(...)`` term behind names that look
    pinned. A reader that skipped or partially matched these would report
    a smaller, safer-looking set than the filter actually selects.
    """
    with pytest.raises(NextestConfigurationError):
        binary_names_in_filter(filter_text)


def test_the_serial_reader_separates_binaries_from_the_embedded_term() -> None:
    """The lock's own unit tests are a term, not a binary name.

    Reading ``kind(test) & test(/^serial::/)`` as a binary would put it in
    the pinned registry, and reading only the binaries would let the term
    be deleted while the group still looked complete.
    """
    filter_text = f"\n  binary(=ui)\n  | {EMBEDDED_SERIAL_TERM}\n"

    assert serial_group_binaries(filter_text) == frozenset({"ui"})


@pytest.mark.parametrize(
    "filter_text",
    [
        pytest.param("binary(=ui)", id="the-term-is-missing"),
        pytest.param(
            f"{EMBEDDED_SERIAL_TERM} | {EMBEDDED_SERIAL_TERM}",
            id="the-term-is-repeated",
        ),
    ],
)
def test_the_serial_reader_requires_the_embedded_term_once(
    filter_text: str,
) -> None:
    """A missing or repeated term is a different group from the pinned one.

    A missing term silently drops the embedded lock tests from the group,
    and a repeated one is a copy-paste that should be resolved rather
    than counted as scope.
    """
    with pytest.raises(NextestConfigurationError):
        serial_group_binaries(filter_text)