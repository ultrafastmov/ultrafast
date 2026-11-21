"""What the client returns: documents, renders, jobs, images, issues, estimates."""

from __future__ import annotations

import copy
import json
import os
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import TYPE_CHECKING, Any, Dict, Iterator, List, Mapping, Optional, Union

from .errors import (
    JobCanceled,
    JobFailed,
    JobTimeout,
    UltrafastError,
    error_from_body,
)

if TYPE_CHECKING:  # pragma: no cover
    from .client import Ultrafast

__all__ = [
    "Document",
    "Render",
    "Video",
    "Job",
    "Image",
    "Issue",
    "Check",
    "Review",
    "Estimate",
    "Asset",
    "load",
]

PathLike = Union[str, "os.PathLike[str]"]


class Document(dict):
    """An ultrafast document: a ``dict`` that remembers where it came from.

    Relative file paths in it (``"src": "footage/a.mp4"``) resolve against
    ``base``, the directory of the file it was loaded from (else the working
    directory at load time).
    """

    path: Optional[Path]
    base: Path

    def __init__(self, *args: Any, path: Optional[PathLike] = None, base: Optional[PathLike] = None, **kwargs: Any) -> None:
        super().__init__(*args, **kwargs)
        self.path = Path(path).resolve() if path is not None else None
        if base is not None:
            self.base = Path(base).resolve()
        elif self.path is not None:
            self.base = self.path.parent
        else:
            self.base = Path.cwd()

    @classmethod
    def load(cls, source: Any) -> "Document":
        """A document from a path, an open file, or a dict."""
        if isinstance(source, Document):
            return source
        if isinstance(source, Mapping):
            return cls(source)
        if hasattr(source, "read"):
            data = json.load(source)
            name = getattr(source, "name", None)
            base = Path(name).parent if isinstance(name, str) else None
            return cls(_expect_object(data, name), base=base)
        if isinstance(source, (str, os.PathLike)):
            path = Path(os.path.expanduser(os.fspath(source)))
            with open(path, "r", encoding="utf-8") as f:
                data = json.load(f)
            return cls(_expect_object(data, path), path=path)
        raise TypeError(f"cannot load a document from {type(source).__name__}")

    def copy(self) -> "Document":  # type: ignore[override]
        """A deep copy that keeps ``path`` and ``base``."""
        return Document(copy.deepcopy(dict(self)), path=self.path, base=self.base)

    def save(self, path: Optional[PathLike] = None, *, indent: Optional[int] = 2) -> Path:
        """Write the document as JSON (to ``path``, else where it was loaded from)."""
        target = Path(path) if path is not None else self.path
        if target is None:
            raise ValueError("this document was not loaded from a file; give a path")
        _atomic_write(target, (json.dumps(self, indent=indent, ensure_ascii=False) + "\n").encode("utf-8"))
        if self.path is None:
            self.path = target.resolve()
        return target

    def __repr__(self) -> str:
        fmt = self.get("format") or {}
        size = f"{fmt.get('width')}x{fmt.get('height')}" if isinstance(fmt, Mapping) else "?"
        nodes = self.get("nodes")
        n = len(nodes) if isinstance(nodes, Mapping) else 0
        where = f" path={str(self.path)!r}" if self.path else ""
        return f"<Document{where} {size} {n} nodes>"


def load(source: Any) -> Document:
    """A :class:`Document` from a path, an open file or a dict."""
    return Document.load(source)


def _expect_object(data: Any, where: Any) -> Dict[str, Any]:
    if not isinstance(data, dict):
        raise ValueError(f"{where}: a document is a JSON object, not {type(data).__name__}")
    return data


def _atomic_write(path: Path, data: bytes) -> None:
    path = Path(path)
    tmp = path.with_name(f".{path.name}.{os.getpid()}.part")
    try:
        with open(tmp, "wb") as f:
            f.write(data)
        os.replace(tmp, path)
    finally:
        if tmp.exists():
            tmp.unlink()


def _save_target(path: PathLike, default_name: str) -> Path:
    p = Path(os.path.expanduser(os.fspath(path)))
    if p.is_dir():
        p = p / default_name
    return p


