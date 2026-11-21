#!/usr/bin/env python3
"""The MCP Registry entry for one release, from the root server.json.

    server-json.py <version> <dir with the .mcpb bundles and their .sha256> > server.json

Sets the version everywhere, points the bundles at that release's assets
with their SHA-256, and tags the image and crate with it. Drops a package
the registry could not verify: the crate when the README lacks its
`mcp-name:` line, the image when the Dockerfile lacks its label.
"""
import json
import pathlib
import re
import sys

version, bundles = sys.argv[1], pathlib.Path(sys.argv[2])
root = pathlib.Path(__file__).resolve().parents[2]
server = json.loads((root / "server.json").read_text())
name = server["name"]
server["version"] = version

repo = server["repository"]["url"]
kept = []
for pkg in server["packages"]:
    kind = pkg["registryType"]
    if kind == "mcpb":
        slug = re.search(r"ultrafast-[^/]+?-((?:macos|linux)-[a-z0-9_]+)\.mcpb$", pkg["identifier"]).group(1)
        file = f"ultrafast-{version}-{slug}.mcpb"
        sums = bundles / f"{file}.sha256"
        if not sums.exists():
            sys.exit(f"server-json.py: no {sums}")
        pkg["identifier"] = f"{repo}/releases/download/v{version}/{file}"
        pkg["fileSha256"] = sums.read_text().split()[0]
    elif kind == "cargo":
        if f"mcp-name: {name}" not in (root / "README.md").read_text():
            print(f"server-json.py: README.md has no 'mcp-name: {name}'; leaving the crate out", file=sys.stderr)
            continue
        pkg["version"] = version
    elif kind == "oci":
        if f'io.modelcontextprotocol.server.name="{name}"' not in (root / "packaging/docker/Dockerfile").read_text():
            print("server-json.py: the Dockerfile has no io.modelcontextprotocol.server.name label; leaving the image out", file=sys.stderr)
            continue
        pkg["identifier"] = pkg["identifier"].rsplit(":", 1)[0] + f":{version}"
    kept.append(pkg)
server["packages"] = kept
json.dump(server, sys.stdout, indent=2)
sys.stdout.write("\n")
