"""Hold the reader and the guard parser to their invariants over generated input.

The example suites name one hazard each. These properties cover the
ranges the readers quantify over: every local-call graph, every nesting
of scopes, and every guard built from conjuncts, some carrying operators
inside quoted literals. Each property is checked against an independent
statement of the answer rather than against the code's own steps.
"""

from __future__ import annotations

import typing as typ

from coverage_shape_rules import conjuncts
from hypothesis import given
from hypothesis import strategies as st
from workflow_reader import Workflow, pull_request_closure, scalars

#: The spellings GitHub accepts for a call into this repository.
LOCAL_PREFIXES: typ.Final = ("./", "$/", "")

Graph = tuple[int, list[bool], list[list[tuple[int, str]]]]


@st.composite
def call_graphs(draw: st.DrawFn) -> Graph:
    """Draw workflows with triggers and local calls, cycles allowed."""
    size = draw(st.integers(min_value=1, max_value=7))
    on_pull_request = draw(st.lists(st.booleans(), min_size=size, max_size=size))
    edges = draw(
        st.lists(
            st.lists(
                st.tuples(
                    st.integers(min_value=0, max_value=size - 1),
                    st.sampled_from(LOCAL_PREFIXES),
                ),
                max_size=3,
            ),
            min_size=size,
            max_size=size,
        )
    )
    return size, on_pull_request, edges


def _workflow(index: int, is_lane: bool, calls: list[tuple[int, str]]) -> Workflow:
    """Return workflow `index`, calling each target with its spelling."""
    trigger = "pull_request" if is_lane else "workflow_call"
    jobs = "".join(
        f"  j{n}:\n    uses: {prefix}.github/workflows/w{target}.yml\n"
        for n, (target, prefix) in enumerate(calls)
    )
    return Workflow.parse(
        f".github/workflows/w{index}.yml", f"on: {trigger}\njobs:\n{jobs or '  {}'}\n"
    )


def _reachable(
    on_pull_request: list[bool], edges: list[list[tuple[int, str]]]
) -> set[int]:
    """Return the indices reachable from a lane, by fixpoint over the edges."""
    reached = {index for index, is_lane in enumerate(on_pull_request) if is_lane}
    while True:
        grown = reached | {target for index in reached for target, _ in edges[index]}
        if grown == reached:
            return reached
        reached = grown


@given(call_graphs())
def test_the_closure_is_exactly_what_a_lane_reaches(graph: Graph) -> None:
    """Every spelling is followed, cycles terminate, and nothing else is added."""
    size, on_pull_request, edges = graph
    flows = [_workflow(i, on_pull_request[i], edges[i]) for i in range(size)]
    closure = [flow.path for flow in pull_request_closure(flows)]
    assert len(closure) == len(set(closure)), "a workflow was visited twice"
    expected = {
        f".github/workflows/w{i}.yml" for i in _reachable(on_pull_request, edges)
    }
    assert set(closure) == expected


#: Short keys and leaves; the traversal, not the text, is under test.
WORDS: typ.Final = st.text(alphabet="abcxyz_-", min_size=1, max_size=6)

trees = st.recursive(
    WORDS,
    lambda children: (
        st.lists(children, max_size=3) | st.dictionaries(WORDS, children, max_size=3)
    ),
    max_leaves=12,
)


def _leaves(node: object) -> list[str]:
    """Return every key and leaf of a tree, in an independent traversal."""
    found: list[str] = []
    pending = [node]
    while pending:
        item = pending.pop()
        if isinstance(item, dict):
            found += list(item)
            pending += list(item.values())
        elif isinstance(item, list):
            pending += item
        else:
            found.append(item)
    return found


@given(trees)
def test_scalars_reads_every_key_and_leaf(tree: object) -> None:
    """No key or value at any depth escapes the traversal the scans rely on."""
    assert sorted(scalars(tree)) == sorted(_leaves(tree))


#: A conjunct's own text, never containing a quote or an operator.
ATOMS: typ.Final = st.from_regex(r"[a-z.]{1,8} (==|!=) [a-z.]{1,8}", fullmatch=True)

#: A quoted literal that may carry either operator as text.
LITERALS: typ.Final = st.lists(
    st.sampled_from(["a", "  ", "&&", "||", "''", "main"]), max_size=5
).map(lambda parts: "'" + "".join(parts) + "'")

#: A conjunct, bare or comparing against a literal.
CONJUNCTS: typ.Final = ATOMS | st.builds(
    lambda atom, text: f"{atom} != {text}", ATOMS, LITERALS
)


@given(st.lists(CONJUNCTS, min_size=1, max_size=5))
def test_quoted_operators_are_text_and_bare_ones_split(parts: list[str]) -> None:
    """Joining with `&&` splits back into the parts; a bare `||` is refused.

    Literals come back byte for byte, including runs of spaces, so a
    compared value is never rewritten by the parser that reads it.
    """
    joined = " && ".join(parts)
    assert conjuncts(f"${{{{ {joined} }}}}") == parts
    assert conjuncts(f"{joined} || github.event_name == 'x'") is None
