"""Read GitHub Actions `if:` conditions as far as the contract needs to.

The contract asks two questions of a condition: whether it confines a
step (the publisher's guard) and whether it lets a step run at all (the
pull-request coverage step). Neither needs a full expression evaluator.
`conjuncts` splits a condition on `&&` and refuses a disjunction, and
`may_run` accepts only conjuncts it can show are satisfiable, so an
expression it does not understand reads as "may never run" and fails
loudly rather than passing.
"""

from __future__ import annotations

import itertools
import re
import typing as typ

#: A single-quoted expression literal, where `''` escapes a quote.
QUOTED: typ.Final = re.compile(r"'(?:[^']|'')*'")

#: What a literal is replaced by while the operators are read.
MASK: typ.Final = "\x00"
QUOTED_MASK: typ.Final = re.compile(MASK)

#: Status functions that hold on an ordinary, successful run.
RUNNING_STATUS: typ.Final = frozenset({"success()", "always()", "!cancelled()"})

#: A comparison of one matrix key with one literal.
MATRIX_EQUALS: typ.Final = re.compile(r"matrix\.([\w-]+) == '([^']*)'")

#: The event comparisons a pull-request lane satisfies.
PULL_REQUEST_EVENT: typ.Final = frozenset(
    {
        "github.event_name == 'pull_request'",
        "github.event_name == 'pull_request_target'",
    }
)

#: What holds when the trunk publisher runs on a push to `main`.
TRUNK_CONTEXT: typ.Final = frozenset(
    {"github.ref == 'refs/heads/main'", "github.event_name == 'push'"}
)

Row = dict[str, str]


def conjuncts(condition: object) -> list[str] | None:
    """Split an `if:` expression on `&&`, or return None if it has `||`.

    Quoted text is skipped, so an operator inside a string literal is
    not an operator. A disjunction makes every conjunct optional, so a
    condition carrying one is refused rather than read.

    Parameters
    ----------
    condition : object
        The `if:` value as parsed, with or without its `${{ }}` wrapper.

    Returns
    -------
    list[str] or None
        The conjuncts with whitespace normalized outside quoted literals,
        or None when the condition carries an unquoted `||`.

    Examples
    --------
    >>> conjuncts("${{ github.ref == 'refs/heads/main' && env.T != '' }}")
    ["github.ref == 'refs/heads/main'", "env.T != ''"]
    >>> conjuncts("a && b || c") is None
    True
    >>> conjuncts("x == 'a || b' && y")
    ["x == 'a || b'", 'y']
    """
    text = str(condition).strip()
    if text.startswith("${{") and text.endswith("}}"):
        text = text[3:-2]
    literals = iter(QUOTED.findall(text))
    masked = QUOTED.sub(MASK, text)
    if "||" in masked:
        return None
    # Whitespace is normalized before the literals come back, so their own
    # text is untouched; they come back in order, so one iterator serves.
    return [
        QUOTED_MASK.sub(lambda _: next(literals), " ".join(part.split()))
        for part in masked.split("&&")
    ]


def may_run(
    job: dict[str, typ.Any],
    step: dict[str, typ.Any],
    context: frozenset[str] = PULL_REQUEST_EVENT,
) -> bool:
    """Return whether a step's and its job's conditions can hold together.

    Every matrix comparison must hold in one and the same matrix leg, and
    every other conjunct must be a running status or hold in `context`.

    Parameters
    ----------
    job : dict
        The parsed job, whose `if:` and matrix apply to the step.
    step : dict
        The parsed step.
    context : frozenset[str]
        Conjuncts that hold in the run being asked about: a pull-request
        event by default, or `TRUNK_CONTEXT` for the publisher.

    Returns
    -------
    bool
        True when some matrix leg satisfies every conjunct of both
        conditions.

    Examples
    --------
    >>> job = {"strategy": {"matrix": {"privilege": ["root", "unprivileged"]}}}
    >>> may_run(job, {"if": "${{ matrix.privilege == 'unprivileged' }}"})
    True
    >>> may_run(job, {"if": False})
    False
    """
    parts = _conjuncts_of(job.get("if"), step.get("if"))
    if parts is None:
        return False
    pairs = [match.groups() for match in map(MATRIX_EQUALS.fullmatch, parts) if match]
    others = [part for part in parts if MATRIX_EQUALS.fullmatch(part) is None]
    if not all(part in RUNNING_STATUS or part in context for part in others):
        return False
    return any(
        all(row.get(key) == value for key, value in pairs) for row in matrix_rows(job)
    )


def _conjuncts_of(*conditions: object) -> list[str] | None:
    """Return the conjuncts of several conditions, or None if any has `||`."""
    parts: list[str] = []
    for condition in conditions:
        if condition is None or condition is True:
            continue
        found = conjuncts(condition)
        if found is None:
            return None
        parts += found
    return parts


def matrix_rows(job: dict[str, typ.Any]) -> list[Row]:
    """Return the job's matrix legs as GitHub expands them.

    The axes are crossed, `exclude` removes the legs it matches, and each
    `include` object extends every leg whose original values it does not
    overwrite, or becomes a leg of its own when it extends none.

    Parameters
    ----------
    job : dict
        The parsed job.

    Returns
    -------
    list[dict[str, str]]
        One mapping per leg, values as text. A job with no matrix has one
        empty leg.

    Examples
    --------
    >>> matrix = {"os": ["linux"], "include": [{"os": "linux", "x": 1}, {"os": "mac"}]}
    >>> matrix_rows({"strategy": {"matrix": matrix}})
    [{'os': 'linux', 'x': '1'}, {'os': 'mac'}]
    """
    matrix = (job.get("strategy") or {}).get("matrix")
    if not isinstance(matrix, dict):
        return [{}]
    axes = {
        str(key): [str(value) for value in values]
        for key, values in matrix.items()
        if key not in {"include", "exclude"} and isinstance(values, list)
    }
    excluded = [_as_row(entry) for entry in matrix.get("exclude") or []]
    legs = [
        leg
        for leg in (
            dict(zip(axes, combo)) for combo in itertools.product(*axes.values())
        )
        if not any(_matches(leg, entry) for entry in excluded)
    ]
    for entry in (_as_row(item) for item in matrix.get("include") or []):
        _include(legs, entry, set(axes))
    return legs or [{}]


def _as_row(entry: object) -> Row:
    """Return a matrix `include` or `exclude` object with text values."""
    return {str(key): str(value) for key, value in dict(entry).items()}


def _matches(leg: Row, entry: Row) -> bool:
    """Return whether a leg carries every value an entry names."""
    return all(leg.get(key) == value for key, value in entry.items())


def _include(legs: list[Row], entry: Row, original: set[str]) -> None:
    """Extend the legs one `include` object fits, or add it as a leg."""
    fits = [
        leg
        for leg in legs
        if original
        and all(
            leg.get(key) == value for key, value in entry.items() if key in original
        )
    ]
    for leg in fits:
        leg.update(entry)
    if not fits:
        legs.append(dict(entry))
