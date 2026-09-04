"""Contract tests binding the release workflow to the binstall metadata.

The release workflow publishes assets that `cargo binstall` resolves from
`[package.metadata.binstall]`. These tests fail when the two drift apart, and
when a `gh`-invoking job loses the repository context it needs, which is how
every release before v0.5.2 silently published nothing.
"""

from __future__ import annotations

import hashlib
import importlib.util
import re
import sys
import tarfile
import tomllib
import typing as typ
from pathlib import Path

import pytest
import yaml

if typ.TYPE_CHECKING:
    from collections.abc import Iterator, Mapping

REPO_ROOT = Path(__file__).resolve().parents[2]
RELEASE_WORKFLOW = REPO_ROOT / ".github" / "workflows" / "release.yml"
CI_WORKFLOW = REPO_ROOT / ".github" / "workflows" / "ci.yml"
MANIFEST = REPO_ROOT / "Cargo.toml"

SCRIPT_PATH = REPO_ROOT / "scripts" / "release_archive.py"
sys.path.insert(0, str(SCRIPT_PATH.parent))
SPEC = importlib.util.spec_from_file_location("release_archive", SCRIPT_PATH)
assert SPEC is not None
release_archive = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = release_archive
SPEC.loader.exec_module(release_archive)

# Jobs that create, upload to, read from, or publish a draft release. Draft
# releases are invisible to read-scoped tokens, so auditing needs write too.
RELEASE_WRITER_JOBS = (
    "create-release",
    "build-assets",
    "audit-draft-assets",
    "publish-release",
)


def load_workflow(path: Path) -> Mapping[str, typ.Any]:
    """Parse a GitHub Actions workflow into a mapping.

    Parameters
    ----------
    path : Path
        Workflow file to parse.

    Returns
    -------
    Mapping[str, typing.Any]
        The parsed workflow document.

    Examples
    --------
    >>> load_workflow(RELEASE_WORKFLOW)["name"]
    'Release'
    """
    return yaml.safe_load(path.read_text(encoding="utf-8"))


def workflow_job(path: Path, name: str) -> Mapping[str, typ.Any]:
    """Return the named job from a workflow, failing when it is absent.

    Parameters
    ----------
    path : Path
        Workflow file to inspect.
    name : str
        Job identifier to return.

    Returns
    -------
    Mapping[str, typing.Any]
        The job definition.

    Examples
    --------
    >>> workflow_job(RELEASE_WORKFLOW, "create-release")["name"]
    'Create draft release'
    """
    jobs = load_workflow(path)["jobs"]
    assert name in jobs, f"{path.name} has no job named {name}"
    return jobs[name]


def iter_jobs(path: Path) -> Iterator[tuple[str, Mapping[str, typ.Any]]]:
    """Yield every job identifier and definition in a workflow."""
    yield from load_workflow(path)["jobs"].items()


def job_checks_out(job: Mapping[str, typ.Any]) -> bool:
    """Report whether a job runs `actions/checkout`."""
    return any(
        str(step.get("uses", "")).startswith("actions/checkout@")
        for step in job.get("steps", ())
    )


# Match `gh` as a command word so prose such as "through" cannot trigger it.
GH_INVOCATION = re.compile(r"(?:^|[\s;&|(])gh\s")


def step_runs_gh(step: Mapping[str, typ.Any]) -> bool:
    """Report whether a step shells out to the `gh` CLI."""
    return GH_INVOCATION.search(str(step.get("run", ""))) is not None


def step_sets_gh_repo(job: Mapping[str, typ.Any], step: Mapping[str, typ.Any]) -> bool:
    """Report whether `GH_REPO` reaches a step from its own or its job's env."""
    return "GH_REPO" in (job.get("env") or {}) or "GH_REPO" in (step.get("env") or {})


def binstall_metadata() -> Mapping[str, str]:
    """Return the `[package.metadata.binstall]` table from the manifest."""
    manifest = tomllib.loads(MANIFEST.read_text(encoding="utf-8"))
    return manifest["package"]["metadata"]["binstall"]


def production_binaries() -> tuple[str, ...]:
    """Return the manifest binaries that carry no `required-features` gate."""
    manifest = tomllib.loads(MANIFEST.read_text(encoding="utf-8"))
    return tuple(
        entry["name"] for entry in manifest["bin"] if "required-features" not in entry
    )


def render_binstall_template(template: str, values: Mapping[str, str]) -> str:
    """Substitute cargo-binstall `{ key }` placeholders in a template.

    Parameters
    ----------
    template : str
        Template drawn from `[package.metadata.binstall]`.
    values : Mapping[str, str]
        Placeholder names mapped to their rendered values.

    Returns
    -------
    str
        The rendered template.

    Raises
    ------
    AssertionError
        Raised when the template retains an unsubstituted placeholder.

    Examples
    --------
    >>> render_binstall_template("{ name }-{ target }", {"name": "a", "target": "b"})
    'a-b'
    """
    rendered = template
    for key, value in values.items():
        rendered = rendered.replace(f"{{ {key} }}", value)
    assert "{" not in rendered, f"unsubstituted placeholder in {rendered!r}"
    return rendered


