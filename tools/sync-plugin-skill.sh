#!/bin/sh
# Copies the ultrafast skill (skills/ultrafast, where `npx skills add` finds
# it) into the Claude Code plugin (plugins/ultrafast/skills/ultrafast), which
# installs from its own folder, along with the license files and security
# policy the plugin ships. `--check` only reports a difference.
set -e
cd "$(dirname "$0")/.."
FILES="LICENSE LICENSE-APACHE LICENSE-MIT SECURITY.md"
if [ "$1" = "--check" ]; then
  diff -r skills/ultrafast plugins/ultrafast/skills/ultrafast
  for f in $FILES; do diff "$f" "plugins/ultrafast/$f"; done
  echo "plugin skill, licenses and SECURITY.md are in sync"
  exit 0
fi
rm -rf plugins/ultrafast/skills/ultrafast
mkdir -p plugins/ultrafast/skills
cp -R skills/ultrafast plugins/ultrafast/skills/ultrafast
for f in $FILES; do cp "$f" "plugins/ultrafast/$f"; done
echo "copied skills/ultrafast and $FILES -> plugins/ultrafast"
