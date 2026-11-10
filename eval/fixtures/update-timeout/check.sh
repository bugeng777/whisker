#!/usr/bin/env bash
set -euo pipefail
python3 - <<'PY'
import json
with open("config.json") as f:
    data = json.load(f)
assert data["timeout"] == 30, f"timeout is {data['timeout']}, expected 30"
print("ok")
PY
