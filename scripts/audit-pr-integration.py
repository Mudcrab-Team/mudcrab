#!/usr/bin/env python3
"""Audit a captured PR inventory against local Git objects, without network access.

Example:
  python3 scripts/audit-pr-integration.py --inventory /tmp/inventory.json \
    --ref-prefix refs/integration/my-review --output /tmp/integration-audit.json

The inventory scopes this audit. Fetch and capture current PR heads separately;
reachability confirms their commits were merged, not their runtime correctness.
"""

from __future__ import annotations

import argparse
from collections import defaultdict
from itertools import combinations
import json
from pathlib import Path
import re
import subprocess
import sys


def git(repository: Path, *arguments: str) -> str:
    result = subprocess.run(
        ["git", "-C", str(repository), *arguments],
        capture_output=True, text=True, check=False,
    )
    if result.returncode:
        raise ValueError(result.stderr.strip() or f"git {' '.join(arguments)} failed")
    return result.stdout.rstrip("\n")


def ancestor(repository: Path, older: str, newer: str) -> bool:
    result = subprocess.run(
        ["git", "-C", str(repository), "merge-base", "--is-ancestor", older, newer],
        capture_output=True, text=True, check=False,
    )
    if result.returncode not in (0, 1):
        raise ValueError(result.stderr.strip() or "could not check commit ancestry")
    return result.returncode == 0


def _yaml_list(value: str, children: list[str]) -> list[str]:
    """Read only the simple YAML branch lists used by GitHub workflow triggers."""
    value = value.strip()
    if value:
        if not (value.startswith("[") and value.endswith("]")):
            raise ValueError("workflow branch filters must use a YAML list")
        entries = value[1:-1].split(",") if value[1:-1].strip() else []
    else:
        entries = []
        for child in children:
            if not child.strip() or child.lstrip().startswith("#"):
                continue
            match = re.fullmatch(r"\s*-\s+(.+?)\s*", child)
            if not match:
                raise ValueError("unsupported workflow branch filter entry")
            entries.append(match[1])
    patterns = []
    for entry in entries:
        entry = entry.strip()
        if len(entry) >= 2 and entry[0] == entry[-1] and entry[0] in "\"'":
            entry = entry[1:-1]
        if not entry or any(character in entry for character in ("#", "\\", "${")):
            raise ValueError("unsupported workflow branch filter pattern")
        patterns.append(entry)
    return patterns


def pull_request_policy(workflow: str) -> dict:
    """Extract PR branch filters; fail explicitly for unsupported trigger syntax."""
    lines = workflow.splitlines()
    start = next((index for index, line in enumerate(lines)
                  if re.match(r"^(?:on|'on'|\"on\")\s*:", line)), None)
    if start is None:
        raise ValueError("workflow has no on trigger")
    inline = lines[start].split(":", 1)[1].strip()
    if inline:
        if inline.startswith("["):
            events = _yaml_list(inline, [])
        elif re.fullmatch(r"\w+", inline):
            events = [inline]
        else:
            raise ValueError("unsupported inline workflow trigger")
        return {"enabled": "pull_request" in events, "branches": None, "branches_ignore": []}
    end = next((index for index in range(start + 1, len(lines))
                if lines[index].strip() and not lines[index][0].isspace()
                and not lines[index].startswith("#")), len(lines))
    event = next((index for index in range(start + 1, end)
                  if re.fullmatch(r"\s+pull_request\s*:\s*(?:#.*)?", lines[index])), None)
    if event is None:
        return {"enabled": False, "branches": None, "branches_ignore": []}
    event_indent = len(lines[event]) - len(lines[event].lstrip())
    event_end = next((index for index in range(event + 1, end)
                      if lines[index].strip() and not lines[index].lstrip().startswith("#")
                      and len(lines[index]) - len(lines[index].lstrip()) <= event_indent), end)
    policy = {"enabled": True, "branches": None, "branches_ignore": []}
    for index in range(event + 1, event_end):
        match = re.fullmatch(r"\s+(branches|branches-ignore)\s*:\s*(.*?)\s*", lines[index])
        if not match:
            continue
        indent = len(lines[index]) - len(lines[index].lstrip())
        child_end = next((child for child in range(index + 1, event_end)
                          if lines[child].strip() and not lines[child].lstrip().startswith("#")
                          and not lines[child].lstrip().startswith("- ")
                          and len(lines[child]) - len(lines[child].lstrip()) <= indent), event_end)
        policy[match[1].replace("-", "_")] = _yaml_list(match[2], lines[index + 1:child_end])
    if policy["branches"] is not None and policy["branches_ignore"]:
        raise ValueError("workflow cannot combine branches and branches-ignore")
    return policy


def _branch_matches(branch: str, pattern: str) -> bool:
    # GitHub branch globs keep '*' within one path component; '**' crosses '/'.
    if not pattern or any(character in pattern for character in ("?", "+", "[", "]")):
        raise ValueError(f"unsupported GitHub branch glob: {pattern}")
    placeholder = "\x00"
    expression = re.escape(pattern.replace("**", placeholder))
    expression = expression.replace(re.escape(placeholder), ".*")
    expression = expression.replace(r"\*", "[^/]*")
    return re.fullmatch(expression, branch) is not None


def base_runs_ci(base: str, policy: dict) -> bool:
    if not policy["enabled"]:
        return False
    include = policy["branches"]
    accepted = include is None
    for pattern in include or []:
        if pattern.startswith("!"):
            if _branch_matches(base, pattern[1:]):
                accepted = False
        elif _branch_matches(base, pattern):
            accepted = True
    return accepted and not any(_branch_matches(base, pattern)
                                for pattern in policy["branches_ignore"])


