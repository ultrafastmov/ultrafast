#!/usr/bin/env python3
"""Generate src/ultrafastmov/types.py from the ultrafast document schema.

    python3 tools/gen_types.py                      # from https://ultrafast.mov/schema/v1.json
    python3 tools/gen_types.py path/to/v1.json      # from a local copy
    python3 tools/gen_types.py --check [SCHEMA]     # exit 1 if types.py is out of date

Light typing, no runtime cost: every ``$defs`` entry of the schema becomes a
TypedDict (objects, all ``total=False``) or an alias (unions), the top level
becomes ``Document``, and each op's params become ``<Op>Params``, plus the
``OPS``, ``PARAMS`` and ``INPUTS`` tables.
"""

from __future__ import annotations

import json
import keyword
import re
import sys
import urllib.request
from pathlib import Path
from typing import Any, Dict, List, Optional, Set, Tuple

DEFAULT_SCHEMA = "https://ultrafast.mov/schema/v1.json"
OUT = Path(__file__).resolve().parent.parent / "src" / "ultrafastmov" / "types.py"
BUILTIN = {"Any", "Dict", "List", "Literal", "Sequence", "Tuple", "TypedDict", "Union", "Vec2", "Vec3", "Vec4", "Numbers"}


def load(source: str) -> Dict[str, Any]:
    if source.startswith(("https://", "http://")):
        req = urllib.request.Request(source, headers={"User-Agent": "ultrafastmov-gen-types"})
        with urllib.request.urlopen(req, timeout=30) as r:
            return json.loads(r.read().decode("utf-8"))
    return json.loads(Path(source).read_text(encoding="utf-8"))


def camel(name: str) -> str:
    return "".join(p[:1].upper() + p[1:] for p in re.split(r"[^A-Za-z0-9]+", name) if p)


def one_line(text: str) -> str:
    return " ".join(str(text).split())


def split_union(expr: str) -> List[str]:
    if not expr.startswith("Union["):
        return [expr]
    inner, depth, cur, out = expr[6:-1], 0, "", []
    for ch in inner:
        if ch == "," and depth == 0:
            out.append(cur.strip())
            cur = ""
            continue
        depth += ch in "[("
        depth -= ch in "])"
        cur += ch
    out.append(cur.strip())
    return out


def union(parts: List[str]) -> str:
    flat: List[str] = []
    for p in parts:
        for q in split_union(p):
            if q not in flat:
                flat.append(q)
    if "Any" in flat:
        return "Any"
    if "str" in flat:  # a Literal of strings beside str says nothing more
        flat = [p for p in flat if not (p.startswith("Literal[") and p[8:9] in "'\"")]
    return flat[0] if len(flat) == 1 else "Union[" + ", ".join(flat) + "]"


