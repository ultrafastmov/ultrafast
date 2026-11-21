#!/bin/sh
# Copies the ultrafast skill (skills/ultrafast, where `npx skills add` finds
# it) into the Claude Code plugin (plugins/ultrafast/skills/ultrafast), which
# installs from its own folder. `--check` only reports a difference.
set -e
cd "$(dirname "$0")/.."
if [ "$1" = "--check" ]; then
  diff -r skills/ultrafast plugins/ultrafast/skills/ultrafast && echo "plugin skill is in sync"
  exit $?
fi
rm -rf plugins/ultrafast/skills/ultrafast
mkdir -p plugins/ultrafast/skills
cp -R skills/ultrafast plugins/ultrafast/skills/ultrafast
echo "copied skills/ultrafast -> plugins/ultrafast/skills/ultrafast"