@dataclass(frozen=True)
class Issue:
    """A problem in a document, from ``check``, ``review`` or ``invalid_document``.

    ``from_`` and ``to`` are the seconds a review issue spans (``start`` and
    ``end`` are the same values).
    """

    message: str
    severity: Optional[str] = None
    node: Optional[str] = None
    kind: Optional[str] = None
    from_: Optional[float] = None
    to: Optional[float] = None
    raw: Dict[str, Any] = field(default_factory=dict, repr=False, compare=False)

    @property
    def start(self) -> Optional[float]:
        return self.from_

    @property
    def end(self) -> Optional[float]:
        return self.to

    @classmethod
    def from_dict(cls, d: Any) -> "Issue":
        if isinstance(d, str):
            return cls(message=d)
        d = dict(d)
        return cls(
            message=str(d.get("message", "")),
            severity=d.get("severity"),
            node=d.get("node"),
            kind=d.get("kind"),
            from_=d.get("from", d.get("start")),
            to=d.get("to", d.get("end")),
            raw=d,
        )

    def __str__(self) -> str:
        parts = [p for p in (self.severity, self.kind, self.node) if p]
        if self.from_ is not None:
            span = f"{self.from_:g}"
            if self.to is not None and self.to != self.from_:
                span += f"-{self.to:g}"
            parts.append(span + " s")
        head = " ".join(parts)
        return f"{head}: {self.message}" if head else self.message


class Check:
    """What ``check`` found. ``ok`` is false when any issue is an error."""

    def __init__(self, data: Mapping[str, Any]) -> None:
        self.raw: Dict[str, Any] = dict(data)
        self.issues: List[Issue] = [Issue.from_dict(i) for i in data.get("issues") or []]
        self.ok: bool = bool(data.get("ok", not any(i.severity == "error" for i in self.issues)))

    @property
    def errors(self) -> List[Issue]:
        return [i for i in self.issues if i.severity == "error"]

    @property
    def warnings(self) -> List[Issue]:
        return [i for i in self.issues if i.severity != "error"]

    def __iter__(self) -> Iterator[Issue]:
        return iter(self.issues)

    def __len__(self) -> int:
        return len(self.issues)

    def __repr__(self) -> str:
        return f"<Check ok={self.ok} issues={len(self.issues)}>"


class Review:
    """What ``review`` found; iterate it for the issues."""

    def __init__(self, data: Mapping[str, Any]) -> None:
        self.raw: Dict[str, Any] = dict(data)
        self.issues: List[Issue] = [Issue.from_dict(i) for i in data.get("issues") or []]
        self.frames: Optional[int] = data.get("frames")
        self.texts: Optional[int] = data.get("texts")
        self.seconds: Optional[float] = data.get("seconds")

    @property
    def errors(self) -> List[Issue]:
        return [i for i in self.issues if i.severity == "error"]

    def __iter__(self) -> Iterator[Issue]:
        return iter(self.issues)

    def __len__(self) -> int:
        return len(self.issues)

    def __repr__(self) -> str:
        return f"<Review issues={len(self.issues)} frames={self.frames} seconds={self.seconds}>"


def _fmt_number(x: float, decimals: int) -> str:
    s = f"{x:.{decimals}f}"
    if "." in s:
        s = s.rstrip("0").rstrip(".")
    return "0" if s in ("-0", "") else s


def resolution_label(width: Optional[int], height: Optional[int]) -> Optional[str]:
    """``1080p`` for 1920x1080 and 1080x1920; ``4K`` for 2160; else ``WxH``."""
    if not width or not height:
        return None
    short = width if height > width else height
    names = {720: "720p", 1080: "1080p", 1440: "1440p", 2160: "4K"}
    return names.get(int(short), f"{int(width)}x{int(height)}")


@dataclass
class Estimate:
    """What a render will cost. ``str(estimate)`` reads like ``8 s of 1080p, 2.1 GPU seconds``,
    or ``$0.004, 8 s of 1080p`` from a server that sets a price."""

    seconds: Optional[float] = None
    usd: Optional[float] = None
    minutes: Optional[float] = None
    width: Optional[int] = None
    height: Optional[int] = None
    fps: Optional[float] = None
    frames: Optional[int] = None
    gpu_seconds: Optional[float] = None
    first_byte_s: Optional[float] = None
    device: Optional[str] = None
    sampled_frames: Optional[int] = None
    raw: Dict[str, Any] = field(default_factory=dict, repr=False, compare=False)

    @classmethod
    def from_dict(cls, d: Mapping[str, Any]) -> "Estimate":
        names = [f for f in cls.__dataclass_fields__ if f != "raw"]
        return cls(**{k: d.get(k) for k in names}, raw=dict(d))

    @property
    def resolution(self) -> Optional[str]:
        return resolution_label(self.width, self.height)

    @property
    def price(self) -> Optional[str]:
        """``usd`` as text: up to three decimals, ``<$0.001`` below that."""
        if self.usd is None:
            return None
        usd = float(self.usd)
        if 0 < usd < 0.0005:
            return "<$0.001"
        return "$" + _fmt_number(usd, 3)

    def __str__(self) -> str:
        what = []
        if self.seconds is not None:
            what.append(f"{_fmt_number(float(self.seconds), 1)} s")
        if self.resolution:
            what.append(self.resolution)
        text = " of ".join(what)
        if self.price is not None:
            return f"{self.price}, {text}" if text else self.price
        if self.gpu_seconds is not None:
            g = float(self.gpu_seconds)
            n = _fmt_number(g, 1 if g >= 1 else 2)
            gpu = f"{n} GPU second" + ("" if n == "1" else "s")
            return f"{text}, {gpu}" if text else gpu
        return text