class Gen:
    def __init__(self, schema: Dict[str, Any]) -> None:
        self.defs: Dict[str, Any] = schema.get("$defs", {})
        self.def_names: Dict[str, str] = {}
        self.taken: Set[str] = set(BUILTIN)
        self.blocks: List[str] = []
        self.exported: List[str] = []

    def name(self, want: str, parent: str = "") -> str:
        n = want or parent or "Object"
        if n in self.taken:
            n = parent + want
        base, i = n, 2
        while n in self.taken:
            n, i = f"{base}{i}", i + 1
        self.taken.add(n)
        return n

    def eager(self, expr: str) -> str:
        """Quote generated names in an expression evaluated at import (forward references)."""
        generated = self.taken - BUILTIN
        return re.sub(r"\b[A-Z][A-Za-z0-9]*\b", lambda m: f'"{m.group(0)}"' if m.group(0) in generated else m.group(0), expr)

    def ref(self, def_name: str) -> str:
        if def_name not in self.def_names:
            if def_name not in self.defs:
                return "Any"
            py = self.name(camel(def_name))
            self.def_names[def_name] = py
            self.define(py, self.defs[def_name])
        return self.def_names[def_name]

    def define(self, name: str, s: Dict[str, Any]) -> None:
        """Emit ``name`` for a schema: a TypedDict for an object, else an alias."""
        alts = s.get("oneOf") or s.get("anyOf")
        if s.get("properties") and not alts:
            self.typed_dict(name, s["properties"], one_line(s.get("description", "")))
            return
        if alts:
            parts = []
            for alt in alts:
                if alt.get("properties"):
                    key = (alt.get("required") or list(alt["properties"]))[0]
                    sub = self.name(name + camel(key))
                    self.typed_dict(sub, alt["properties"], one_line(alt.get("description", "")))
                    parts.append(sub)
                else:
                    parts.append(self.py(alt, name))
            expr = union(parts)
        else:
            expr = self.py(s, name)
        doc = one_line(s.get("description", ""))
        self.blocks.append((f"#: {doc}\n" if doc else "") + f"{name} = {self.eager(expr)}")
        self.exported.append(name)

    def py(self, s: Dict[str, Any], hint: str = "", key: str = "") -> str:
        """A type expression for a schema fragment; named types are emitted on the way."""
        if "$ref" in s:
            return self.ref(s["$ref"].rsplit("/", 1)[-1])
        if "const" in s:
            return f"Literal[{s['const']!r}]"
        if "enum" in s:
            return "Literal[" + ", ".join(repr(v) for v in s["enum"]) + "]"
        alts = s.get("anyOf") or s.get("oneOf")
        if alts:
            return union([self.py(a, hint, key) for a in alts])
        t = s.get("type")
        if isinstance(t, list):
            return union([self.py({**s, "type": x}, hint, key) for x in t])
        if t == "number":
            return "float"
        if t == "integer":
            return "int"
        if t == "string":
            return "str"
        if t == "boolean":
            return "bool"
        if t == "null":
            return "None"
        if t == "array":
            items = s.get("items") or {}
            if items.get("type") == "number":
                n = s.get("minItems")
                if n is not None and n == s.get("maxItems") and n in (2, 3, 4):
                    return f"Vec{n}"
                return "Numbers"
            return f"List[{self.py(items, hint, key) if items else 'Any'}]"
        if s.get("properties"):
            sub = self.name(camel(key), hint)
            self.typed_dict(sub, s["properties"], one_line(s.get("description", "")))
            return sub
        extra = s.get("additionalProperties")
        if isinstance(extra, dict):
            return f"Dict[str, {self.py(extra, hint, key)}]"
        return "Dict[str, Any]" if t == "object" else "Any"

    def typed_dict(self, name: str, props: Dict[str, Any], doc: str = "", skipped: Tuple[str, ...] = ()) -> None:
        self.taken.add(name)
        fields = [(k, self.py(v, name, k), one_line(v.get("description", ""))) for k, v in props.items()]
        self.emit_typed_dict(name, fields, doc, skipped)

    def emit_typed_dict(self, name: str, fields: List[Tuple[str, str, str]], doc: str = "", skipped: Tuple[str, ...] = ()) -> None:
        lines: List[str] = []
        note = lambda d: f"  # {d}" if d else ""  # noqa: E731
        if all(k.isidentifier() and not keyword.iskeyword(k) for k, _, _ in fields):
            lines.append(f"class {name}(TypedDict, total=False):")
            if doc:
                lines.append(f'    """{doc}"""')
            lines += [f"    {k}: {t}{note(d)}" for k, t, d in fields]
            if not fields and not doc:
                lines.append("    pass")
        else:
            if doc:
                lines.append(f"#: {doc}")
            lines.append(f'{name} = TypedDict(\n    "{name}",\n    {{')
            lines += [f"        {k!r}: {self.eager(t)},{note(d)}" for k, t, d in fields]
            lines.append("    },\n    total=False,\n)")
        lines += [f"# Also `{k}`: a pattern; any key of that form is accepted." for k in skipped]
        self.blocks.append("\n".join(lines))
        self.exported.append(name)


