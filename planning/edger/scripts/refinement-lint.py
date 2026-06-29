#!/usr/bin/env python3
"""
Planning lint for edger — mirrors agile-refinement SKILL.md Mode 1 checklist.
Emits verbose per-category, per-file lines (OK and findings).
Usage:
  python3 planning/edger/scripts/refinement-lint.py [--scope PATH] [--repo ROOT]
"""

from __future__ import annotations

import argparse
import re
import sys
from collections import defaultdict
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Iterable

# Severity
RED = "red"
WARN = "warn"
INFO = "info"

PLACEHOLDER_PATTERNS = [
    re.compile(p, re.I)
    for p in [
        r"\bTODO\b",
        r"\bFIXME\b",
        r"\bTBD\b",
        r"\bplaceholder\b",
        r"<\s*fill\s*>",
        r"\{\{.*?\}\}",
        r"Lorem ipsum",
    ]
]

SECRET_PATTERNS = [
    re.compile(p, re.I)
    for p in [
        r"sk-[a-zA-Z0-9]{20,}",
        r"api[_-]?key\s*[:=]\s*['\"][^'\"]+['\"]",
        r"password\s*[:=]\s*['\"][^'\"]+['\"]",
        r"BEGIN (RSA |EC )?PRIVATE KEY",
    ]
]

STORY_REQUIRED = [
    "## Context",
    "## Files",
    "## Detail",
    "## Tasks",
    "## Verification",
]

EPIC_REQUIRED = [
    "## Context",
    "## Story backlog",
    "## Epic acceptance criteria",
    "## Status",
]

PATH_REF_RE = re.compile(
    r"(?:`|\[)(planning/edger/[^`\]\s|)]+)(?:`|\])"
    r"|(?<![`])planning/edger/[a-zA-Z0-9_./-]+\.md"
)

STORY_DEP_RE = re.compile(
    r"(?:Depends on|Depende de|Depends on epic):\s*`?([^`\n]+)`?",
    re.I,
)
STORY_REF_RE = re.compile(r"\b(\d{2})\.(\d{2})\b")
EPIC_FOLDER_RE = re.compile(r"epics/(\d{2})-[^/]+/")

SIZE_VALUES = {"small", "medium", "large", "spike", "xlarge"}


@dataclass
class Finding:
    category: str
    severity: str
    location: str
    message: str


@dataclass
class LintState:
    findings: list[Finding] = field(default_factory=list)
    ok_lines: list[str] = field(default_factory=list)
    files_checked: list[Path] = field(default_factory=list)

    def ok(self, category: str, location: str, detail: str = "") -> None:
        suffix = f" — {detail}" if detail else ""
        self.ok_lines.append(f"  [OK] [{category}] {location}{suffix}")

    def add(self, category: str, severity: str, location: str, message: str) -> None:
        self.findings.append(Finding(category, severity, location, message))


def rel(repo: Path, p: Path) -> str:
    try:
        return str(p.relative_to(repo))
    except ValueError:
        return str(p)


def iter_md_files(scope: Path) -> list[Path]:
    if scope.is_file():
        return [scope] if scope.suffix == ".md" else []
    return sorted(scope.rglob("*.md"))


def is_story_file(p: Path) -> bool:
    name = p.name
    if name == "00-overview.md":
        return False
    parent = p.parent.name
    if not parent.startswith(tuple(f"{i:02d}-" for i in range(1, 100))):
        return False
    return bool(re.match(r"^\d{2}-.+\.md$", name))


def is_epic_overview(p: Path) -> bool:
    return p.name == "00-overview.md" and "/epics/" in str(p)


def is_planning_md(p: Path, repo: Path) -> bool:
    r = rel(repo, p)
    return r.startswith("planning/edger/")


