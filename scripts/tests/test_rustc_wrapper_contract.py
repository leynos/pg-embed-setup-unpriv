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
    assert wrapper_faults(parse(_GOOD)) == []


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
    assert old in _GOOD
    faults = wrapper_faults(parse(_GOOD.replace(old, new)))
    assert faults, "restoring the wrapper must be reported"
    assert all(fragment in fault for fault in faults)


def test_the_real_ci_workflow_empties_the_wrapper() -> None:
    """The repository's own `ci.yml` keeps both steps on plain rustc."""
    text = (REPOSITORY_ROOT / CI).read_text(encoding="utf-8")
    assert wrapper_faults(parse(text)) == []
