#!/usr/bin/env bash
# Expose the tail as a GitHub check annotation as well as the full job log.
set -o pipefail
log=$(mktemp)
trap 'rm -f "$log"' EXIT
set +e
"$@" 2>&1 | tee "$log"
status=${PIPESTATUS[0]}
set -e
if (( status != 0 )); then
  python3 - "$log" <<'PY'
import sys
with open(sys.argv[1], encoding='utf-8', errors='replace') as f:
    text = f.read()[-3500:]
text = text.replace('%', '%25').replace('\r', '%0D').replace('\n', '%0A')
print(f'::error title=Build or test failed::{text}')
PY
fi
exit "$status"