def extract_path_refs(text: str) -> set[str]:
    refs: set[str] = set()
    for m in PATH_REF_RE.finditer(text):
        raw = m.group(1) if m.lastindex and m.group(1) else m.group(0)
        raw = raw.strip("`[]")
        if raw.startswith("planning/edger/"):
            refs.add(raw.split("#")[0].rstrip("/"))
    return refs


def resolve_ref(repo: Path, ref: str) -> Path | None:
    candidate = repo / ref
    if candidate.exists():
        return candidate
    # story shorthand in same epic: `01-setup-core-crate.md`
    if "/" not in ref and ref.endswith(".md"):
        return None  # caller resolves with epic context
    return None


def parse_story_table_deps(text: str, epic_dir: Path) -> list[str]:
    deps: list[str] = []
    in_table = False
    for line in text.splitlines():
        if "| Story |" in line or "| Story " in line:
            in_table = True
            continue
        if in_table:
            if not line.strip().startswith("|"):
                break
            if re.match(r"^\|[-| ]+\|$", line.strip()):
                continue
            cols = [c.strip() for c in line.strip().strip("|").split("|")]
            if len(cols) >= 6:
                dep_col = cols[5]
                if dep_col and dep_col.lower() not in ("-", "—", "none", "epic 01", "epic 01 (completed)"):
                    deps.append(dep_col)
    return deps


def lint_cross_refs(repo: Path, files: list[Path], state: LintState) -> None:
    category = "Cross-references"
    epic_story_files: dict[str, set[str]] = defaultdict(set)

    for epic_dir in (repo / "planning/edger/epics").glob("*"):
        if epic_dir.is_dir():
            epic_id = epic_dir.name[:2]
            for sf in epic_dir.glob("*.md"):
                if sf.name != "00-overview.md":
                    epic_story_files[epic_id].add(sf.name)

    for fpath in files:
        text = fpath.read_text(encoding="utf-8")
        r = rel(repo, fpath)

        # Origin field
        origin_m = re.search(r"\*\*Origin:\*\*\s*`([^`]+)`", text)
        if origin_m:
            origin = origin_m.group(1).split("#")[0]
            op = repo / origin
            if op.exists():
                state.ok(category, r, f"Origin → {origin}")
            else:
                state.add(category, RED, r, f"Origin missing: {origin}")
        elif is_story_file(fpath) or is_epic_overview(fpath):
            state.add(category, WARN, r, "Missing **Origin:** field")

        for ref in extract_path_refs(text):
            target = repo / ref
            if target.exists():
                state.ok(category, r, f"ref → {ref}")
            else:
                # same-epic story shorthand
                if is_story_file(fpath) or is_epic_overview(fpath):
                    epic_dir = fpath.parent
                    local = epic_dir / Path(ref).name
                    if local.exists():
                        state.ok(category, r, f"ref → {ref} (resolved locally)")
                        continue
                state.add(category, RED, r, f"Broken ref: {ref}")


