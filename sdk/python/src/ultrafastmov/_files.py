"""Local files in documents: find them, upload them, point a copy at the uploads.

A server reads only references (``asset://``, ``https://``, ``gs://``,
``s3://``, ``r2://``), so before a document goes up every local file it
names is uploaded and the copy that is sent says ``asset://ID`` instead.
Which strings are files follows the server (``crates/ultrafast-service/src/refs.rs``):
the file params of built-in ops, every param of a component instance and
every component default, the nodes inside components, and
``meta.fonts[].src``; and the ``src`` of every audio driver
(``{"audio": {"src": "music.mp3", "band": "low"}}``), which any param may be.
"""

from __future__ import annotations

import copy
import os
import re
from pathlib import Path
from typing import Any, Callable, Dict, List, Mapping, Optional
from urllib.parse import unquote, urlparse
from urllib.request import url2pathname

__all__ = ["FILE_PARAMS", "is_reference", "is_family_name", "localize", "sequence_files"]

#: ``(op, param)`` pairs that name files the renderer reads.
FILE_PARAMS = frozenset(
    {
        ("image", "src"),
        ("video", "src"),
        ("audio", "src"),
        ("svg", "src"),
        ("lottie", "src"),
        ("lut", "src"),
        ("mesh", "src"),
        ("captions", "src"),
        ("clip_camera", "src"),
        ("clip_camera", "faces"),
        ("block", "logo"),
        ("text", "font"),
        ("text", "em_font"),
        ("block", "display_font"),
        ("block", "text_font"),
        ("block", "mono_font"),
        ("mesh", "font"),
        ("swarm", "font"),
        ("code", "font"),
        ("captions", "font"),
    }
)

REFERENCE_PREFIXES = ("asset://", "https://", "http://", "gs://", "s3://", "r2://", "data:")
FONT_SUFFIXES = (".ttf", ".otf", ".ttc", ".otc", ".woff", ".woff2", ".dfont")
_SCHEME = re.compile(r"^[A-Za-z][A-Za-z0-9+.\-]*://")
FRAME = re.compile(r"\{frame(?::(0?)(\d+))?\}")


def is_reference(s: str) -> bool:
    """Whether ``s`` already names something the server can read (or a URL of any scheme but file://)."""
    if s.startswith(REFERENCE_PREFIXES):
        return True
    return bool(_SCHEME.match(s)) and not s.lower().startswith("file://")


def is_family_name(s: str) -> bool:
    """Whether a font param value is a family name (``"Inter"``) rather than a file."""
    lower = s.lower()
    return not ("/" in s or "\\" in s or s.startswith(".") or s.startswith("~") or lower.endswith(FONT_SUFFIXES))


def _is_font_param(param: str) -> bool:
    return param.endswith("font")


def sequence_files(path: Path) -> List[Path]:
    """The files of an image sequence whose name holds ``{frame}`` or ``{frame:06}``."""
    parts = FRAME.split(path.name)
    # split() with two groups yields: text, zero, width, text, zero, width, ..., text
    regex = ""
    for i in range(0, len(parts), 3):
        regex += re.escape(parts[i])
        if i + 2 < len(parts):
            width = parts[i + 2]
            regex += r"\d{%d,}" % int(width) if width else r"\d+"
    pattern = re.compile(regex + r"\Z")
    try:
        entries = sorted(os.listdir(path.parent))
    except OSError:
        return []
    return [path.parent / e for e in entries if pattern.match(e) and (path.parent / e).is_file()]


def _encode_member(name: str) -> str:
    return name.replace("%", "%25").replace("#", "%23").replace("?", "%3F")


