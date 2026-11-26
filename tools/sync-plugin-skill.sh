#!/bin/sh
# Copies the ultrafast skill (skills/ultrafast, where `npx skills add` finds
# it) into each Claude plugin that ships it, along with the license files
# and security policy: plugins/ultrafast (the Claude Code marketplace plugin,
# skill plus the local MCP server) and plugins/ultrafast-directory (the
# skill-only bundle listed in Claude's plugin directory). Each plugin installs
# from its own folder. It also copies the skill into the MCP crate
# (crates/ultrafast-mcp/skills/ultrafast), which builds it into `uf mcp` and
# `uf serve` to serve over MCP: a published crate can't reach files outside
# its own folder. `--check` only reports a difference.
#
# One file differs on purpose: the directory bundle keeps its own
# references/setup.md, which installs `uf` and stops there. The directory
# refuses plugins whose text adds an MCP server they don't declare, and only
# a Verified organization's plugin may include a local one, so that copy
# leaves out `uf mcp` and the remote server. This script never overwrites it.
set -e
cd "$(dirname "$0")/.."
FILES="LICENSE LICENSE-APACHE LICENSE-MIT SECURITY.md"
PLUGINS="plugins/ultrafast plugins/ultrafast-directory"
DIRECTORY=plugins/ultrafast-directory
OWN_SETUP=skills/ultrafast/references/setup.md
CRATE=crates/ultrafast-mcp/skills/ultrafast
if [ "$1" = "--check" ]; then
  for p in $PLUGINS; do
    if [ "$p" = "$DIRECTORY" ]; then
      diff -r -x setup.md skills/ultrafast "$p/skills/ultrafast"
      test -f "$p/$OWN_SETUP" || { echo "missing $p/$OWN_SETUP"; exit 1; }
      if grep -n "uf mcp\|mcp add\|mcpServers\|mcp_servers" "$p/$OWN_SETUP"; then
        echo "$p/$OWN_SETUP must not add an MCP server"; exit 1
      fi
    else
      diff -r skills/ultrafast "$p/skills/ultrafast"
    fi
    for f in $FILES; do diff "$f" "$p/$f"; done
  done
  diff -r skills/ultrafast "$CRATE"
  echo "plugin and crate skills, licenses and SECURITY.md are in sync"
  exit 0
fi
own=$(mktemp)
trap 'rm -f "$own"' EXIT
cp "$DIRECTORY/$OWN_SETUP" "$own"
for p in $PLUGINS; do
  rm -rf "$p/skills/ultrafast"
  mkdir -p "$p/skills"
  cp -R skills/ultrafast "$p/skills/ultrafast"
  if [ "$p" = "$DIRECTORY" ]; then cp "$own" "$p/$OWN_SETUP"; fi
  for f in $FILES; do cp "$f" "$p/$f"; done
  echo "copied skills/ultrafast and $FILES -> $p"
done
rm -rf "$CRATE"
mkdir -p "$(dirname "$CRATE")"
cp -R skills/ultrafast "$CRATE"
echo "copied skills/ultrafast -> $CRATE"
