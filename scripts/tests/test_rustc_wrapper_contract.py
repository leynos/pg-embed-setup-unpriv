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


@pytest.mark.parametrize("remove_directory_permissions", [False, True])
def test_root_step_stages_worker_for_privilege_drop(
    tmp_path: Path, remove_directory_permissions: bool
) -> None:
    """The actual root step supplies an accessible worker and cleans it up."""
    import os
    import subprocess
    import sys

    workflow = parse((REPOSITORY_ROOT / CI).read_text(encoding="utf-8"))
    step = next(step for _, step in workflow.steps() if step.get("name") == ROOT_STEP)
    script = step["run"]
    if remove_directory_permissions:
        assert 'chmod 755 "$worker_dir"' in script
        script = script.replace('chmod 755 "$worker_dir"', ":")

    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    commands = {
        "sudo": '#!/bin/sh\nshift\nexec "$@"\n',
        "cargo": (
            '#!/bin/sh\nmkdir -p target/debug\n'
            'printf "#!/bin/sh\\nexit 0\\n" > target/debug/pg_worker\n'
        ),
        "make": (
            '#!/bin/sh\n'
            'test -x "$PG_EMBEDDED_WORKER" || exit 1\n'
            '"$PYTHON_EXECUTABLE" -c \'import os, sys; '
            'mode = os.stat(os.path.dirname(os.environ["PG_EMBEDDED_WORKER"])).st_mode; '
            'sys.exit((mode & 0o777) != 0o755)\' || exit 2\n'
            'printf "%s" "$PG_EMBEDDED_WORKER" > "$WORKER_RECORD"\n'
        ),
    }
    for name, content in commands.items():
        executable = bin_dir / name
        executable.write_text(content, encoding="utf-8")
        executable.chmod(0o755)

    record = tmp_path / "worker-path"
    env = os.environ.copy()
    env.update(
        HOME=str(tmp_path),
        CARGO_HOME=str(tmp_path),
        RUSTUP_HOME=str(tmp_path),
        GITHUB_TOKEN="test-token",
        PYTHON_EXECUTABLE=sys.executable,
        PATH=f"{bin_dir}:/usr/bin:/bin",
        WORKER_RECORD=str(record),
    )
    result = subprocess.run(
        ["bash", "-e", "-o", "pipefail", "-c", script],
        cwd=tmp_path,
        env=env,
        capture_output=True,
        text=True,
        check=False,
    )
    if remove_directory_permissions:
        assert result.returncode != 0, "the check must reject an inaccessible staging directory"
    else:
        assert result.returncode == 0, result.stderr
        assert not Path(record.read_text(encoding="utf-8")).exists(), "the worker must be cleaned up"
