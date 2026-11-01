"""Properties the filter reader holds for filters no file writes down.

``test_nextest_filters_contract.py`` pins the exact filters the repository
holds, which is the right check for scope but cannot notice a reader that
is wrong about a name nobody has chosen yet. These generate the rest: a
name using every permitted character, a term repeated to state scope, a
term padded with whitespace, and a term that is not ``binary(=name)`` at
all. The expected result is built in the test rather than imported, so a
reader and a contract that drifted together would still fail here.
"""

import typing as typ

import pytest
from hypothesis import given
from hypothesis import strategies as st
from nextest_filters import (
    EMBEDDED_SERIAL_TERM,
    binary_names_in_filter,
    serial_group_binaries,
)
from timeout_budgets import NextestConfigurationError

#: A name in the charset Cargo permits in a target name.
_BINARY_NAME: typ.Final[st.SearchStrategy[str]] = st.from_regex(
    r"[A-Za-z0-9_.-]{1,40}", fullmatch=True
)

#: Terms this reader must refuse rather than interpret.
_NON_BINARY_TERM: typ.Final[st.SearchStrategy[str]] = st.sampled_from(
    ["all()", "test(foo)", "binary(=a) & binary(=b)", "!binary(=x)"]
)

#: Whitespace an author might leave around a term.
_PADDING: typ.Final[st.SearchStrategy[str]] = st.sampled_from(
    ["", " ", "  ", "\t"]
)


def _terms(names: list[str]) -> list[str]:
    """Render each name as a ``binary(=name)`` term."""
    return [f"binary(={name})" for name in names]


@given(names=st.lists(_BINARY_NAME, min_size=1, max_size=8), padding=_PADDING)
def test_a_disjunction_reads_as_exactly_the_names_it_terminates(
    names: list[str], padding: str
) -> None:
    """Every generated name round-trips, and nothing else is reported.

    The name charset includes ``.``, ``_`` and ``-``, which no file under
    version control happens to exercise, and padding around a term is
    stripped rather than folded into the name.
    """
    filter_text = " | ".join(
        f"{padding}{term}{padding}" for term in _terms(names)
    )
    assert binary_names_in_filter(filter_text) == frozenset(names)


@given(name=_BINARY_NAME, repeats=st.integers(min_value=1, max_value=6))
def test_a_repeated_term_states_scope_rather_than_multiplicity(
    name: str, repeats: int
) -> None:
    """A reader that counted terms would report the same name once anyway.

    Repeating a name must not make it a different scope, whether the
    reader collects a set or compares a list.
    """
    filter_text = "|".join(_terms([name] * repeats))
    assert binary_names_in_filter(filter_text) == frozenset({name})


@given(
    names=st.lists(_BINARY_NAME, min_size=0, max_size=4),
    decoy=_NON_BINARY_TERM,
    position=st.integers(min_value=0, max_value=4),
)
def test_a_non_binary_term_anywhere_in_the_disjunction_is_refused(
    names: list[str], decoy: str, position: int
) -> None:
    """The reader checks every term, not only the first or the last.

    A decoy at either end distinguishes a reader that validates only the
    first term from one that validates the whole disjunction.
    """
    terms = _terms(names)
    terms.insert(min(position, len(terms)), decoy)
    with pytest.raises(NextestConfigurationError):
        binary_names_in_filter(" | ".join(terms))


@given(
    names=st.lists(_BINARY_NAME, min_size=0, max_size=6),
    position=st.integers(min_value=0, max_value=6),
)
def test_the_serial_reader_ignores_where_the_embedded_term_sits(
    names: list[str], position: int
) -> None:
    """The embedded term is subtracted wherever the author placed it.

    The repository writes it last, so a reader that only looked at the
    final term would pass every file here while rejecting a reordering.
    """
    terms = _terms(names)
    terms.insert(min(position, len(terms)), EMBEDDED_SERIAL_TERM)
    assert serial_group_binaries(" | ".join(terms)) == frozenset(names)


@given(
    names=st.lists(_BINARY_NAME, min_size=0, max_size=6),
    copies=st.integers(min_value=0, max_value=3),
)
def test_the_serial_reader_requires_the_embedded_term_exactly_once(
    names: list[str], copies: int
) -> None:
    """Zero or several copies are refused; exactly one is accepted.

    An absent term would leave the lock's own unit tests outside the
    group, and a duplicated one is a sign the filter was edited without
    understanding it, so both are configuration errors.
    """
    terms = _terms(names) + [EMBEDDED_SERIAL_TERM] * copies
    filter_text = " | ".join(terms)
    if copies == 1:
        assert serial_group_binaries(filter_text) == frozenset(names)
        return
    with pytest.raises(NextestConfigurationError):
        serial_group_binaries(filter_text)