def audit(repository: Path, inventory: list[dict], ref_prefix: str,
          integration_head: str = "HEAD",
          workflow_path: str = ".github/workflows/run_tests.yml") -> dict:
    head = git(repository, "rev-parse", "--verify", f"{integration_head}^{{commit}}")
    errors, rows, changed = [], [], defaultdict(list)
    numbers = [item.get("number") for item in inventory]
    if any(type(number) is not int or number < 1 for number in numbers):
        raise ValueError("every inventory row needs a positive integer PR number")
    if len(numbers) != len(set(numbers)):
        raise ValueError("inventory contains duplicate PR numbers")
    if not inventory:
        raise ValueError("inventory is empty; no pending PRs can be verified")
    try:
        policy = pull_request_policy(git(repository, "show", f"{head}:{workflow_path}"))
    except ValueError as error:
        policy = None
        errors.append({"kind": "ci_policy_unreadable", "detail": str(error)})
    captured_heads = {item.get("headRefName"): item.get("headRefOid") for item in inventory}
    for item in sorted(inventory, key=lambda row: row["number"]):
        number, expected, base = item["number"], item.get("headRefOid"), item.get("baseRefName")
        row = {"number": number, "headRefOid": expected, "baseRefName": base,
               "ref": f"{ref_prefix.rstrip('/')}/pr-{number}", "integrated": False}
        rows.append(row)
        try:
            if not isinstance(expected, str) or not re.fullmatch(r"[a-f0-9]{40}|[a-f0-9]{64}", expected):
                raise ValueError("inventory headRefOid must be a full Git object ID")
            if not isinstance(base, str) or not base:
                raise ValueError("inventory baseRefName is missing")
            actual = git(repository, "rev-parse", "--verify", f"{row['ref']}^{{commit}}")
            row["refOid"] = actual
            if actual != expected:
                errors.append({"kind": "ref_head_mismatch", "number": number,
                               "expected": expected, "actual": actual})
                continue
            row["integrated"] = ancestor(repository, expected, head)
            if not row["integrated"]:
                errors.append({"kind": "head_not_integrated", "number": number, "headRefOid": expected})
            merge_base = item.get("mergeBaseOid")
            if merge_base is None:
                base_ref = captured_heads.get(base) or f"refs/remotes/origin/{base}"
                merge_base = git(repository, "merge-base", base_ref, expected)
            elif not isinstance(merge_base, str) or not re.fullmatch(r"[a-f0-9]{40}|[a-f0-9]{64}", merge_base):
                raise ValueError("inventory mergeBaseOid must be a full Git object ID")
            merge_base = git(repository, "rev-parse", "--verify", f"{merge_base}^{{commit}}")
            if not ancestor(repository, merge_base, expected):
                raise ValueError("captured mergeBaseOid is not an ancestor of the captured PR head")
            row["mergeBaseOid"] = merge_base
            paths = git(repository, "diff", "--name-only", "-z", merge_base, expected).split("\x00")
            row["files"] = sorted(path for path in paths if path)
            if "files" in item and set(item["files"]) != set(row["files"]):
                errors.append({"kind": "captured_files_mismatch", "number": number})
            for path in row["files"]:
                changed[path].append(number)
            if policy is not None:
                row["ci_base_covered"] = base_runs_ci(base, policy)
                if not row["ci_base_covered"]:
                    errors.append({"kind": "ci_base_not_covered", "number": number, "baseRefName": base})
        except ValueError as error:
            row["error"] = str(error)
            errors.append({"kind": "pr_audit_failed", "number": number, "detail": str(error)})
    shared = [{"path": path, "prs": prs, "count": len(prs)}
              for path, prs in changed.items() if len(prs) > 1]
    shared.sort(key=lambda row: (-row["count"], row["path"]))
    oids = {row["number"]: row["headRefOid"] for row in rows}
    independent, pair_cache = [], {}
    for overlap in shared:
        pairs = []
        for first, second in combinations(overlap["prs"], 2):
            pair = (first, second)
            if pair not in pair_cache:
                pair_cache[pair] = not (ancestor(repository, oids[first], oids[second])
                                       or ancestor(repository, oids[second], oids[first]))
            if pair_cache[pair]:
                pairs.append([first, second])
        if pairs:
            independent.append({"path": overlap["path"], "independent_pr_pairs": pairs,
                                "count": len(pairs)})
    independent.sort(key=lambda row: (-row["count"], row["path"]))
    return {"passed": not errors, "integrationHeadOid": head, "pr_count": len(rows),
            "integrated_count": sum(row["integrated"] for row in rows),
            "ci_policy": policy, "errors": errors, "prs": rows, "shared_paths": shared,
            "independent_shared_paths": independent}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--inventory", type=Path, required=True)
    parser.add_argument("--ref-prefix", required=True)
    parser.add_argument("--repository", type=Path, default=Path.cwd())
    parser.add_argument("--integration-head", default="HEAD")
    parser.add_argument("--ci-workflow", default=".github/workflows/run_tests.yml")
    parser.add_argument("--output", type=Path)
    arguments = parser.parse_args()
    try:
        inventory = json.loads(arguments.inventory.read_text(encoding="utf-8"))
        if not isinstance(inventory, list) or not all(isinstance(row, dict) for row in inventory):
            raise ValueError("inventory must be a JSON array of PR objects")
        result = audit(arguments.repository, inventory, arguments.ref_prefix,
                       arguments.integration_head, arguments.ci_workflow)
        encoded = json.dumps(result, indent=2, sort_keys=True) + "\n"
        if arguments.output:
            arguments.output.write_text(encoded, encoding="utf-8")
        else:
            sys.stdout.write(encoded)
        return 0 if result["passed"] else 1
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(f"integration audit: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