def lint_dependencies(repo: Path, files: list[Path], state: LintState) -> None:
    category = "Dependencies"
    graph: dict[str, set[str]] = defaultdict(set)
    nodes: set[str] = set()

    epics_root = repo / "planning/edger/epics"
    story_id_to_file: dict[str, Path] = {}

    for epic_dir in sorted(epics_root.glob("*")):
        if not epic_dir.is_dir():
            continue
        epic_num = epic_dir.name[:2]
        nodes.add(f"epic:{epic_num}")
        for sf in epic_dir.glob("[0-9][0-9]-*.md"):
            sid = f"{epic_num}.{sf.name[:2]}"
            story_id_to_file[sid] = sf
            nodes.add(sid)

    def add_edge(src: str, dst: str) -> None:
        if src != dst:
            graph[src].add(dst)

    for fpath in files:
        if not is_epic_overview(fpath) and not is_story_file(fpath):
            continue
        text = fpath.read_text(encoding="utf-8")
        r = rel(repo, fpath)
        epic_num = fpath.parent.name[:2]

        if is_epic_overview(fpath):
            dep_m = re.search(
                r"\*\*Depends on epic:\*\*\s*`([^`]+)`", text, re.I
            )
            if dep_m:
                dep_path = dep_m.group(1)
                m = EPIC_FOLDER_RE.search(dep_path)
                if m:
                    dep_epic = m.group(1)
                    if (repo / dep_path).exists():
                        add_edge(f"epic:{epic_num}", f"epic:{dep_epic}")
                        state.ok(category, r, f"epic depends on epic:{dep_epic}")
                    else:
                        state.add(category, RED, r, f"Depends on epic missing: {dep_path}")
                else:
                    state.add(category, WARN, r, f"Unparsed epic dep: {dep_path}")

        if is_story_file(fpath):
            sid = f"{epic_num}.{fpath.name[:2]}"
            dep_section = re.search(
                r"(?:\*\*Depende de:\*\*|### Dependências)\s*\n+((?:- .+\n?)+)",
                text,
                re.I,
            )
            dep_lines = dep_section.group(1) if dep_section else ""
            if not dep_lines:
                trace = re.search(r"\*\*Depende de:\*\*\s*(.+)$", text, re.I | re.M)
                dep_lines = trace.group(1) if trace else ""
            for line in dep_lines.splitlines():
                dep_raw = line.lstrip("- ").strip()
                if not dep_raw or "epic" in dep_raw.lower() and "story" not in dep_raw.lower():
                    if "epic" in dep_raw.lower():
                        em = re.search(r"(\d{2})", dep_raw)
                        if em:
                            dep_epic = em.group(1)
                            add_edge(sid, f"epic:{dep_epic}")
                            state.ok(category, r, f"story depends on epic:{dep_epic}")
                    continue
                for sm in STORY_REF_RE.finditer(dep_raw):
                    if sm.group(1) != epic_num:
                        continue  # cross-epic refs like Epic 02.03 handled above
                    dep_sid = f"{sm.group(1)}.{sm.group(2)}"
                    if dep_sid == sid:
                        continue
                    if dep_sid in story_id_to_file:
                        add_edge(sid, dep_sid)
                        state.ok(category, r, f"story depends on {dep_sid}")
                    else:
                        state.add(category, RED, r, f"Unknown story dep: {dep_sid}")

        if is_epic_overview(fpath):
            for dep_col in parse_story_table_deps(text, fpath.parent):
                for sm in STORY_REF_RE.finditer(dep_col):
                    dep_sid = f"{sm.group(1)}.{sm.group(2)}"
                    src_overview = f"epic:{epic_num}"
                    if dep_sid in story_id_to_file:
                        # table deps are per-story; record representative
                        state.ok(category, r, f"backlog dep {dep_sid}")
                    elif "epic" in dep_col.lower():
                        em = re.search(r"(\d{2})", dep_col)
                        if em:
                            state.ok(category, r, f"backlog dep epic:{em.group(1)}")
                    else:
                        state.add(category, WARN, r, f"Unparsed backlog dep: {dep_col}")

    # cycle detection (DFS)
    visited: set[str] = set()
    stack: set[str] = set()
    cycles: list[list[str]] = []

    def dfs(node: str, path: list[str]) -> None:
        if node in stack:
            cycles.append(path + [node])
            return
        if node in visited:
            return
        visited.add(node)
        stack.add(node)
        for nxt in graph.get(node, []):
            dfs(nxt, path + [node])
        stack.remove(node)

    for node in nodes:
        dfs(node, [])

    if cycles:
        for cyc in cycles:
            state.add(category, RED, "planning/edger/epics/", f"Circular dep: {' → '.join(cyc)}")
    else:
        state.ok(category, "planning/edger/epics/", "no circular dependencies")


