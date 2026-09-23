"""Hold the workflow reader to what GitHub runs, with constructed files.

The repository's own workflows cannot prove the reader: they use one
trigger spelling, declare no key twice and call no local workflow, so a
reader that mishandled any of those would still read them correctly.
Each case here builds the document the hazard needs and parses it with
the same strict, resolving loader the contract uses.
"""

from __future__ import annotations

from pathlib import Path

import pytest
import yaml
from workflow_reader import Workflow, load_workflows, pull_request_closure

TRIGGER_SPELLINGS = [
    "on: pull_request",
    "'on': pull_request",
    "on: [push, pull_request]",
    "'on': [push, pull_request]",
    "on:\n  pull_request:\n    branches: [main]",
    "'on':\n  pull_request:",
]


@pytest.mark.parametrize("trigger", TRIGGER_SPELLINGS)
def test_every_trigger_spelling_reads_as_a_pull_request_lane(trigger: str) -> None:
    """Scalar, sequence and mapping, under the bare and the quoted key.

    The bare key resolves to the boolean `True` and the quoted one stays
    a string; a sequence read as a mapping would become one key named
    after the whole list.
    """
    flow = Workflow.parse("x.yml", f"{trigger}\njobs: {{}}\n")
    assert flow.on_pull_request, flow.triggers


@pytest.mark.parametrize(
    "text",
    [
        "on: push\n'on': pull_request\njobs: {}\n",
        "on: 3\njobs: {}\n",
        "jobs: {}\n",
    ],
    ids=["both keys", "a number", "no trigger"],
)
def test_an_unreadable_trigger_is_refused(text: str) -> None:
    """A trigger the reader cannot classify fails rather than reads as none."""
    with pytest.raises(ValueError, match="trigger"):
        Workflow.parse("x.yml", text)


def test_a_key_declared_twice_is_refused() -> None:
    """The first `runs-on` would otherwise be discarded in silence."""
    text = (
        "on: push\njobs:\n  a:\n    runs-on: ubicloud-standard-2\n"
        "    runs-on: ubuntu-latest\n"
    )
    with pytest.raises(yaml.constructor.ConstructorError, match="duplicate key"):
        Workflow.parse("x.yml", text)


@pytest.mark.parametrize(
    ("text", "error"),
    [("on: [push\n", yaml.YAMLError), ("- on: push\n", TypeError)],
    ids=["not YAML", "not a mapping"],
)
def test_a_file_that_is_not_a_workflow_is_refused(
    text: str, error: type[Exception]
) -> None:
    """An unparseable file fails loudly rather than drop out of every rule."""
    with pytest.raises(error):
        Workflow.parse("x.yml", text)


def test_an_upper_case_suffix_is_loaded(tmp_path: Path) -> None:
    """GitHub loads `.YML`; a case-sensitive glob would skip it."""
    directory = tmp_path / ".github" / "workflows"
    directory.mkdir(parents=True)
    (directory / "LANE.YML").write_text("on: pull_request\njobs: {}\n")
    (directory / "notes.txt").write_text("not a workflow\n")
    assert [flow.path for flow in load_workflows(tmp_path)] == [
        ".github/workflows/LANE.YML"
    ]


def call(path: str, target: str, trigger: str = "workflow_call") -> Workflow:
    """Return a workflow whose one job calls `target`."""
    return Workflow.parse(path, f"on: {trigger}\njobs: {{x: {{uses: {target}}}}}\n")


def test_the_closure_follows_calls_transitively_and_stops_on_a_cycle() -> None:
    """A lane two calls away is reached, and a cycle does not hang."""
    flows = [
        call(".github/workflows/a.yml", "./.github/workflows/b.yml", "pull_request"),
        call(".github/workflows/b.yml", "$/.github/workflows/c.yml"),
        call(".github/workflows/c.yml", ".github/workflows/b.yml"),
        call(".github/workflows/d.yml", "./.github/workflows/a.yml"),
    ]
    reached = [flow.path for flow in pull_request_closure(flows)]
    assert reached == [f".github/workflows/{name}.yml" for name in "abc"]


def test_a_call_to_a_missing_workflow_is_refused() -> None:
    """The closure cannot stop at a file it failed to read."""
    flows = [
        call(".github/workflows/a.yml", "./.github/workflows/gone.yml", "pull_request")
    ]
    with pytest.raises(AssertionError, match="gone.yml"):
        pull_request_closure(flows)


def test_a_remote_workflow_is_not_a_local_call() -> None:
    """A pinned reference to another repository's workflow is not followed."""
    flow = call(
        ".github/workflows/a.yml",
        "leynos/shared-actions/.github/workflows/x.yml@abc",
        "pull_request",
    )
    assert flow.callees() == []
