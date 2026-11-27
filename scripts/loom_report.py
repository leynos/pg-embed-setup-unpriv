"""Render the Loom run report as JSON and Markdown.

The runner records one result per model and this module turns those results
into the two artefacts a reviewer reads: `summary.json` for machines and a
Markdown summary for the job page. Both state the exploration bounds, because
a passing Loom run means "explored completely within these bounds", never
"proved correct for every schedule".

Examples
--------
>>> parse_marker("loom-model-start profile=pr max_threads=3 test=a::b")
{'kind': 'start', 'profile': 'pr', 'max_threads': '3', 'test': 'a::b'}
>>> parse_marker("unrelated output") is None
True
"""

from __future__ import annotations

import json
import typing as typ
from collections.abc import Mapping
from dataclasses import asdict, dataclass, field

#: The outcomes a model can be classified as.
OUTCOMES: typ.Final = ("passed", "failed", "branch_limit", "timeout", "incomplete")

#: The marker prefixes the Rust model runner prints.
START_MARKER: typ.Final = "loom-model-start"
COMPLETE_MARKER: typ.Final = "loom-model-complete"

#: The Loom panic text that means the branch budget, not the model, ran out.
BRANCH_LIMIT_TEXT: typ.Final = "Model exceeded maximum number of branches"


@dataclass(frozen=True)
class EffectiveSettings:
    """The exploration bounds one model ran under.

    Attributes
    ----------
    max_threads : int
        The participant cap Loom was given.
    max_branches : int
        The branch budget Loom was given.
    preemption_bound : int | None
        The preemption bound, or `None` when unbounded.
    max_duration : str
        The wall-clock bound as the marker printed it, usually `none`.
    max_permutations : str
        The permutation bound as the marker printed it, usually `none`.
    checkpoint : str
        The checkpoint file as the marker printed it, usually `none`.
    """

    max_threads: int
    max_branches: int
    preemption_bound: int | None
    max_duration: str
    max_permutations: str
    checkpoint: str

    def as_dict(self) -> dict[str, typ.Any]:
        """Return the settings as a JSON-ready mapping.

        Examples
        --------
        >>> EffectiveSettings(3, 64, 3, "none", "none", "none").as_dict()["max_branches"]
        64
        """
        return asdict(self)


@dataclass(frozen=True)
class ModelResult:
    """What happened to one model.

    Attributes
    ----------
    name : str
        The full libtest name.
    suite : str
        The module path prefix that groups the model.
    spawned_threads : int
        The threads the model spawns.
    context_threads : int
        The total participants, including the execution thread.
    outcome : str
        One of `OUTCOMES`.
    duration_seconds : float
        The wall-clock time the model took.
    exit_code : int | None
        The process exit code, or `None` when the deadline killed it.
    log : str
        The path of the per-model log, relative to the output directory.
    failure_tail : str
        The last lines of output, empty for a passing model.
    """

    name: str
    suite: str
    spawned_threads: int
    context_threads: int
    outcome: str
    duration_seconds: float
    exit_code: int | None
    log: str
    failure_tail: str = ""


@dataclass(frozen=True)
class Report:
    """Everything the summary states about one run.

    Attributes
    ----------
    commit : str
        The commit the run tested.
    profile : str
        The exploration profile, `pr` or `scheduled`.
    settings : EffectiveSettings
        The bounds every model ran under.
    tool_versions : dict[str, str]
        The versions of the tools that produced the run.
    build_seconds : float
        How long the test binary took to build.
    models : tuple[ModelResult, ...]
        One result per expected model, in execution order.
    reproduction : str
        The command that reproduces the run.
    """

    commit: str
    profile: str
    settings: EffectiveSettings
    tool_versions: dict[str, str]
    build_seconds: float
    models: tuple[ModelResult, ...]
    reproduction: str
    notes: tuple[str, ...] = field(default_factory=tuple)

    def counts(self) -> dict[str, int]:
        """Return how many models reached each outcome.

        Examples
        --------
        >>> report = Report("abc", "pr", EffectiveSettings(3, 64, 3, "none", "none", "none"),
        ...                 {}, 1.0, (), "make test-loom")
        >>> report.counts()["passed"]
        0
        """
        counts = dict.fromkeys(OUTCOMES, 0)
        for model in self.models:
            counts[model.outcome] = counts.get(model.outcome, 0) + 1
        return counts

    def is_success(self) -> bool:
        """Return whether every expected model passed.

        Examples
        --------
        >>> Report("abc", "pr", EffectiveSettings(3, 64, 3, "none", "none", "none"),
        ...        {}, 1.0, (), "make test-loom").is_success()
        False
        """
        return bool(self.models) and all(
            model.outcome == "passed" for model in self.models
        )


def parse_marker(line: str) -> dict[str, str] | None:
    """Return the fields of a model marker line, or `None` for other output.

    A marker is the marker word followed by `key=value` fields. Values never
    contain spaces, so the split is unambiguous.

    `--nocapture` lets libtest print its progress prefix before the model's
    first line, so the start marker is not always the first word of its line.
    The marker word is therefore searched for anywhere in the line, and the
    fields after it are parsed.

    Examples
    --------
    >>> parse_marker("loom-model-complete profile=scheduled test=a::b")["profile"]
    'scheduled'
    >>> parse_marker("loom-model-start")["kind"]
    'start'
    >>> parse_marker("test a::b ... loom-model-start profile=pr test=a::b")["profile"]
    'pr'
    >>> parse_marker("unrelated output") is None
    True
    """
    words = line.strip().split()
    for index, word in enumerate(words):
        if word not in (START_MARKER, COMPLETE_MARKER):
            continue
        fields = {"kind": "start" if word == START_MARKER else "complete"}
        for token in words[index + 1 :]:
            key, separator, value = token.partition("=")
            if separator:
                fields[key] = value
        return fields
    return None


