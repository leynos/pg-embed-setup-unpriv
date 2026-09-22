"""Drive each coverage-shape rule with the hazard it exists to catch.

The repository's own workflows satisfy every rule, so a rule that could
never fire passes the contract suite as well as a correct one does. Each
case here starts from a constructed repository the rules accept, applies
one hazard, and asserts that the rule meant to catch it names it. The
unmodified repository is asserted clean too, so a probe cannot pass
because the base was already rejected.
"""

from __future__ import annotations

import typing as typ

import pytest
from coverage_shape_rules import (
    codescene_contacts,
    publisher_faults,
    unratcheted_lanes,
)
from workflow_reader import Workflow

CI: typ.Final = """\
on: pull_request
jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: leynos/shared-actions/.github/actions/generate-coverage@abc
        with:
          with-ratchet: 'true'
          publish-artefact: 'false'
"""

PUBLISHER: typ.Final = """\
on:
  push:
    branches: [main]
  workflow_dispatch:
concurrency:
  group: coverage-upload
  cancel-in-progress: false
jobs:
  upload:
    runs-on: ubuntu-latest
    steps:
      - uses: leynos/shared-actions/.github/actions/generate-coverage@abc
      - name: Upload
        env:
          CS_ACCESS_TOKEN: ${{ secrets.CS_ACCESS_TOKEN }}
        if: ${{ github.ref == 'refs/heads/main' && env.CS_ACCESS_TOKEN != '' }}
        uses: leynos/shared-actions/.github/actions/upload-codescene-coverage@abc
        with:
          mode: upload
"""

#: A workflow that only answers calls, reaching the service with the
#: inherited token: the closure probe measured on episodic.
CALLEE: typ.Final = """\
on: workflow_call
jobs:
  probe:
    runs-on: ubuntu-latest
    steps:
      - run: |
          curl -H "Authorization: Bearer $T" https://api.codescene.io/v2/projects
        env:
          T: ${{ secrets.CS_ACCESS_TOKEN }}
"""

Rule = typ.Callable[[list[Workflow]], list[str]]


def repository(
    ci: str = CI, publisher: str = PUBLISHER, callee: str | None = None
) -> list[Workflow]:
    """Return a constructed repository's parsed workflows."""
    files = {"ci.yml": ci, "coverage-main.yml": publisher}
    if callee is not None:
        files["callee.yml"] = callee
    return [
        Workflow.parse(f".github/workflows/{name}", text)
        for name, text in files.items()
    ]


def with_job(job: str) -> str:
    """Return the pull-request workflow with one more job appended."""
    return CI + "\n".join(f"  {line}" for line in job.splitlines()) + "\n"


def with_step(step: str) -> str:
    """Return the pull-request workflow with one more step appended."""
    return CI + "\n".join(f"      {line}" for line in step.splitlines()) + "\n"


@pytest.mark.parametrize(
    "rule", [codescene_contacts, unratcheted_lanes, publisher_faults]
)
def test_the_base_repository_satisfies_every_rule(rule: Rule) -> None:
    """The unmodified base is clean, so each probe's failure is its own."""
    assert rule(repository()) == []


def test_a_workflow_nothing_calls_is_not_a_lane() -> None:
    """The closure follows calls, not files: an uncalled callee is clean."""
    assert codescene_contacts(repository(callee=CALLEE)) == []


CONTACTS: typ.Final = {
    "a called workflow, reached through ./": (
        with_job("probe:\n  uses: ./.github/workflows/callee.yml"),
        [
            "callee.yml: references the access token",
            "callee.yml: names the CodeScene host",
        ],
    ),
    "a called workflow, reached without ./": (
        with_job("probe:\n  uses: .github/workflows/callee.yml"),
        ["callee.yml: references the access token"],
    ),
    "the token in a run body, lower case": (
        with_step("- run: echo ${{ secrets.cs_access_token }}"),
        ["ci.yml: references the access token"],
    ),
    "the token as an action input": (
        with_step(
            "- uses: some/action@abc\n  with:\n    key: ${{ secrets.CS_ACCESS_TOKEN }}"
        ),
        ["ci.yml: references the access token"],
    ),
    "the token at workflow scope": (
        "env:\n  X: ${{ secrets.CS_ACCESS_TOKEN }}\n" + CI,
        ["ci.yml: references the access token"],
    ),
    "the token forwarded by name": (
        with_job(
            "call:\n  uses: o/r/.github/workflows/x.yml@abc\n  secrets:\n    K: ${{ secrets.CS_ACCESS_TOKEN }}"
        ),
        ["ci.yml: references the access token"],
    ),
    "every secret forwarded": (
        with_job("call:\n  uses: o/r/.github/workflows/x.yml@abc\n  secrets: inherit"),
        ["ci.yml:call: forwards every secret"],
    ),
    "the host, with no token or action": (
        with_step("- run: curl https://api.codescene.io/v2/projects"),
        ["ci.yml: names the CodeScene host"],
    ),
    "the CodeScene CLI": (
        with_step("- run: cs-coverage check lcov.info"),
        ["ci.yml: runs the CodeScene CLI"],
    ),
    "the uploader action": (
        with_step(
            "- uses: leynos/shared-actions/.github/actions/upload-codescene-coverage@abc"
        ),
        ["ci.yml:test: uses the uploader action"],
    ),
}


