#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Check relative links in repository Markdown files.

External URLs, anchors, and site-absolute paths are ignored. Links inside
fenced code blocks are not links. The check fails if a relative target escapes
the repository or does not exist.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path
from urllib.parse import unquote

ROOT = Path(__file__).resolve().parent.parent
SKIP_DIRS = {".git", "target", "build", ".cargo"}
LINK = re.compile(r"!?\[[^\]]*\]\(\s*([^)\s]+)")
FENCE = re.compile(r"^\s*(```|~~~)")


def markdown_files() -> list[Path]:
    files = []
    for path in ROOT.rglob("*.md"):
        relative = path.relative_to(ROOT)
        if any(part in SKIP_DIRS for part in relative.parts):
            continue
        files.append(path)
    return sorted(files)


def non_code_lines(text: str) -> list[str]:
    lines = []
    in_fence = False
    for line in text.splitlines():
        if FENCE.match(line):
            in_fence = not in_fence
            continue
        if not in_fence:
            lines.append(line)
    return lines


def check_target(source: Path, raw_target: str) -> str | None:
    target = raw_target.strip()
    if target.startswith("<") and target.endswith(">"):
        target = target[1:-1]
    if target.startswith(("http://", "https://", "mailto:", "#")):
        return None
    target = target.split("#", 1)[0]
    if not target:
        return None
    target = unquote(target)
    if target.startswith("/"):
        return None

    resolved = (source.parent / target).resolve()
    try:
        resolved.relative_to(ROOT.resolve())
    except ValueError:
        return f"{source.relative_to(ROOT)}: link escapes repository: {raw_target}"
    if not resolved.exists():
        return f"{source.relative_to(ROOT)}: missing link target: {raw_target}"
    return None


def main() -> int:
    errors = []
    checked = 0
    for source in markdown_files():
        text = source.read_text(errors="replace")
        for line in non_code_lines(text):
            for match in LINK.finditer(line):
                checked += 1
                problem = check_target(source, match.group(1))
                if problem is not None:
                    errors.append(problem)

    if errors:
        for error in errors:
            print(f"error: {error}", file=sys.stderr)
        print(f"FAIL: {len(errors)} broken link(s) checked {checked}", file=sys.stderr)
        return 1

    print(f"PASS: {checked} relative Markdown links checked")
    return 0


if __name__ == "__main__":
    sys.exit(main())