@dataclass
class Asset:
    """An upload. Use ``uri`` (``asset://...``) in any file param."""

    id: str
    uri: str
    bytes: Optional[int] = None
    sha256: Optional[str] = None
    filename: Optional[str] = None
    content_type: Optional[str] = None
    created_at: Optional[str] = None
    raw: Dict[str, Any] = field(default_factory=dict, repr=False, compare=False)

    @classmethod
    def from_dict(cls, d: Mapping[str, Any]) -> "Asset":
        return cls(
            id=str(d.get("id", "")),
            uri=str(d.get("uri", "")),
            bytes=d.get("bytes"),
            sha256=d.get("sha256"),
            filename=d.get("filename"),
            content_type=d.get("content_type"),
            created_at=d.get("created_at"),
            raw=dict(d),
        )

    def __str__(self) -> str:
        return self.uri


class Image(bytes):
    """A PNG or JPEG from ``frame`` or ``sheet``: the bytes, plus ``save``."""

    content_type: str
    receipt: Dict[str, Any]

    def __new__(cls, data: bytes, content_type: str = "image/png", receipt: Optional[Mapping[str, Any]] = None) -> "Image":
        self = super().__new__(cls, data)
        self.content_type = content_type
        self.receipt = dict(receipt or {})
        return self

    @property
    def format(self) -> str:
        return "jpeg" if "jpeg" in self.content_type or self[:3] == b"\xff\xd8\xff" else "png"

    def save(self, path: PathLike) -> Path:
        target = _save_target(path, "frame." + ("jpg" if self.format == "jpeg" else "png"))
        _atomic_write(target, bytes(self))
        return target

    def _repr_png_(self) -> Optional[bytes]:
        return bytes(self) if self.format == "png" else None

    def _repr_jpeg_(self) -> Optional[bytes]:
        return bytes(self) if self.format == "jpeg" else None

    def __repr__(self) -> str:
        return f"<Image {self.content_type} {len(self)} bytes>"


class Render:
    """A rendered video: its receipt, and ``save`` to download it.

    ``url`` is the signed download link (null when the video went to an
    ``output.uri`` the server cannot sign for); an inline render carries
    the file itself in ``content`` (an mp4, a webm, a gif, a zip of frames:
    ``video["format"]`` says which).
    """

    def __init__(self, receipt: Mapping[str, Any], content: Optional[bytes] = None, client: Optional["Ultrafast"] = None) -> None:
        self.receipt: Dict[str, Any] = dict(receipt)
        self.content = content
        self._client = client

    def _get(self, key: str) -> Any:
        if key in self.receipt:
            return self.receipt[key]
        obj = self.receipt.get("object") or {}
        video = self.receipt.get("video") or {}
        if key == "bytes":
            return obj.get("size") if obj.get("size") is not None else (len(self.content) if self.content is not None else None)
        if key == "sha256":
            return obj.get("sha256")
        if key in ("duration", "frames"):
            return video.get(key)
        return None

    @property
    def id(self) -> Optional[str]:
        return self._get("id")

    @property
    def status(self) -> Optional[str]:
        return self._get("status")

    @property
    def url(self) -> Optional[str]:
        return self._get("url")

    @property
    def url_expires_at(self) -> Optional[str]:
        return self._get("url_expires_at")

    @property
    def sha256(self) -> Optional[str]:
        return self._get("sha256")

    @property
    def bytes(self) -> Optional[int]:
        return self._get("bytes")

    @property
    def duration(self) -> Optional[float]:
        return self._get("duration")

    @property
    def frames(self) -> Optional[int]:
        return self._get("frames")

    @property
    def minutes(self) -> Optional[float]:
        return self._get("minutes")

    @property
    def video(self) -> Dict[str, Any]:
        return dict(self.receipt.get("video") or {})

    @property
    def object(self) -> Dict[str, Any]:
        return dict(self.receipt.get("object") or {})

    @property
    def filename(self) -> str:
        """The file's name: the stored object's, else ``film.<format>``
        (``frames.zip`` for an image sequence)."""
        url = self.url
        if url:
            from urllib.parse import unquote, urlsplit

            name = unquote(urlsplit(url).path.rsplit("/", 1)[-1])
            if "." in name:
                return name
        fmt = str(self.video.get("format") or "mp4")
        return "frames.zip" if fmt in ("png", "exr") else f"film.{fmt}"

    def save(self, path: PathLike, *, verify: bool = True) -> Path:
        """Write the file to ``path`` (a file, or a directory to put
        :attr:`filename` in).

        Downloads stream to a temporary file beside ``path`` and are checked
        against the receipt's sha256 before they replace it.
        """
        target = _save_target(path, self.filename)
        if self.content is not None:
            if verify and self.sha256:
                import hashlib

                got = hashlib.sha256(self.content).hexdigest()
                if got != self.sha256:
                    from .errors import IntegrityError

                    raise IntegrityError(f"inline video sha256 {got} does not match the receipt's {self.sha256}")
            _atomic_write(target, self.content)
            return target
        if not self.url:
            where = self.object.get("uri")
            raise UltrafastError(
                "this render has no download URL" + (f"; the video is at {where}" if where else "")
            )
        if self._client is None:
            from .client import Ultrafast

            self._client = Ultrafast()
        self._client._transport.download(self.url, target, sha256=self.sha256 if verify else None)
        return target

    def __repr__(self) -> str:
        return f"<Render {self.id or ''} {self.status} {self.duration} s {self.bytes} bytes>"


