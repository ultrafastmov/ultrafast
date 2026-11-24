#!/bin/sh
# Copies the ultrafast skill (skills/ultrafast, where `npx skills add` finds
# it) into each Claude plugin that ships it, along with the license files
# and security policy: plugins/ultrafast (the Claude Code marketplace plugin,
# skill plus the local MCP server) and plugins/ultrafast-directory (the
# skill-only bundle listed in Claude's plugin directory). Each plugin installs
# from its own folder. `--check` only reports a difference.
set -e
cd "$(dirname "$0")/.."
FILES="LICENSE LICENSE-APACHE LICENSE-MIT SECURITY.md"
PLUGINS="plugins/ultrafast plugins/ultrafast-directory"
if [ "$1" = "--check" ]; then
  for p in $PLUGINS; do
    diff -r skills/ultrafast "$p/skills/ultrafast"
    for f in $FILES; do diff "$f" "$p/$f"; done
  done
  echo "plugin skills, licenses and SECURITY.md are in sync"
  exit 0
fi
for p in $PLUGINS; do
  rm -rf "$p/skills/ultrafast"
  mkdir -p "$p/skills"
  cp -R skills/ultrafast "$p/skills/ultrafast"
  for f in $FILES; do cp "$f" "$p/$f"; done
  echo "copied skills/ultrafast and $FILES -> $p"
done