def lint_completeness(repo: Path, files: list[Path], state: LintState) -> None:
    category = "Completeness"
    for fpath in files:
        if not is_story_file(fpath) and not is_epic_overview(fpath):
            continue
        text = fpath.read_text(encoding="utf-8")
        r = rel(repo, fpath)
        required = STORY_REQUIRED if is_story_file(fpath) else EPIC_REQUIRED

        for sec in required:
            if sec in text:
                state.ok(category, r, f"section {sec}")
            else:
                state.add(category, RED, r, f"Missing section: {sec}")

        if is_story_file(fpath):
            ac_pat = re.compile(
                r"###?\s*(Acceptance criteria|Critérios de aceite)",
                re.I,
            )
            if ac_pat.search(text):
                state.ok(category, r, "acceptance criteria present")
            else:
                state.add(category, RED, r, "Missing acceptance criteria")

            unchecked = re.findall(r"^- \[ \] .+", text, re.M)
            checked = re.findall(r"^- \[x\] .+", text, re.I | re.M)
            completed = (
                "completed" in text.lower()
                or "delivered" in text.lower()
                or (checked and not unchecked)
            )
            if len(unchecked) >= 3:
                state.ok(category, r, f"{len(unchecked)} actionable tasks")
            elif unchecked:
                state.add(category, WARN, r, f"Only {len(unchecked)} tasks (expected >=3)")
            elif completed:
                state.ok(category, r, f"completed story ({len(checked)} tasks done)")
            else:
                state.add(category, RED, r, "No unchecked tasks")

            if re.search(r"```(?:bash|sh)?\s*\n", text):
                state.ok(category, r, "verification commands")
            else:
                state.add(category, WARN, r, "No fenced verification commands")


def lint_product_traceability(repo: Path, files: list[Path], state: LintState) -> None:
    category = "Product traceability"
    business_dir = repo / "business"
    proto_dir = repo / "planning/edger/prototypes"
    has_business = business_dir.exists() and any(business_dir.glob("*.md"))
    has_proto = proto_dir.exists() and any(proto_dir.rglob("*.md"))

    for fpath in files:
        if not is_story_file(fpath) and not is_epic_overview(fpath):
            continue
        text = fpath.read_text(encoding="utf-8")
        r = rel(repo, fpath)

        for pat in SECRET_PATTERNS:
            if pat.search(text):
                state.add(category, RED, r, "Possible secret/credential pattern detected")
                break
        else:
            state.ok(category, r, "no secret patterns")

        if has_business and "business/" not in text and "UI" in text:
            state.add(category, INFO, r, "UI story without business/ ref (proto may suffice)")
        else:
            state.ok(category, r, "business traceability N/A or present")

        if has_proto:
            state.add(category, INFO, r, "prototypes exist — verify screen refs if UI-heavy")
        else:
            state.ok(category, r, "no prototypes dir — skip screen refs")


def lint_consistency(repo: Path, files: list[Path], state: LintState) -> None:
    category = "Consistency"
    for fpath in files:
        if not is_epic_overview(fpath):
            continue
        text = fpath.read_text(encoding="utf-8")
        r = rel(repo, fpath)

        status_m = re.search(r"^## Status\s*\n+(.+)$", text, re.M)
        if status_m:
            state.ok(category, r, f"status: {status_m.group(1).strip()[:60]}")
        else:
            state.add(category, WARN, r, "Missing ## Status")

        sizes = re.findall(r"\|\s*(small|medium|large|spike|xlarge)\s*\|", text, re.I)
        bad = [s for s in sizes if s.lower() not in SIZE_VALUES]
        if bad:
            state.add(category, WARN, r, f"Unknown sizes: {bad}")
        elif sizes:
            state.ok(category, r, f"sizes: {', '.join(sizes)}")


