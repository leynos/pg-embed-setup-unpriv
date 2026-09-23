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
  group: ${{ github.workflow }}-${{ github.ref }}
  cancel-in-progress: false
jobs:
  upload:
    runs-on: ubuntu-latest
    steps:
      - uses: leynos/shared-actions/.github/actions/generate-coverage@abc
      - name: Check
        id: codescene-token
        run: echo "available=${{ secrets.CS_ACCESS_TOKEN != '' }}" >> "$GITHUB_OUTPUT"
      - name: Upload
        if: ${{ steps.codescene-token.outputs.available == 'true' && github.ref == 'refs/heads/main' }}
        uses: leynos/shared-actions/.github/actions/upload-codescene-coverage@abc
        with:
          mode: upload
          access-token: ${{ secrets.CS_ACCESS_TOKEN }}
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

#: A callee that only declares the token among the secrets it accepts.
DECLARES: typ.Final = """\
on:
  workflow_call:
    secrets:
      CS_ACCESS_TOKEN:
        required: false
jobs:
  noop:
    runs-on: ubuntu-latest
    steps:
      - run: true
"""

Rule = typ.Callable[[list[Workflow]], list[str]]


def repository(
    ci: str = CI,
    publisher: str = PUBLISHER,
    callee: str | None = None,
    extra: dict[str, str] | None = None,
) -> list[Workflow]:
    """Return a constructed repository's parsed workflows."""
    files = {"ci.yml": ci, "coverage-main.yml": publisher, **(extra or {})}
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


AVAILABLE: typ.Final = "steps.codescene-token.outputs.available == 'true'"
TRUNK: typ.Final = "github.ref == 'refs/heads/main'"
GUARD: typ.Final = f"{AVAILABLE} && {TRUNK}"

#: The token check step, as the base publisher writes it.
CHECK: typ.Final = (
    "      - name: Check\n"
    "        id: codescene-token\n"
    "        run: echo \"available=${{ secrets.CS_ACCESS_TOKEN != '' }}\""
    ' >> "$GITHUB_OUTPUT"\n'
)

PUBLISHER_HAZARDS: typ.Final = {
    "a disjunction after the guard": (
        GUARD,
        GUARD + " || github.event_name == 'workflow_dispatch'",
        "has a disjunction",
    ),
    # Every required conjunct stays whole here, so only the refusal of a bare
    # `||` catches it: the disjunction hides in an extra, narrowing conjunct.
    "a disjunction inside an extra conjunct": (
        GUARD,
        GUARD + " && github.actor != 'x' || github.event_name == 'workflow_dispatch'",
        "has a disjunction",
    ),
    "the ref guard dropped": (GUARD, AVAILABLE, "must require"),
    "the ref guard negated": (
        GUARD,
        f"{AVAILABLE} && github.ref != 'refs/heads/main'",
        "must require",
    ),
    "the token guard reversed": (
        GUARD,
        f"steps.codescene-token.outputs.available == 'false' && {TRUNK}",
        "must require",
    ),
    "the token guard negated": (
        GUARD,
        f"steps.codescene-token.outputs.available != 'true' && {TRUNK}",
        "must require",
    ),
    "the guard reads another step's output": (
        GUARD,
        f"steps.other.outputs.available == 'true' && {TRUNK}",
        "must require",
    ),
    "the concurrency group dropped": (
        "  group: ${{ github.workflow }}-${{ github.ref }}\n",
        "",
        "no concurrency group",
    ),
    "the concurrency group keyed on the event": (
        "  group: ${{ github.workflow }}-${{ github.ref }}\n",
        "  group: ${{ github.workflow }}-${{ github.ref }}-${{ github.event_name }}\n",
        "is not the workflow and ref",
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
        (
            "concurrency:\n  group: ${{ github.workflow }}-${{ github.ref }}\n"
            "  cancel-in-progress: false\n"
        ),
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
    "the token bound at job scope": (
        "    runs-on: ubuntu-latest\n",
        "    runs-on: ubuntu-latest\n    env:\n      CS_ACCESS_TOKEN: ${{ secrets.CS_ACCESS_TOKEN }}\n",
        "the token is bound in an env",
    ),
    "the token bound in the upload step's env": (
        "      - name: Upload\n",
        "      - name: Upload\n        env:\n          CS_ACCESS_TOKEN: ${{ secrets.CS_ACCESS_TOKEN }}\n",
        "the token is bound in an env",
    ),
    "the token check deleted": (CHECK, "", "runs no single token check"),
    "the token check's command changed": (
        "!= '' }}",
        "!= '' || true }}",
        "runs no single token check",
    ),
    "the token check made conditional": (
        "        id: codescene-token\n",
        "        id: codescene-token\n        if: always()\n",
        "the token check is conditional",
    ),
    "the token check given an env": (
        "        id: codescene-token\n",
        "        id: codescene-token\n        env:\n          A: b\n",
        "the token check declares an env",
    ),
    "the token check given no id": (
        "        id: codescene-token\n",
        "",
        "the token check has no id",
    ),
    "another step naming the token": (
        "      - uses: leynos/shared-actions/.github/actions/generate-coverage@abc\n",
        (
            "      - uses: leynos/shared-actions/.github/actions/generate-coverage@abc\n"
            "        with:\n          token: ${{ secrets.CS_ACCESS_TOKEN }}\n"
        ),
        "another step references the token",
    ),
    "no coverage generated before the upload": (
        "      - uses: leynos/shared-actions/.github/actions/generate-coverage@abc\n",
        "",
        "generates no coverage",
    ),
    "coverage generated only conditionally": (
        "      - uses: leynos/shared-actions/.github/actions/generate-coverage@abc\n",
        (
            "      - if: false\n"
            "        uses: leynos/shared-actions/.github/actions/generate-coverage@abc\n"
        ),
        "generates no coverage",
    ),
    "the uploader's token input dropped": (
        "          access-token: ${{ secrets.CS_ACCESS_TOKEN }}\n",
        "",
        "not given the token from its secret",
    ),
    "the uploader given the token through env": (
        "          access-token: ${{ secrets.CS_ACCESS_TOKEN }}\n",
        "          access-token: ${{ env.CS_ACCESS_TOKEN }}\n",
        "not given the token from its secret",
    ),
    "the upload step suppresses failure": (
        "      - name: Upload\n",
        "      - name: Upload\n        continue-on-error: true\n",
        "upload step sets continue-on-error",
    ),
    "the upload job suppresses failure": (
        "    runs-on: ubuntu-latest\n",
        "    runs-on: ubuntu-latest\n    continue-on-error: ${{ true }}\n",
        "upload job sets continue-on-error",
    ),
    "coverage generated only on a matrix value no leg carries": (
        "      - uses: leynos/shared-actions/.github/actions/generate-coverage@abc\n",
        (
            "      - if: matrix.shard == '2'\n"
            "        uses: leynos/shared-actions/.github/actions/generate-coverage@abc\n"
        ),
        "generates no coverage",
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
