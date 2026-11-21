#!/usr/bin/env python3
"""The MCP Registry entry for one release, from the root server.json.

    server-json.py <version> <dir with the .mcpb bundles' .sha256 files> [--live] > server.json

Sets the version everywhere, points the bundles at that release's assets
with their SHA-256, and tags the image and crate with it. Drops a package
the registry could not verify: the crate without its `mcp-name:` line in
the README, the image without its io.modelcontextprotocol.server.name
label. By default it checks this checkout (README.md, the Dockerfile), for
a release being built; with --live it checks what is published for that
version (the README crates.io shows, the image's labels on ghcr.io).
"""
import json
import pathlib
import re
import sys
import urllib.request

args = [a for a in sys.argv[1:] if not a.startswith("--")]
live = "--live" in sys.argv
version, bundles = args[0], pathlib.Path(args[1])
root = pathlib.Path(__file__).resolve().parents[2]
server = json.loads((root / "server.json").read_text())
name = server["name"]
server["version"] = version


def get(url, headers=None):
    req = urllib.request.Request(url, headers={"User-Agent": "ultrafast-release", **(headers or {})})
    with urllib.request.urlopen(req, timeout=30) as r:
        return r.read()


def crate_has_marker(crate):
    if not live:
        return f"mcp-name: {name}" in (root / "README.md").read_text()
    try:
        html = get(f"https://static.crates.io/readmes/{crate}/{crate}-{version}.html").decode()
    except Exception:
        return False
    return f"mcp-name: {name}" in html


def image_has_label(image):
    if not live:
        return f'io.modelcontextprotocol.server.name="{name}"' in (root / "packaging/docker/Dockerfile").read_text()
    host, repo = image.split("/", 1)
    try:
        token = json.loads(get(f"https://{host}/token?scope=repository:{repo}:pull"))["token"]
        auth = {"Authorization": f"Bearer {token}"}
        accept = ", ".join([
            "application/vnd.oci.image.index.v1+json",
            "application/vnd.docker.distribution.manifest.list.v2+json",
            "application/vnd.oci.image.manifest.v1+json",
            "application/vnd.docker.distribution.manifest.v2+json",
        ])
        manifest = json.loads(get(f"https://{host}/v2/{repo}/manifests/{version}", {**auth, "Accept": accept}))
        if "manifests" in manifest:
            pick = next(m for m in manifest["manifests"] if m.get("platform", {}).get("architecture") == "amd64")
            manifest = json.loads(get(f"https://{host}/v2/{repo}/manifests/{pick['digest']}", {**auth, "Accept": accept}))
        config = json.loads(get(f"https://{host}/v2/{repo}/blobs/{manifest['config']['digest']}", auth))
    except Exception:
        return False
    return (config.get("config", {}).get("Labels") or {}).get("io.modelcontextprotocol.server.name") == name


repo_url = server["repository"]["url"]
where = "on crates.io" if live else "README.md"
kept = []
for pkg in server["packages"]:
    kind = pkg["registryType"]
    if kind == "mcpb":
        slug = re.search(r"ultrafast-[^/]+?-((?:macos|linux)-[a-z0-9_]+)\.mcpb$", pkg["identifier"]).group(1)
        file = f"ultrafast-{version}-{slug}.mcpb"
        sums = bundles / f"{file}.sha256"
        if not sums.exists():
            sys.exit(f"server-json.py: no {sums}")
        pkg["identifier"] = f"{repo_url}/releases/download/v{version}/{file}"
        pkg["fileSha256"] = sums.read_text().split()[0]
    elif kind == "cargo":
        if not crate_has_marker(pkg["identifier"]):
            print(f"server-json.py: {pkg['identifier']} {version}'s README ({where}) has no 'mcp-name: {name}'; leaving the crate out", file=sys.stderr)
            continue
        pkg["version"] = version
    elif kind == "oci":
        image = pkg["identifier"].rsplit(":", 1)[0]
        if not image_has_label(image):
            print(f"server-json.py: {image}:{version} has no io.modelcontextprotocol.server.name label; leaving the image out", file=sys.stderr)
            continue
        pkg["identifier"] = f"{image}:{version}"
    kept.append(pkg)
server["packages"] = kept
json.dump(server, sys.stdout, indent=2)
sys.stdout.write("\n")
