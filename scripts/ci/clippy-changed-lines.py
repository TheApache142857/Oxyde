#!/usr/bin/env python3
"""Fails when clippy or rustc warns on a line that a change adds or modifies.

The workspace carries older clippy warnings (CLAUDE.md, Quality gate): a change fixes
those in the code it touches and adds none. Errors are printed whatever their line;
cargo's own exit status reports them.

Usage: clippy-changed-lines.py CLIPPY_JSON BASE_REV
  CLIPPY_JSON  the output of `cargo clippy --message-format=json`
  BASE_REV     the revision the change is compared with (HEAD is the change)
"""

import json
import re
import subprocess
import sys

HUNK = re.compile(r"@@ -\S+ \+(\d+)(?:,(\d+))? @@")


def changed_lines(base):
    """Maps each changed Rust file to the line numbers HEAD adds or modifies in it."""
    diff = subprocess.run(
        ["git", "diff", "--unified=0", "--no-color", "--no-ext-diff", base, "HEAD", "--", "*.rs"],
        check=True,
        capture_output=True,
        text=True,
        encoding="utf-8",
    ).stdout
    lines = {}
    path = None
    for line in diff.splitlines():
        if line.startswith("+++ "):
            path = line[len("+++ b/"):] if line.startswith("+++ b/") else None
        elif line.startswith("@@") and path is not None:
            hunk = HUNK.match(line)
            if hunk:
                start = int(hunk.group(1))
                count = int(hunk.group(2) or "1")
                lines.setdefault(path, set()).update(range(start, start + count))
    return lines


def escape(text):
    """GitHub's workflow-command escaping for an annotation message."""
    return text.replace("%", "%25").replace("\r", "%0D").replace("\n", "%0A")


def main():
    json_path, base = sys.argv[1], sys.argv[2]
    changed = changed_lines(base)
    seen = set()
    warnings = 0
    with open(json_path, encoding="utf-8") as messages:
        for raw in messages:
            try:
                message = json.loads(raw)
            except json.JSONDecodeError:
                continue
            if message.get("reason") != "compiler-message":
                continue
            diagnostic = message["message"]
            level = diagnostic.get("level")
            rendered = diagnostic.get("rendered") or diagnostic.get("message", "")
            if level == "error":
                print(rendered)
                continue
            if level != "warning":
                continue
            for span in diagnostic.get("spans", []):
                if not span.get("is_primary"):
                    continue
                path = span["file_name"].replace("\\", "/")
                lines = range(span["line_start"], span["line_end"] + 1)
                if not changed.get(path, set()).intersection(lines):
                    continue
                key = (path, span["line_start"], diagnostic["message"])
                if key in seen:
                    continue
                seen.add(key)
                warnings += 1
                print(f"::error file={path},line={span['line_start']}::{escape(diagnostic['message'])}")
                print(rendered)
    print(f"{warnings} warning(s) in changed lines")
    return 1 if warnings else 0


if __name__ == "__main__":
    sys.exit(main())
