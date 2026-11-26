#!/usr/bin/env python3
"""ultrafast's version, everywhere it is written.

    packaging/version.py              print the version (the root Cargo.toml's)
    packaging/version.py check        fail if any file below says otherwise
    packaging/version.py set X.Y.Z    write X.Y.Z into all of them

The files: the root Cargo.toml (the workspace version and the workspace
crates' dependency versions), Cargo.lock's entries for the workspace
crates, the agent plugin manifests (plugin.json at the root, the Claude
Code plugin's, gemini-extension.json) and the skill's metadata (every
copy, the one the MCP crate serves included). The SDKs keep their own
versions; server.json is a template that release CI fills in for each
version.
"""
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
SEMVER = r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?"
JSON = [
    "plugin.json",
    "plugins/ultrafast/.claude-plugin/plugin.json",
    "plugins/ultrafast-directory/.claude-plugin/plugin.json",
    "gemini-extension.json",
]
SKILLS = [
    "skills/ultrafast/SKILL.md",
    "plugins/ultrafast/skills/ultrafast/SKILL.md",
    "plugins/ultrafast-directory/skills/ultrafast/SKILL.md",
    "crates/ultrafast-mcp/skills/ultrafast/SKILL.md",
]


def crate_names():
    names = []
    for toml in sorted(ROOT.glob("crates/*/Cargo.toml")):
        m = re.search(r'^\[package\][^\[]*?^name = "([^"]+)"', toml.read_text(), re.M | re.S)
        if not m:
            sys.exit(f"version.py: no package name in {toml.relative_to(ROOT)}")
        names.append(m.group(1))
    return names


def current():
    m = re.search(rf'^\[workspace\.package\][^\[]*?^version = "({SEMVER})"', (ROOT / "Cargo.toml").read_text(), re.M | re.S)
    if not m:
        sys.exit("version.py: no version in [workspace.package] of Cargo.toml")
    return m.group(1)


def found():
    """(file, what, version) for every place the version is written."""
    out = []
    cargo = (ROOT / "Cargo.toml").read_text()
    out.append(("Cargo.toml", "[workspace.package]", current()))
    for name, v in re.findall(rf'^(ultrafast[\w-]*) = \{{ path = "crates/[^"]+", version = "({SEMVER})"', cargo, re.M):
        out.append(("Cargo.toml", f"dependency {name}", v))
    lock = (ROOT / "Cargo.lock").read_text()
    for name in crate_names():
        m = re.search(rf'^name = "{re.escape(name)}"\nversion = "({SEMVER})"\n(?!source)', lock, re.M)
        out.append(("Cargo.lock", name, m.group(1) if m else "missing"))
    for f in JSON:
        m = re.search(rf'^  "version": "({SEMVER})"', (ROOT / f).read_text(), re.M)
        out.append((f, "version", m.group(1) if m else "missing"))
    for f in SKILLS:
        m = re.search(rf'^  version: "({SEMVER})"', (ROOT / f).read_text(), re.M)
        out.append((f, "metadata.version", m.group(1) if m else "missing"))
    return out


def check():
    want = current()
    bad = [f"  {f} ({what}): {v}" for f, what, v in found() if v != want]
    if bad:
        sys.exit(f"version.py: Cargo.toml says {want}, but:\n" + "\n".join(bad) + "\nRun packaging/version.py set " + want)
    print(f"{want} everywhere ({len(found())} places)")


def write(path, pattern, repl, count, flags=re.M):
    p = ROOT / path
    text, n = re.subn(pattern, repl, p.read_text(), flags=flags)
    if n != count:
        sys.exit(f"version.py: expected {count} version(s) in {path}, found {n}")
    p.write_text(text)


def set_version(new):
    if not re.fullmatch(SEMVER, new):
        sys.exit(f"version.py: {new!r} is not X.Y.Z")
    cargo = (ROOT / "Cargo.toml").read_text()
    deps = len(re.findall(rf'^ultrafast[\w-]* = \{{ path = "crates/[^"]+", version = "{SEMVER}"', cargo, re.M))
    write("Cargo.toml", rf'(^\[workspace\.package\][^\[]*?^version = "){SEMVER}(")', rf"\g<1>{new}\2", 1, re.M | re.S)
    write("Cargo.toml", rf'(^ultrafast[\w-]* = \{{ path = "crates/[^"]+", version = "){SEMVER}(")', rf"\g<1>{new}\2", deps)
    for name in crate_names():
        write("Cargo.lock", rf'(^name = "{re.escape(name)}"\nversion = "){SEMVER}("\n(?!source))', rf"\g<1>{new}\2", 1)
    for f in JSON:
        write(f, rf'(^  "version": "){SEMVER}(")', rf"\g<1>{new}\2", 1)
    for f in SKILLS:
        write(f, rf'(^  version: "){SEMVER}(")', rf"\g<1>{new}\2", 1)
    check()


if __name__ == "__main__":
    args = sys.argv[1:]
    if not args:
        print(current())
    elif args[0] == "check":
        check()
    elif args[0] == "set" and len(args) == 2:
        set_version(args[1])
    else:
        sys.exit(__doc__)
