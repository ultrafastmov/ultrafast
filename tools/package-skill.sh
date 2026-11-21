#!/bin/sh
# Packs the ultrafast skill as the ZIP Claude.ai takes under Customize >
# Skills (the skill folder at the top of the archive): target/skill/ultrafast.zip.
set -e
cd "$(dirname "$0")/.."
mkdir -p target/skill
rm -f target/skill/ultrafast.zip
(cd skills && zip -qr -X ../target/skill/ultrafast.zip ultrafast)
echo "target/skill/ultrafast.zip"