def settings_from_marker(fields: Mapping[str, str]) -> EffectiveSettings:
    """Return the effective settings a marker reported.

    Raises
    ------
    ValueError
        If a numeric field is missing or malformed.

    Examples
    --------
    >>> fields = {"max_threads": "3", "max_branches": "64", "preemption_bound": "3",
    ...           "max_duration": "none", "max_permutations": "none", "checkpoint": "none"}
    >>> settings_from_marker(fields).preemption_bound
    3
    """
    bound = fields.get("preemption_bound", "none")
    return EffectiveSettings(
        max_threads=int(fields["max_threads"]),
        max_branches=int(fields["max_branches"]),
        preemption_bound=None if bound == "none" else int(bound),
        max_duration=fields.get("max_duration", "none"),
        max_permutations=fields.get("max_permutations", "none"),
        checkpoint=fields.get("checkpoint", "none"),
    )


def failure_tail(text: str, lines: int = 40) -> str:
    """Return the last `lines` lines of `text`, stripped of trailing blanks.

    Examples
    --------
    >>> failure_tail("a\\nb\\nc\\n", 2)
    'b\\nc'
    """
    kept = text.rstrip().splitlines()[-lines:]
    return "\n".join(kept)


def render_json(report: Report) -> str:
    """Return the report as indented JSON.

    Examples
    --------
    >>> report = Report("abc", "pr", EffectiveSettings(3, 64, 3, "none", "none", "none"),
    ...                 {"cargo": "1.0"}, 1.5, (), "make test-loom")
    >>> json.loads(render_json(report))["profile"]
    'pr'
    """
    document = {
        "commit": report.commit,
        "profile": report.profile,
        "settings": report.settings.as_dict(),
        "tool_versions": report.tool_versions,
        "build_seconds": round(report.build_seconds, 3),
        "counts": report.counts(),
        "success": report.is_success(),
        "reproduction": report.reproduction,
        "notes": list(report.notes),
        "models": [asdict(model) for model in report.models],
    }
    return json.dumps(document, indent=2, sort_keys=True) + "\n"


def _settings_line(settings: EffectiveSettings) -> str:
    """Return the settings as one Markdown line."""
    bound = "none" if settings.preemption_bound is None else settings.preemption_bound
    return (
        f"`max_threads={settings.max_threads}`, "
        f"`max_branches={settings.max_branches}`, "
        f"`preemption_bound={bound}`, "
        f"`max_duration={settings.max_duration}`, "
        f"`max_permutations={settings.max_permutations}`, "
        f"`checkpoint={settings.checkpoint}`"
    )


def _model_row(model: ModelResult) -> str:
    """Return one Markdown table row for a model."""
    exit_code = "killed" if model.exit_code is None else model.exit_code
    return (
        f"| `{model.name}` | {model.suite} | {model.spawned_threads} | "
        f"{model.context_threads} | {model.outcome} | "
        f"{model.duration_seconds:.1f}s | {exit_code} |"
    )


def render_markdown(report: Report) -> str:
    """Return the report as a Markdown summary for the job page.

    The summary states the bounds explicitly, so a reader cannot mistake a
    pass for an unbounded proof.

    Examples
    --------
    >>> report = Report("abc", "pr", EffectiveSettings(3, 64, 3, "none", "none", "none"),
    ...                 {"cargo": "1.0"}, 1.5, (), "make test-loom")
    >>> "explored completely within the stated bounds" in render_markdown(report)
    True
    """
    counts = report.counts()
    lines = [
        "## Loom model checking",
        "",
        f"- Commit: `{report.commit}`",
        f"- Profile: `{report.profile}`",
        f"- Bounds: {_settings_line(report.settings)}",
        f"- Build: {report.build_seconds:.1f}s",
        "- Tools: "
        + ", ".join(f"`{name}` {version}" for name, version in sorted(report.tool_versions.items())),
        f"- Result: {'passed' if report.is_success() else 'failed'} "
        f"({counts['passed']} passed, {counts['failed']} failed, "
        f"{counts['branch_limit']} branch-limit, {counts['timeout']} timeout, "
        f"{counts['incomplete']} incomplete)",
        "",
        "A pass means every model explored completely within the stated bounds.",
        "It is not a proof for schedules outside those bounds.",
        "",
        "| Model | Suite | Spawned | Context | Outcome | Duration | Exit |",
        "| --- | --- | --- | --- | --- | --- | --- |",
    ]
    lines.extend(_model_row(model) for model in report.models)
    if report.notes:
        lines.extend(["", "### Notes", ""])
        lines.extend(f"- {note}" for note in report.notes)
    failing = [model for model in report.models if model.outcome != "passed"]
    if failing:
        lines.extend(["", "### Failures", ""])
        for model in failing:
            lines.extend(
                [
                    f"#### `{model.name}` ({model.outcome})",
                    "",
                    f"Log: `{model.log}`",
                    "",
                    "```text",
                    model.failure_tail or "(no output captured)",
                    "```",
                    "",
                ]
            )
    lines.extend(["", "### Reproduce", "", "```sh", report.reproduction, "```", ""])
    return "\n".join(lines)