@pytest.mark.parametrize(("ci", "expected"), CONTACTS.values(), ids=CONTACTS.keys())
def test_every_route_to_codescene_is_named(ci: str, expected: list[str]) -> None:
    """Each way a pull-request lane could reach the service is reported."""
    found = codescene_contacts(repository(ci=ci, callee=CALLEE))
    missing = [
        fragment
        for fragment in expected
        if not any(fragment in offence for offence in found)
    ]
    assert not missing, f"{missing} not among {found}"


@pytest.mark.parametrize(
    ("old", "new", "expected"),
    [
        ("with-ratchet: 'true'", "with-ratchet: 'false'", "with-ratchet is false"),
        (
            "publish-artefact: 'false'",
            "publish-artefact: 'true'",
            "publish-artefact is true",
        ),
    ],
)
def test_a_lane_that_does_not_compare_locally_is_named(
    old: str, new: str, expected: str
) -> None:
    """Both inputs are read, and each is reported on its own."""
    found = unratcheted_lanes(repository(ci=CI.replace(old, new)))
    assert any(expected in offence for offence in found), found


GUARD: typ.Final = "github.ref == 'refs/heads/main' && env.CS_ACCESS_TOKEN != ''"

PUBLISHER_HAZARDS: typ.Final = {
    "a disjunction after the guard": (
        GUARD,
        GUARD + " || github.event_name == 'workflow_dispatch'",
        "has a disjunction",
    ),
    "the ref guard dropped": (GUARD, "env.CS_ACCESS_TOKEN != ''", "must require"),
    "the ref guard negated": (
        GUARD,
        "github.ref != 'refs/heads/main' && env.CS_ACCESS_TOKEN != ''",
        "must require",
    ),
    "cancelled in progress": (
        "cancel-in-progress: false",
        "cancel-in-progress: true",
        "cancels the publisher",
    ),
    "cancelled by expression": (
        "cancel-in-progress: false",
        "cancel-in-progress: ${{ true }}",
        "cancels the publisher",
    ),
    "no concurrency group": (
        "concurrency:\n  group: coverage-upload\n  cancel-in-progress: false\n",
        "",
        "no concurrency group",
    ),
    "a tag push rather than the trunk": (
        "branches: [main]",
        "tags: ['v*']",
        "does not run on push to main",
    ),
    "a push to every branch": (
        "  push:\n    branches: [main]\n",
        "  push:\n",
        "does not run on push to main",
    ),
    "the token moved to job scope": (
        "    runs-on: ubuntu-latest\n",
        "    runs-on: ubuntu-latest\n    env:\n      CS_ACCESS_TOKEN: x\n",
        "workflow or job scope",
    ),
    "the token moved off the upload step": (
        "      - name: Upload\n        env:\n          CS_ACCESS_TOKEN: ${{ secrets.CS_ACCESS_TOKEN }}\n",
        "      - name: Upload\n",
        "does not declare the token",
    ),
    "check mode": ("mode: upload", "mode: check", "uploads in check mode"),
    "a pull-request trigger": (
        "  workflow_dispatch:\n",
        "  workflow_dispatch:\n  pull_request:\n",
        "also answers a pull request",
    ),
}


@pytest.mark.parametrize(
    ("old", "new", "expected"), PUBLISHER_HAZARDS.values(), ids=PUBLISHER_HAZARDS.keys()
)
def test_every_publisher_hazard_is_named(old: str, new: str, expected: str) -> None:
    """Each way the trunk publisher can go wrong is reported."""
    assert old in PUBLISHER, f"the probe's anchor {old!r} is not in the base"
    found = publisher_faults(repository(publisher=PUBLISHER.replace(old, new)))
    assert any(expected in fault for fault in found), found


def test_a_second_publisher_is_named() -> None:
    """Two upload steps race to write one baseline."""
    found = publisher_faults(repository(callee=PUBLISHER))
    assert any("expected exactly one upload step" in fault for fault in found), found
