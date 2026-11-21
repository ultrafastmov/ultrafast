#!/usr/bin/env python3
"""Writes an MCP Bundle manifest.json for one release archive.

    manifest.py <version> <platform: darwin|linux|win32> <tools.json> > manifest.json

tools.json is the server's own `tools/list` result ({"tools": [...]}), so
the bundle lists exactly the tools the binary serves.

The server runs through /bin/sh so that it starts in a folder the person
can find: hosts start local servers in / (read-only on macOS), and tools
resolve relative paths against the working directory. The same line adds
the usual Homebrew and /usr/local folders to PATH, which hosts don't pass
on, so ffmpeg, espeak-ng and tesseract are found.
"""
import json
import re
import sys

version, platform, tools_path = sys.argv[1:4]
with open(tools_path) as f:
    tools = json.load(f)["tools"]


def first_sentence(text):
    text = " ".join(text.split())
    m = re.match(r"(.+?[.!?])(\s|$)", text)
    s = m.group(1) if m else text
    return s if len(s) <= 200 else s[:199].rsplit(" ", 1)[0] + "…"


# $0 is the folder, $1 the binary. An unresolved ${...} default falls back to ~/ultrafast.
LAUNCH = (
    "d=$0; case $d in ''|*'${'*) d=$HOME/ultrafast;; esac; "
    'mkdir -p "$d" 2>/dev/null; cd "$d" 2>/dev/null || cd "$HOME" 2>/dev/null || cd /tmp; '
    'PATH=${PATH:+$PATH:}/opt/homebrew/bin:/usr/local/bin:/home/linuxbrew/.linuxbrew/bin:/usr/bin:/bin; export PATH; '
    'exec "$1" mcp'
)

if platform == "darwin":
    needs = "Apple silicon. Renders with Metal; exports, voices and OCR need nothing else."
elif platform == "linux":
    needs = (
        "Linux x86_64 (glibc 2.28 or later) with your GPU's Vulkan driver. "
        "Exports need ffmpeg; `ultrafast_voiceover` needs espeak-ng and `ultrafast_ocr` needs tesseract."
    )
else:
    sys.exit(f"manifest.py: no bundle for {platform}")

manifest = {
    "manifest_version": "0.3",
    "name": "ultrafast",
    "display_name": "ultrafast",
    "version": version,
    "description": "The GPU video engine for agents: JSON documents in, video out.",
    "long_description": (
        "ultrafast renders video on your GPU from JSON documents. Claude writes a document, "
        "looks at exact frames and contact sheets, gets a review of what a viewer would trip over "
        "(unreadable or clipped text, unsafe areas, flashing), fixes it, and exports MP4, MOV, WebM, "
        "a GIF, or a PNG or EXR sequence at any size.\n\n"
        "Documents and videos given as relative paths go in the folder set below "
        "(`~/ultrafast` by default).\n\n"
        f"Requirements: {needs}"
    ),
    "author": {"name": "ultrafast", "url": "https://ultrafast.mov"},
    "repository": {"type": "git", "url": "https://github.com/ultrafastmov/ultrafast"},
    "homepage": "https://ultrafast.mov",
    "documentation": "https://ultrafast.mov/docs/mcp/",
    "support": "https://github.com/ultrafastmov/ultrafast/issues",
    "icon": "icon.png",
    "server": {
        "type": "binary",
        "entry_point": "server/ultrafast",
        "mcp_config": {
            "command": "/bin/sh",
            "args": ["-c", LAUNCH, "${user_config.folder}", "${__dirname}/server/ultrafast"],
            "env": {},
        },
    },
    "tools": [{"name": t["name"], "description": first_sentence(t["description"])} for t in tools],
    "keywords": ["video", "motion graphics", "gpu", "render", "export", "animation", "captions"],
    "license": "Apache-2.0 OR MIT",
    "compatibility": {"platforms": [platform]},
    "user_config": {
        "folder": {
            "type": "directory",
            "title": "Folder",
            "description": "Where documents and videos go when Claude gives a relative path. Created if missing.",
            "default": "${HOME}/ultrafast",
            "required": False,
        }
    },
}
json.dump(manifest, sys.stdout, indent=2, ensure_ascii=False)
sys.stdout.write("\n")
