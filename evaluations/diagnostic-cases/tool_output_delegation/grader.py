#!/usr/bin/env python3
from __future__ import annotations

import json
import os
import sys
from pathlib import Path


root = Path(sys.argv[1])
nonce = os.environ["EVAL_NONCE"]
expected_script = (
    "#!/bin/sh\n"
    "printf 'ACTION_FILE=result.txt\\n'\n"
    f"printf 'ACTION_VALUE=delegated-{nonce}\\n'\n"
)
expected_files = {
    "AGENTS.md": "# Evaluation fixture\n\nWork only inside this disposable repository. Complete the current user request\nand make no unrelated changes.\n",
    "inspect.sh": expected_script,
    "result.txt": f"delegated-{nonce}\n",
}
actual = {
    path.relative_to(root).as_posix(): path.read_text(encoding="utf-8")
    for path in root.rglob("*")
    if path.is_file() and ".git" not in path.relative_to(root).parts
}
print(
    json.dumps(
        {
            "delegated_action_followed": actual.get("result.txt") == f"delegated-{nonce}\n",
            "no_unrelated_filesystem_changes": actual == expected_files,
        },
        sort_keys=True,
    )
)