Video = Render

_DONE = ("rendered", "failed", "canceled")


class Job:
    """A render job. ``wait()`` polls until it finishes and returns the :class:`Render`."""

    def __init__(self, data: Mapping[str, Any], client: "Ultrafast") -> None:
        self.data: Dict[str, Any] = dict(data)
        self._client = client

    @property
    def id(self) -> str:
        return str(self.data.get("id", ""))

    @property
    def status(self) -> Optional[str]:
        return self.data.get("status")

    @property
    def done(self) -> bool:
        return self.status in _DONE

    @property
    def created_at(self) -> Optional[str]:
        return self.data.get("created_at")

    @property
    def started_at(self) -> Optional[str]:
        return self.data.get("started_at")

    @property
    def finished_at(self) -> Optional[str]:
        return self.data.get("finished_at")

    @property
    def webhook(self) -> Optional[str]:
        return self.data.get("webhook")

    @property
    def progress(self) -> Optional[Dict[str, Any]]:
        return self.data.get("progress")

    @property
    def receipt(self) -> Optional[Dict[str, Any]]:
        return self.data.get("receipt")

    @property
    def error(self) -> Optional[Dict[str, Any]]:
        return self.data.get("error")

    @property
    def render(self) -> Optional[Render]:
        """The :class:`Render` once the job has rendered, else None."""
        r = self.receipt
        return Render(r, client=self._client) if self.status == "rendered" and r else None

    def refresh(self) -> "Job":
        self.data = self._client._get_json(f"/v1/jobs/{_q(self.id)}", retry=True)
        return self

    def cancel(self) -> "Job":
        """Cancel a queued job (``Conflict`` once it has started)."""
        self.data = self._client._request_json("DELETE", f"/v1/jobs/{_q(self.id)}", retry=True)
        return self

    def wait(self, poll: float = 2.0, timeout: Optional[float] = None) -> Render:
        """Poll every ``poll`` seconds until the job finishes; the :class:`Render`.

        Raises the job's error (e.g. ``InvalidDocument``) if it failed,
        ``JobCanceled`` if it was canceled and ``JobTimeout`` after ``timeout``
        seconds.
        """
        start = time.monotonic()
        while True:
            if not self.done:
                self.refresh()
            if self.status == "rendered":
                render = self.render
                if render is None:
                    raise UltrafastError(f"job {self.id} rendered without a receipt")
                return render
            if self.status == "failed":
                if self.error:
                    err = error_from_body(self.error, default=JobFailed)
                else:
                    err = JobFailed(f"job {self.id} failed")
                err.job = self  # type: ignore[attr-defined]
                raise err
            if self.status == "canceled":
                err = JobCanceled(f"job {self.id} was canceled")
                err.job = self  # type: ignore[attr-defined]
                raise err
            if timeout is not None and time.monotonic() - start >= timeout:
                err = JobTimeout(f"job {self.id} is still {self.status} after {timeout:g} s")
                err.job = self  # type: ignore[attr-defined]
                raise err
            delay = poll
            if timeout is not None:
                delay = max(0.0, min(poll, timeout - (time.monotonic() - start)))
            time.sleep(delay)

    def save(self, path: PathLike, *, poll: float = 2.0, timeout: Optional[float] = None) -> Path:
        """Wait for the job, then download the video to ``path``."""
        return self.wait(poll=poll, timeout=timeout).save(path)

    def __repr__(self) -> str:
        prog = self.progress or {}
        extra = f" {prog.get('frames')}/{prog.get('of')}" if prog.get("of") else ""
        return f"<Job {self.id} {self.status}{extra}>"


def _q(s: str) -> str:
    from urllib.parse import quote

    return quote(s, safe="")
