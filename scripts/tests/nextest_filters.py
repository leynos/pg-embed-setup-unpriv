"""Reading the binaries and terms an override's filter names.

The timeout contract pins scope by structure, not by substring. A reader
that searched a filter for a binary name would accept ``binary(=ui_extra)``
as ``ui``, and one that only counted terms would not notice two overrides
giving the same binary two budgets. This module reads the flat
disjunctions the repository writes and refuses every other shape.

It parses text structure rather than nextest filter semantics: terms are
split on ``|``, stripped, and matched whole against ``binary(=name)``.
Precedence, nesting and any expression the repository does not write are
reported as unreadable rather than guessed at.

The duration and budget readings live in ``nextest_budgets``; this
module stays separate so neither outgrows the 400-line limit
``AGENTS.md`` sets.
"""

import re
import typing as typ

from timeout_budgets import NextestConfigurationError

#: A single ``binary(=name)`` filterset term. The name charset is the one
#: Cargo permits in a target name; the reader never interprets the name.
BINARY_TERM_PATTERN: typ.Final[re.Pattern[str]] = re.compile(
    r"binary\(=([A-Za-z0-9_.-]+)\)"
)

#: The term that adds the scenario lock's own unit tests, embedded in every
#: integration binary as ``serial::tests::*`` and ``serial::non_unix::tests::*``.
EMBEDDED_SERIAL_TERM: typ.Final[str] = "kind(test) & test(/^serial::/)"


def _disjunction_terms(filter_text: str, *, path: str) -> list[str]:
    """Split a filterset disjunction into its non-empty terms."""
    # The split is structural rather than semantic: it knows nothing of
    # precedence or nesting, because the filters this contract reads are
    # flat disjunctions and every other shape is refused rather than
    # guessed at.
    terms = [term.strip() for term in filter_text.split("|")]
    if any(not term for term in terms):
        message = (
            f"{path}.filter is not a disjunction of non-empty terms: "
            f"{filter_text!r}"
        )
        raise NextestConfigurationError(
            message, field="filter", value=filter_text
        )
    return terms


def _binary_name(term: str, *, path: str) -> str:
    """Return the binary name one ``binary(=name)`` term names, or refuse it."""
    match = BINARY_TERM_PATTERN.fullmatch(term)
    if match is None:
        message = (
            f"{path}.filter term {term!r} is not a binary(=name) term; this "
            f"reader accepts only a disjunction of binary terms"
        )
        raise NextestConfigurationError(message, field="filter", value=term)
    return match.group(1)


def binary_names_in_filter(
    filter_text: str, *, path: str = "filter"
) -> frozenset[str]:
    """Return the binary names a pure disjunction of binary terms selects.

    Duplicate names collapse, so the result states scope rather than the
    number of times an author wrote a term. Every other shape is refused:
    a single term is allowed, but ``all()``, a ``test(...)`` term, an
    intersection such as ``binary(=a) & binary(=b)``, and any mixture of
    binary and non-binary terms are not.

    The reading is structural. A parenthesized expression, a negated term
    or a filter whose ``|`` sits inside another construct is refused
    rather than interpreted, because this contract is checking that the
    configuration names exact binaries, not reimplementing nextest.

    Parameters
    ----------
    filter_text : str
        The ``filter`` string from an override.
    path : str
        The dotted path of the override, for the message.

    Returns
    -------
    frozenset[str]
        The binary names the filter selects.

    Raises
    ------
    NextestConfigurationError
        If any term is not a ``binary(=name)`` term.

    Examples
    --------
    >>> binary_names_in_filter("binary(=ui) | binary(=settings)") == frozenset(
    ...     {"ui", "settings"}
    ... )
    True
    >>> binary_names_in_filter("binary(=ui)") == frozenset({"ui"})
    True
    >>> try:
    ...     binary_names_in_filter("all()")
    ... except NextestConfigurationError:
    ...     print("rejected")
    rejected
    """
    return frozenset(
        _binary_name(term, path=path)
        for term in _disjunction_terms(filter_text, path=path)
    )


def serial_group_binaries(
    filter_text: str, *, path: str = "filter"
) -> frozenset[str]:
    """Return the whole-binary members of the serial group filter.

    The serial filter carries both direct-consumer binaries and the exact
    term that selects the scenario lock's embedded unit tests. This
    reader requires that term once, and reads every other term as
    ``binary(=name)``.

    Parameters
    ----------
    filter_text : str
        The ``filter`` string from the serial override.
    path : str
        The dotted path of the override, for the message.

    Returns
    -------
    frozenset[str]
        The whole-binary names in the group.

    Raises
    ------
    NextestConfigurationError
        If the embedded term is missing or repeated, or if any other term
        is not a ``binary(=name)`` term.
    """
    terms = _disjunction_terms(filter_text, path=path)
    embedded = [term for term in terms if term == EMBEDDED_SERIAL_TERM]
    if len(embedded) != 1:
        message = (
            f"{path}.filter must carry {EMBEDDED_SERIAL_TERM!r} exactly once; "
            f"found it {len(embedded)} time(s)"
        )
        raise NextestConfigurationError(
            message, field="filter", value=filter_text
        )
    return frozenset(
        _binary_name(term, path=path)
        for term in terms
        if term != EMBEDDED_SERIAL_TERM
    )