HEADER = '''"""Types for ultrafast documents, generated from the document schema.

Generated by tools/gen_types.py from {source}
({title}). Do not edit; regenerate with ``python3 tools/gen_types.py``.

Light typing for editors and type checkers; every TypedDict is
``total=False``::

    from ultrafastmov.types import Document, TextParams

    title: TextParams = {{"text": "Hello", "size": 96, "color": "#ffffff"}}
    doc: Document = {{
        "version": 1,
        "format": {{"width": 1080, "height": 1920, "fps": 30, "duration": 6}},
        "nodes": {{"title": {{"op": "text", "params": title}}}},
        "output": "title",
    }}
"""

from __future__ import annotations

from typing import Any, Dict, List, Literal, Sequence, Tuple, TypedDict, Union

#: ``[x, y]``
Vec2 = Sequence[float]
#: ``[x, y, z]``
Vec3 = Sequence[float]
#: four numbers
Vec4 = Sequence[float]
#: a list of numbers
Numbers = Sequence[float]
'''


def generate(schema: Dict[str, Any], source: str) -> str:
    g = Gen(schema)
    node_def = schema.get("$defs", {}).get("node", {})
    # Name the shared definitions first so per-op types refer to them.
    for d in schema.get("$defs", {}):
        g.ref(d)
    g.typed_dict("Document", schema.get("properties", {}), one_line(schema.get("title", "An ultrafast document.")))

    ops: List[str] = []
    inputs: Dict[str, List[str]] = {}
    params_names: Dict[str, str] = {}
    for rule in node_def.get("allOf", []):
        op = rule.get("if", {}).get("properties", {}).get("op", {}).get("const")
        if not isinstance(op, str):
            continue
        ops.append(op)
        then = rule.get("then", {}).get("properties", {})
        params = then.get("params", {}).get("properties", {})
        ok = {k: v for k, v in params.items() if re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", k)}
        skipped = tuple(k for k in params if k not in ok)
        params_names[op] = g.name(f"{camel(op)}Params")
        g.typed_dict(params_names[op], ok, f"Params of a `{op}` node.", skipped)
        inputs[op] = list(then.get("inputs", {}).get("properties", {}))

    out = [HEADER.format(source=source, title=schema.get("title", "")).rstrip() + "\n"]
    out += ["\n\n" + b + "\n" for b in g.blocks]
    out.append("\n\n#: Every built-in op.\nOp = Literal[\n" + "".join(f"    {o!r},\n" for o in ops) + "]\n")
    out.append("OPS: Tuple[str, ...] = (\n" + "".join(f"    {o!r},\n" for o in ops) + ")\n")
    out.append("\n#: The params TypedDict of each op.\nPARAMS: Dict[str, Any] = {\n" + "".join(f"    {o!r}: {params_names[o]},\n" for o in ops) + "}\n")

    def tup(xs: List[str]) -> str:
        return "(" + ", ".join(repr(x) for x in xs) + ("," if len(xs) == 1 else "") + ")"

    out.append("\n#: The input ports of each op.\nINPUTS: Dict[str, Tuple[str, ...]] = {\n" + "".join(f"    {o!r}: {tup(inputs[o])},\n" for o in ops) + "}\n")
    names = ["Vec2", "Vec3", "Vec4", "Numbers"] + g.exported + ["Op", "OPS", "PARAMS", "INPUTS"]
    out.append("\n__all__ = [\n" + "".join(f"    {n!r},\n" for n in names) + "]\n")
    return "".join(out)


def main(argv: List[str]) -> int:
    check = "--check" in argv
    args = [a for a in argv if a != "--check"]
    source: Optional[str] = args[0] if args else DEFAULT_SCHEMA
    text = generate(load(source), DEFAULT_SCHEMA)
    if check:
        current = OUT.read_text(encoding="utf-8") if OUT.exists() else ""
        if current != text:
            print(f"{OUT} is out of date; run python3 tools/gen_types.py {source}", file=sys.stderr)
            return 1
        print(f"{OUT} is up to date")
        return 0
    OUT.write_text(text, encoding="utf-8")
    print(f"wrote {OUT} ({text.count(chr(10))} lines)")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
