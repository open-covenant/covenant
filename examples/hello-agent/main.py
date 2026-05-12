#!/usr/bin/env python3
"""Minimal Covenant hello-agent.

Reads one JSON line on stdin (an Intent), writes one JSON line on stdout
(a result). The daemon supplies the intent, captures the result, and
records the surrounding events in the audit log.
"""

import json
import sys


def main() -> int:
    line = sys.stdin.readline()
    if not line:
        return 1

    intent = json.loads(line)
    text = intent.get("text", "")

    result = {
        "text": f"hello — you asked: {text!r}",
        "sources": [],
    }

    sys.stdout.write(json.dumps(result) + "\n")
    sys.stdout.flush()
    return 0


if __name__ == "__main__":
    sys.exit(main())