def lint_format(repo: Path, files: list[Path], state: LintState) -> None:
    category = "Format"
    epics_root = repo / "planning/edger/epics"

    for epic_dir in sorted(epics_root.glob("*")):
        if epic_dir.is_dir():
            r = rel(repo, epic_dir)
            if not re.match(r"^\d{2}-", epic_dir.name):
                state.add(category, WARN, r, f"Epic folder naming: {epic_dir.name}")
            else:
                state.ok(category, r, "epic folder naming")

            overview = epic_dir / "00-overview.md"
            if overview.exists():
                state.ok(category, rel(repo, overview), "00-overview.md present")
            else:
                state.add(category, RED, r, "Missing 00-overview.md")

    for fpath in files:
        if not is_planning_md(fpath, repo):
            continue
        r = rel(repo, fpath)
        name = fpath.name

        if is_story_file(fpath):
            if re.match(r"^\d{2}-.+\.md$", name):
                state.ok(category, r, "story file naming")
            else:
                state.add(category, RED, r, f"Bad story filename: {name}")

        for pat in PLACEHOLDER_PATTERNS:
            if pat.search(fpath.read_text(encoding="utf-8")):
                # TODO in task checkboxes is OK if whole-file placeholder
                if pat.pattern == r"\bTODO\b":
                    continue
                state.add(category, WARN, r, f"Placeholder pattern: {pat.pattern}")
                break
        else:
            state.ok(category, r, "no placeholder tokens")


def lint_scope(repo: Path, files: list[Path], state: LintState) -> None:
    category = "Scope conflicts"
    for fpath in files:
        if not is_story_file(fpath):
            continue
        text = fpath.read_text(encoding="utf-8")
        r = rel(repo, fpath)

        out_m = re.search(r"### Out of scope\s*\n+((?:- .+\n?)+)", text, re.I)
        in_m = re.search(r"### Scope\s*\n+((?:- .+\n?)+)", text, re.I)
        if out_m and in_m:
            state.ok(category, r, "In/Out scope sections present")
        elif is_story_file(fpath):
            state.add(category, INFO, r, "Scope/Out sections incomplete")

        # duplicate story titles in same epic
        title = text.splitlines()[0] if text else ""
        epic_dir = fpath.parent
        dupes = [
            sf
            for sf in epic_dir.glob("[0-9][0-9]-*.md")
            if sf != fpath and sf.read_text(encoding="utf-8").splitlines()[0] == title
        ]
        if dupes:
            state.add(category, RED, r, f"Duplicate title with {dupes[0].name}")
        else:
            state.ok(category, r, "unique title in epic")


def lint_stale(repo: Path, files: list[Path], state: LintState) -> None:
    category = "Stale content"
    now = datetime.now(timezone.utc)

    for fpath in files:
        if not is_story_file(fpath) and not is_epic_overview(fpath):
            continue
        text = fpath.read_text(encoding="utf-8")
        r = rel(repo, fpath)

        unchecked = len(re.findall(r"^- \[ \] ", text, re.M))
        checked = len(re.findall(r"^- \[x\] ", text, re.I | re.M))

        if "in progress" in text.lower() or "in-progress" in text.lower():
            if unchecked == 0 and checked > 0:
                state.add(category, WARN, r, "Status in-progress but all tasks checked")
            else:
                state.ok(category, r, "in-progress with open tasks")
        else:
            state.ok(category, r, "status not stale-in-progress")

        age_days = (now - datetime.fromtimestamp(fpath.stat().st_mtime, tz=timezone.utc)).days
        if age_days > 30 and unchecked > 0:
            state.add(category, INFO, r, f"File >30d old ({age_days}d) with open tasks")
        else:
            state.ok(category, r, f"age {age_days}d")


def lint_inventory(repo: Path, state: LintState) -> None:
    epics_root = repo / "planning/edger/epics"
    epic_count = 0
    story_count = 0
    lines = []
    for epic_dir in sorted(epics_root.glob("*")):
        if not epic_dir.is_dir():
            continue
        epic_count += 1
        stories = [f for f in epic_dir.glob("*.md") if f.name != "00-overview.md"]
        story_count += len(stories)
        lines.append(f"  {epic_dir.name}: {len(stories)} stories")
    state.ok_lines.append(f"[Inventory] {epic_count} epics, {story_count} stories")
    state.ok_lines.extend(lines)