def build_archive(repo: Path, target: str, version: str) -> Path:
    """Stage an archive of stub binaries and return its path.

    Parameters
    ----------
    repo : Path
        Temporary repository root receiving the stub Cargo outputs.
    target : str
        Rust target triple to package.
    version : str
        Package version without the leading `v`.

    Returns
    -------
    Path
        Path to the staged `.tgz` archive.
    """
    binaries = release_archive.DEFAULT_BINARIES
    extension = release_archive.binary_extension(target)
    release_dir = repo / "target" / target / "release"
    release_dir.mkdir(parents=True)
    for binary in binaries:
        (release_dir / f"{binary}{extension}").write_text(binary, encoding="utf-8")
    return release_archive.stage_archive(
        release_archive.ReleaseArchiveSpec(
            repo=repo,
            target=target,
            version=version,
            dist_dir=repo / "dist",
            binaries=binaries,
        )
    )


@pytest.fixture(name="version")
def version_fixture() -> str:
    """Return the version used when staging contract archives."""
    return "9.9.9"


@pytest.mark.parametrize("workflow", [RELEASE_WORKFLOW, CI_WORKFLOW])
def test_gh_steps_resolve_the_repository(workflow: Path) -> None:
    """Every `gh` step must have a checkout or an explicit `GH_REPO`.

    Without either, `gh` tries to infer the repository from git remotes and
    fails with `not a git repository`, which skipped every downstream release
    job before this contract existed.
    """
    for job_name, job in iter_jobs(workflow):
        if job_checks_out(job):
            continue
        for step in job.get("steps", ()):
            if not step_runs_gh(step):
                continue
            assert step_sets_gh_repo(job, step), (
                f"{workflow.name} job {job_name} step "
                f"{step.get('name', '<unnamed>')!r} runs gh without a checkout "
                "or GH_REPO"
            )


def test_create_release_avoids_verify_tag_without_a_checkout() -> None:
    """`--verify-tag` requires a local clone, so the API check replaces it."""
    job = workflow_job(RELEASE_WORKFLOW, "create-release")
    scripts = "\n".join(str(step.get("run", "")) for step in job["steps"])
    if job_checks_out(job):
        pytest.skip("job has a checkout, so --verify-tag can resolve the tag")
    assert "--verify-tag" not in scripts
    assert "git/ref/tags" in scripts, "tag existence must still be checked"


@pytest.mark.parametrize("job_name", RELEASE_WRITER_JOBS)
def test_release_jobs_request_contents_write(job_name: str) -> None:
    """Jobs that create, upload, read draft, or publish assets need write scope."""
    job = workflow_job(RELEASE_WORKFLOW, job_name)
    assert job.get("permissions", {}).get("contents") == "write"


def test_upload_step_publishes_archives_and_sidecars() -> None:
    """The upload step must publish each archive together with its sidecar."""
    job = workflow_job(RELEASE_WORKFLOW, "build-assets")
    uploads = [
        step for step in job["steps"] if "gh release upload" in str(step.get("run", ""))
    ]
    assert len(uploads) == 1, "expected exactly one release upload step"
    run = str(uploads[0]["run"])
    assert "dist/*.tgz" in run
    assert "dist/*.tgz.sha256" in run


def test_default_binaries_match_the_manifest_production_binaries() -> None:
    """The packaged binary set must equal the ungated manifest binaries."""
    assert set(release_archive.DEFAULT_BINARIES) == set(production_binaries())


@pytest.mark.parametrize(
    "target",
    [
        "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-gnu",
        "aarch64-apple-darwin",
        "x86_64-pc-windows-msvc",
    ],
)
def test_archive_layout_matches_binstall_metadata(
    tmp_path: Path, version: str, target: str
) -> None:
    """The staged archive name and members must render the binstall templates."""
    metadata = binstall_metadata()
    manifest = tomllib.loads(MANIFEST.read_text(encoding="utf-8"))
    values = {
        "repo": manifest["package"]["repository"],
        "name": manifest["package"]["name"],
        "target": target,
        "version": version,
        "archive-suffix": f".{metadata['pkg-fmt']}",
        "binary-ext": release_archive.binary_extension(target),
    }

    archive = build_archive(tmp_path, target, version)

    expected_url = render_binstall_template(metadata["pkg-url"], values)
    assert archive.name == expected_url.rsplit("/", 1)[-1]

    with tarfile.open(archive, "r:gz") as tar:
        members = {member.name for member in tar.getmembers() if member.isfile()}
    expected_members = {
        render_binstall_template(metadata["bin-dir"], {**values, "bin": binary})
        for binary in release_archive.DEFAULT_BINARIES
    }
    assert members == expected_members


def test_archive_sidecar_records_the_archive_digest(tmp_path: Path, version: str) -> None:
    """Each archive gains a `sha256sum`-compatible sidecar naming that archive."""
    archive = build_archive(tmp_path, "x86_64-unknown-linux-gnu", version)
    sidecar = release_archive.checksum_sidecar_path(archive)

    assert sidecar.name == f"{archive.name}.sha256"
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    assert sidecar.read_text(encoding="ascii") == f"{digest}  {archive.name}\n"
