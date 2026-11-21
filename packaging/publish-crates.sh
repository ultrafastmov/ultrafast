#!/usr/bin/env bash
# Publishes the workspace crates to crates.io in dependency order. Versions
# already on crates.io are skipped, so a run that stopped can run again, and
# crates.io's limit on new crates (a burst, then one every ten minutes) is
# waited out. Needs CARGO_REGISTRY_TOKEN.
#
#   packaging/publish-crates.sh [--dry-run]
set -euo pipefail
cd "$(dirname "$0")/.."
DRY=${1:-}
UA="ultrafast-release (https://github.com/ultrafastmov/ultrafast)"
# Workspace crates, each after the workspace crates it depends on
# (normal, build and versioned dev dependencies).
ORDER=$(cargo metadata --format-version 1 --no-deps | python3 -c '
import json, sys
m = json.load(sys.stdin)
pk = {p["name"]: p for p in m["packages"] if p.get("publish") != []}
deps = {n: {d["name"] for d in p["dependencies"] if d["name"] in pk and (d["kind"] != "dev" or d.get("req", "*") != "*")} for n, p in pk.items()}
out, seen = [], set()
def visit(n, path=()):
    if n in seen: return
    if n in path: sys.exit("dependency cycle: " + " -> ".join(path + (n,)))
    for d in sorted(deps[n]): visit(d, path + (n,))
    seen.add(n); out.append(n)
for n in sorted(pk): visit(n)
print(" ".join(n + "@" + pk[n]["version"] for n in out))')
echo "order: $ORDER"
for cv in $ORDER; do
  c=${cv%@*} v=${cv#*@}
  if curl -fsS -A "$UA" "https://crates.io/api/v1/crates/$c/$v" >/dev/null 2>&1; then
    echo "== $c $v is on crates.io already"; continue
  fi
  if [ "$DRY" = --dry-run ]; then echo "== would publish $c $v"; continue; fi
  for attempt in 1 2 3 4 5 6 7 8; do
    echo "== publishing $c $v (attempt $attempt)"
    if out=$(cargo publish -p "$c" --locked 2>&1); then echo "$out" | tail -3; break; fi
    echo "$out" | tail -8
    if echo "$out" | grep -qiE '429|too many|rate limit'; then
      echo "rate limited; waiting 11 minutes"; sleep 660
    else
      exit 1
    fi
    [ "$attempt" = 8 ] && { echo "giving up on $c"; exit 1; }
  done
done