class _Localizer:
    def __init__(
        self,
        base: Path,
        upload_file: Callable[[Path], str],
        upload_sequence: Callable[[Path, str, List[Path]], str],
    ) -> None:
        self.base = base
        self.upload_file = upload_file
        self.upload_sequence = upload_sequence
        self.done: Dict[str, str] = {}

    def resolve(self, s: str) -> Path:
        if s.lower().startswith("file://"):
            p = Path(url2pathname(unquote(urlparse(s).path)))
        else:
            p = Path(os.path.expanduser(s))
        return p if p.is_absolute() else self.base / p

    def one(self, s: Any, where: str, *, strict: bool, font: bool = False, faces: bool = False, component: bool = False) -> Any:
        if not isinstance(s, str) or not s:
            return s
        if component and s.startswith("$"):
            return s  # a component param reference, filled in on the server
        if is_reference(s):
            return s
        if font and is_family_name(s):
            return s
        if faces and s.lstrip().startswith(("[", "{")):
            return s  # face tracks inline as JSON text
        if not strict and ("\n" in s or "\0" in s or len(s) > 4096):
            return s
        if s in self.done:
            return self.done[s]
        try:
            path = self.resolve(s)
            if FRAME.search(path.name):
                files = sequence_files(path)
                if files:
                    uri = self.upload_sequence(path.parent, path.name, files)
                    out = f"{uri}!/{_encode_member(path.name)}"
                    self.done[s] = out
                    return out
                is_file = False
            else:
                is_file = path.is_file()
        except (OSError, ValueError):
            if strict:
                raise
            return s
        if is_file:
            out = self.upload_file(path)
            self.done[s] = out
            return out
        if strict:
            raise FileNotFoundError(f"{where}: {s!r} is not a file (looked for {path})")
        return s

    def walk(self, v: Any, where: str, *, strict: bool, **kw: Any) -> Any:
        """Strings in ``v``; keyframe values (``keys[].v``) keep ``strict``, other nested strings do not."""
        if isinstance(v, str):
            return self.one(v, where, strict=strict, **kw)
        if isinstance(v, list):
            return [self.walk(x, f"{where}[{i}]", strict=False, **kw) for i, x in enumerate(v)]
        if isinstance(v, dict):
            keys = v.get("keys")
            out = {}
            for k, x in v.items():
                if k == "keys" and isinstance(keys, list):
                    out[k] = [
                        {**key, "v": self.walk(key["v"], f"{where}.keys[{i}].v", strict=strict, **kw)}
                        if isinstance(key, dict) and "v" in key
                        else key
                        for i, key in enumerate(keys)
                    ]
                else:
                    out[k] = self.walk(x, f"{where}.{k}", strict=False, **kw)
            return out
        return v

    def drivers(self, v: Any, where: str, component: bool = False) -> None:
        """Audio drivers (``{"audio": {"src": ...}}``) read a file wherever a param is animated."""
        if isinstance(v, dict):
            audio = v.get("audio")
            if isinstance(audio, dict) and isinstance(audio.get("src"), str):
                audio["src"] = self.one(audio["src"], f"{where}.audio.src", strict=True, component=component)
            for k, x in v.items():
                if k != "audio":
                    self.drivers(x, f"{where}.{k}", component)
        elif isinstance(v, list):
            for i, x in enumerate(v):
                self.drivers(x, f"{where}[{i}]", component)

    def nodes(self, nodes: Any, where: str, components: Mapping[str, Any], in_component: bool) -> None:
        if not isinstance(nodes, dict):
            return
        for node_id, node in nodes.items():
            if not isinstance(node, dict):
                continue
            op = node.get("op")
            params = node.get("params")
            if not isinstance(op, str) or not isinstance(params, dict):
                continue
            at = f"{where}.{node_id}.params"
            for k in params:
                self.drivers(params[k], f"{at}.{k}", in_component)
            if op in components:
                for k in list(params):
                    params[k] = self.walk(params[k], f"{at}.{k}", strict=False, component=in_component)
                continue
            for k in list(params):
                if (op, k) in FILE_PARAMS:
                    params[k] = self.walk(
                        params[k],
                        f"{at}.{k}",
                        strict=True,
                        font=_is_font_param(k),
                        faces=k == "faces",
                        component=in_component,
                    )


def localize(
    doc: Mapping[str, Any],
    base: Path,
    upload_file: Callable[[Path], str],
    upload_sequence: Callable[[Path, str, List[Path]], str],
) -> Dict[str, Any]:
    """A deep copy of ``doc`` with every local file uploaded and replaced by its ``asset://`` uri.

    ``upload_file(path)`` and ``upload_sequence(directory, pattern, files)``
    return the uri to use. A file param naming a missing file raises
    ``FileNotFoundError``; strings in component params are uploaded only
    when they name a file that exists.
    """
    out: Dict[str, Any] = copy.deepcopy(dict(doc))
    loc = _Localizer(Path(base), upload_file, upload_sequence)
    components = out.get("components")
    components = components if isinstance(components, dict) else {}
    loc.nodes(out.get("nodes"), "nodes", components, False)
    for name, comp in components.items():
        if not isinstance(comp, dict):
            continue
        loc.nodes(comp.get("nodes"), f"components.{name}.nodes", components, True)
        defaults = comp.get("params")
        if isinstance(defaults, dict):
            for k in list(defaults):
                loc.drivers(defaults[k], f"components.{name}.params.{k}")
                defaults[k] = loc.walk(defaults[k], f"components.{name}.params.{k}", strict=False)
    meta = out.get("meta")
    fonts = meta.get("fonts") if isinstance(meta, dict) else None
    if isinstance(fonts, list):
        for i, font in enumerate(fonts):
            if isinstance(font, dict) and "src" in font:
                font["src"] = loc.one(font["src"], f"meta.fonts[{i}].src", strict=True)
    return out