def run_lint(repo: Path, scope: Path) -> LintState:
    state = LintState()
    files = [f for f in iter_md_files(scope) if is_planning_md(f, repo) or f == scope]
    if scope.name == "planning" or str(scope).endswith("planning/edger"):
        files = iter_md_files(repo / "planning/edger")
        files = [f for f in files if f.is_file()]

    state.files_checked = files

    lint_cross_refs(repo, files, state)
    lint_dependencies(repo, files, state)
    lint_completeness(repo, files, state)
    lint_product_traceability(repo, files, state)
    lint_consistency(repo, files, state)
    lint_format(repo, files, state)
    lint_scope(repo, files, state)
    lint_stale(repo, files, state)

    if str(scope).endswith("planning/edger") or scope == repo / "planning/edger":
        lint_inventory(repo, state)

    return state


def format_report(repo: Path, scope: Path, state: LintState, round_label: str) -> str:
    lines = [
        "=" * 72,
        f"AGILE REFINEMENT LINT — {rel(repo, scope)}",
        f"Round: {round_label}",
        f"Generated: {datetime.now(timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ')}",
        f"Files in scope: {len(state.files_checked)}",
        "=" * 72,
        "",
    ]

    by_cat: dict[str, list[Finding]] = defaultdict(list)
    for f in state.findings:
        by_cat[f.category].append(f)

    categories = [
        "Cross-references",
        "Dependencies",
        "Completeness",
        "Product traceability",
        "Consistency",
        "Format",
        "Scope conflicts",
        "Stale content",
    ]

    for cat in categories:
        lines.append(f"## {cat}")
        cat_ok = [l for l in state.ok_lines if f"[{cat}]" in l]
        for ok in cat_ok:
            lines.append(ok)
        if cat in by_cat:
            for finding in by_cat[cat]:
                sev = finding.severity.upper()
                lines.append(
                    f"  [{sev}] {finding.location}: {finding.message}"
                )
        if not cat_ok and cat not in by_cat:
            lines.append("  (no files in scope for this category)")
        lines.append("")

    inv = [l for l in state.ok_lines if l.startswith("[Inventory]") or re.match(r"^  \d{2}-", l)]
    if inv:
        lines.append("## Inventory")
        lines.extend(inv)
        lines.append("")

    reds = [f for f in state.findings if f.severity == RED]
    warns = [f for f in state.findings if f.severity == WARN]
    infos = [f for f in state.findings if f.severity == INFO]

    lines.append("## Summary")
    lines.append(f"  RED: {len(reds)}  WARN: {len(warns)}  INFO: {len(infos)}  OK lines: {len(state.ok_lines)}")
    if reds:
        lines.append("VERDICT: FAIL — fix RED findings before proceeding")
    else:
        lines.append("VERDICT: PASS — 0 RED findings (warnings may remain)")
    lines.append("")
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description="Planning lint (agile-refinement Mode 1)")
    parser.add_argument(
        "--scope",
        default="planning/edger",
        help="Scope path relative to repo root (file or directory)",
    )
    parser.add_argument(
        "--repo",
        default=".",
        help="Repository root",
    )
    parser.add_argument(
        "--round",
        default="full-tree",
        help="Round label for report header",
    )
    parser.add_argument(
        "--fail-on-warn",
        action="store_true",
        help="Exit 1 on warnings too",
    )
    args = parser.parse_args()

    repo = Path(args.repo).resolve()
    scope = (repo / args.scope).resolve()
    if not scope.exists():
        print(f"Scope not found: {scope}", file=sys.stderr)
        return 2

    state = run_lint(repo, scope)
    report = format_report(repo, scope, state, args.round)
    print(report)

    reds = [f for f in state.findings if f.severity == RED]
    warns = [f for f in state.findings if f.severity == WARN]
    if reds:
        return 1
    if args.fail_on_warn and warns:
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())