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


def conjuncts(condition: object) -> list[str] | None:
    """Split an `if:` expression on `&&`, or return None if it has `||`.

    Quoted text is skipped, so an operator inside a string literal is
    not an operator. A disjunction makes every conjunct optional, so a
    condition carrying one is refused rather than read.

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


def may_run(job: dict[str, typ.Any], step: dict[str, typ.Any]) -> bool:
    """Return whether a step's and its job's conditions can both hold.

    Examples
    --------
    >>> job = {"strategy": {"matrix": {"privilege": ["root", "unprivileged"]}}}
    >>> may_run(job, {"if": "${{ matrix.privilege == 'unprivileged' }}"})
    True
    >>> may_run(job, {"if": False})
    False
    """
    values = matrix_values(job)
    return all(
        _satisfiable(condition, values)
        for condition in (job.get("if"), step.get("if"))
        if condition is not None
    )


def matrix_values(job: dict[str, typ.Any]) -> dict[str, set[str]]:
    """Return every value each matrix key takes, including `include` rows.

    Examples
    --------
    >>> matrix_values({"strategy": {"matrix": {"a": [1], "include": [{"b": "x"}]}}})
    {'a': {'1'}, 'b': {'x'}}
    """
    matrix = (job.get("strategy") or {}).get("matrix") or {}
    values: dict[str, set[str]] = {}
    rows = [
        {key: value}
        for key, listed in matrix.items()
        if key not in {"include", "exclude"} and isinstance(listed, list)
        for value in listed
    ] + list(matrix.get("include") or [])
    for row in rows:
        for key, value in row.items():
            values.setdefault(str(key), set()).add(str(value))
    return values


def _satisfiable(condition: object, values: dict[str, set[str]]) -> bool:
    """Return whether every conjunct of a condition is one that can hold."""
    parts = conjuncts(condition) if condition is not True else []
    return parts is not None and all(_holds(part, values) for part in parts)


def _holds(part: str, values: dict[str, set[str]]) -> bool:
    """Return whether one conjunct is recognized as satisfiable."""
    match = MATRIX_EQUALS.fullmatch(part)
    if match is not None:
        return match.group(2) in values.get(match.group(1), set())
    return part in RUNNING_STATUS or part in PULL_REQUEST_EVENT
