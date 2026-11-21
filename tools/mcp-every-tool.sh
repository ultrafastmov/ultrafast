#!/bin/bash
# Calls every remote MCP tool once through MCP Inspector's CLI and reports
# which answered and which failed, against a server's /mcp (`ultrafast
# serve`, or Cloud before a directory submission).
#
#   tools/mcp-every-tool.sh <mcp-url> <api-key> <audio-asset>
#
# <audio-asset> is an uploaded asset id (asset://...) or an https URL of any
# audio file, for ultrafast_beats.
set -u
URL=$1 KEY=$2 AUDIO=$3
call() {
  npx -y @modelcontextprotocol/inspector@latest --cli "$URL" --transport http \
    --header "Authorization: Bearer $KEY" --method tools/call --tool-name "$@" 2>/dev/null
}
fail=0
check() { # name, output
  if python3 -c 'import json,sys; d=json.loads(sys.stdin.read()); sys.exit(1 if d.get("isError") else 0)' <<<"$2" 2>/dev/null; then
    printf 'ok    %s\n' "$1"
  else
    printf 'FAIL  %s: %s\n' "$1" "$(head -c 300 <<<"$2")"; fail=1
  fi
}
doc=$(call ultrafast_new --tool-arg template=vertical | python3 -c 'import json,sys; print(json.loads(sys.stdin.read())["content"][0]["text"])')
small=$(python3 -c 'import json,sys; d=json.loads(sys.argv[1]); d["format"]["duration"]=2; print(json.dumps(d))' "$doc")
[ -n "$doc" ] && echo "ok    ultrafast_new" || { echo "FAIL  ultrafast_new"; exit 1; }
check ultrafast_ops        "$(call ultrafast_ops --tool-arg 'ops=["text"]')"
check ultrafast_check      "$(call ultrafast_check --tool-arg "document=$small")"
check ultrafast_frame      "$(call ultrafast_frame --tool-arg "document=$small" --tool-arg t=1 --tool-arg max_size=256)"
check ultrafast_sheet      "$(call ultrafast_sheet --tool-arg "document=$small" --tool-arg count=4)"
check ultrafast_review     "$(call ultrafast_review --tool-arg "document=$small" --tool-arg safe=social)"
check ultrafast_inspect    "$(call ultrafast_inspect --tool-arg "document=$small" --tool-arg t=1)"
check ultrafast_edit       "$(call ultrafast_edit --tool-arg "document=$small" --tool-arg 'edits=[{"edit":"add_layer","id":"x","node":{"op":"solid","params":{"color":"#ff000040"}}}]')"
check ultrafast_estimate   "$(call ultrafast_estimate --tool-arg "document=$small")"
check ultrafast_profile    "$(call ultrafast_profile --tool-arg "document=$small" --tool-arg frames=10)"
check ultrafast_export     "$(call ultrafast_export --tool-arg "document=$small")"
check ultrafast_batch      "$(call ultrafast_batch --tool-arg "document=$small" --tool-arg 'rows=[{"name":"a"},{"name":"b"}]')"
check ultrafast_beats      "$(call ultrafast_beats --tool-arg "audio=$AUDIO")"
check ultrafast_import_svg "$(call ultrafast_import_svg --tool-arg 'svg=<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><circle cx="5" cy="5" r="4" fill="#f36"/></svg>')"
check ultrafast_measure    "$(call ultrafast_measure --tool-arg 'params={"text":"Hello","size":64}')"
check ultrafast_outline    "$(call ultrafast_outline --tool-arg 'params={"text":"Hello","size":64}')"
exit $fail
