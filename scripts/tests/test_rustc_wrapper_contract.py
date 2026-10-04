"""Hold the two plain-rustc steps to the workflow.

A check run only over the real file passes whether or not it discriminates, so
each way of restoring the wrapper is driven through the judge first and must
fail, and the real `ci.yml` is judged last.
"""

from __future__ import annotations

from pathlib import Path

import pytest
from rustc_wrapper import ROOT_STEP, UNPRIVILEGED_STEP, wrapper_faults
from workflow_reader import Workflow

REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
CI = ".github/workflows/ci.yml"

_GOOD = f"""
"on": pull_request
jobs:
  build-test:
    steps:
      - name: {UNPRIVILEGED_STEP}
        env:
          RUSTC_WRAPPER: ""
        run: cargo nextest run
      - name: {ROOT_STEP}
        run: |
          run_as_root() {{
            sudo -E env \\
              HOME="$HOME" \\
              RUSTC_WRAPPER= \\
              PATH="$PATH" \\
              "$@"
          }}
          run_as_root make test
"""


def parse(text: str) -> Workflow:
    """Parse a workflow fragment."""
    return Workflow.parse(CI, text)


def test_the_good_shape_has_no_fault() -> None:
    """Both steps empty the wrapper, so nothing is reported."""
    faults = wrapper_faults(parse(_GOOD))
    assert faults == [], f"the reference workflow must be fault-free, got {faults}"


@pytest.mark.parametrize(
    ("old", "new", "fragment"),
    [
        ('        env:\n          RUSTC_WRAPPER: ""\n', "", UNPRIVILEGED_STEP),
        ('RUSTC_WRAPPER: ""', "RUSTC_WRAPPER: sccache", UNPRIVILEGED_STEP),
        ("              RUSTC_WRAPPER= \\\n", "", ROOT_STEP),
        ("RUSTC_WRAPPER= \\", "RUSTC_WRAPPER=sccache \\", ROOT_STEP),
    ],
    ids=["unprivileged-unset", "unprivileged-sccache", "root-dropped", "root-sccache"],
)
def test_restoring_the_wrapper_is_a_fault(old: str, new: str, fragment: str) -> None:
    """Each way of letting the wrapper back into a step names that step."""
    assert old in _GOOD, f"the mutation target {old!r} must exist in the reference workflow"
    faults = wrapper_faults(parse(_GOOD.replace(old, new)))
    assert faults, f"replacing {old!r} with {new!r} must be reported"
    assert all(fragment in fault for fault in faults), f"faults must name {fragment!r}: {faults}"


def test_an_assignment_after_the_command_does_not_count() -> None:
    """An unrelated `RUSTC_WRAPPER=` line after `run_as_root make test` is not the argument.

    `sudo -E env` would still pass sccache to root's build, so a check that
    searched the whole script would accept this shape and miss the defect.
    """
    dropped = _GOOD.replace("              RUSTC_WRAPPER= \\\n", "")
    assert dropped != _GOOD, "the in-function assignment must exist in the reference workflow"
    misplaced = dropped.replace(
        "          run_as_root make test\n",
        "          run_as_root make test\n          RUSTC_WRAPPER= \\\n",
    )
    assert misplaced != dropped, "the trailing assignment must have been added"
    faults = wrapper_faults(parse(misplaced))
    assert faults, "an assignment outside the sudo env arguments must be reported"
    assert all(ROOT_STEP in fault for fault in faults), f"faults must name the root step: {faults}"


def test_the_real_ci_workflow_empties_the_wrapper() -> None:
    """The repository's own `ci.yml` keeps both steps on plain rustc."""
    text = (REPOSITORY_ROOT / CI).read_text(encoding="utf-8")
    faults = wrapper_faults(parse(text))
    assert faults == [], f"ci.yml must keep both steps on plain rustc, got {faults}"
