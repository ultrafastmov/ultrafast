"""The client: ``Ultrafast().render(doc).save("film.mp4")``."""

from __future__ import annotations

import hashlib
import json
import mimetypes
import os
import re
import shutil
import tempfile
import threading
import uuid
import zipfile
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import (
    IO,
    TYPE_CHECKING,
    Any,
    Callable,
    Dict,
    Iterator,
    List,
    Mapping,
    Optional,
    Sequence,
    Tuple,
    Union,
    overload,
)
from urllib.parse import quote

from . import _files
from ._http import CHUNK, Transport
from .errors import Conflict, IntegrityError, InvalidDocument
from .models import Asset, Check, Document, Estimate, Image, Job, Render, Review

if TYPE_CHECKING:  # pragma: no cover
    from . import types

    DocumentLike = Union[Document, types.Document, Mapping[str, Any], str, "os.PathLike[str]"]
else:
    DocumentLike = Any

__all__ = ["Ultrafast", "Jobs", "DEFAULT_BASE_URL"]

#: A local ``ultrafast serve`` on its default port.
DEFAULT_BASE_URL = "http://127.0.0.1:8080"
#: Seconds to wait on a response. Synchronous renders may take up to 10 minutes.
DEFAULT_TIMEOUT = 660.0

_ID = re.compile(r"^[A-Za-z0-9._:\-]{1,128}$")
_TYPES = {
    ".mp4": "video/mp4",
    ".mov": "video/quicktime",
    ".webm": "video/webm",
    ".m4a": "audio/mp4",
    ".mp3": "audio/mpeg",
    ".wav": "audio/wav",
    ".png": "image/png",
    ".jpg": "image/jpeg",
    ".jpeg": "image/jpeg",
    ".webp": "image/webp",
    ".gif": "image/gif",
    ".svg": "image/svg+xml",
    ".json": "application/json",
    ".lottie": "application/zip",
    ".zip": "application/zip",
    ".cube": "text/plain",
    ".srt": "application/x-subrip",
    ".vtt": "text/vtt",
    ".glb": "model/gltf-binary",
    ".gltf": "model/gltf+json",
    ".ttf": "font/ttf",
    ".otf": "font/otf",
    ".ttc": "font/collection",
    ".woff": "font/woff",
    ".woff2": "font/woff2",
}


def _content_type(name: str) -> str:
    ext = os.path.splitext(name)[1].lower()
    return _TYPES.get(ext) or mimetypes.guess_type(name)[0] or "application/octet-stream"


def _sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(CHUNK), b""):
            h.update(chunk)
    return h.hexdigest()


def _new_id(prefix: str) -> str:
    return f"{prefix}_{uuid.uuid4().hex}"


def _attempt(value: Any) -> Optional[Dict[str, str]]:
    """``attempt=True`` (both ids made up), a render id, or ``{"render_id", "attempt_id"}``."""
    if value is None or value is False:
        return None
    if value is True:
        out = {"render_id": _new_id("render"), "attempt_id": _new_id("attempt")}
    elif isinstance(value, str):
        out = {"render_id": value, "attempt_id": _new_id("attempt")}
    elif isinstance(value, Mapping):
        out = {
            "render_id": str(value.get("render_id") or _new_id("render")),
            "attempt_id": str(value.get("attempt_id") or _new_id("attempt")),
        }
    else:
        raise TypeError("attempt is True, a render id, or {'render_id', 'attempt_id'}")
    for k, v in out.items():
        if not _ID.match(v):
            raise ValueError(f"{k} {v!r}: 1 to 128 characters of A-Z a-z 0-9 . _ : -")
    return out


def _deadline(value: Any) -> Optional[str]:
    """A deadline as RFC 3339 UTC: a datetime, seconds from now, or a string as is."""
    if value is None:
        return None
    if isinstance(value, str):
        return value
    if isinstance(value, (int, float)):
        value = datetime.now(timezone.utc) + timedelta(seconds=float(value))
    if isinstance(value, datetime):
        utc = value.astimezone(timezone.utc)
        return utc.strftime("%Y-%m-%dT%H:%M:%S.") + f"{utc.microsecond // 1000:03d}Z"
    raise TypeError("deadline is a datetime, seconds from now, or an RFC 3339 string")


def _receipt_header(value: Optional[str]) -> Dict[str, Any]:
    if not value:
        return {}
    try:
        data = json.loads(value)
    except ValueError:
        return {}
    return data if isinstance(data, dict) else {}


#: An output size: ``"720p"``, ``"1080p"``, ``"4k"``, ``"1280x720"`` or ``(w, h)``.
SizeLike = Union[str, Sequence[int]]

#: The export settings ``render``, ``submit`` and ``estimate`` take.
SETTINGS = (
    "format",
    "codec",
    "quality",
    "crf",
    "bitrate",
    "prores_profile",
    "alpha",
    "size",
    "scale",
    "fps",
    "audio",
    "audio_codec",
    "audio_bitrate",
    "loops",
)


def _settings(settings: Mapping[str, Any]) -> Dict[str, Any]:
    """Export settings as the API spells them (unknown names refused here, not by the server)."""
    unknown = sorted(set(settings) - set(SETTINGS))
    if unknown:
        raise TypeError(f"unknown export setting(s) {', '.join(unknown)} ({', '.join(SETTINGS)})")
    out: Dict[str, Any] = {}
    for k, v in settings.items():
        if v is None:
            continue
        if k == "size" and not isinstance(v, str):
            v = [int(x) for x in v]
        out[k] = v
    return out


def _drop_none(d: Mapping[str, Any]) -> Dict[str, Any]:
    return {k: v for k, v in d.items() if v is not None}


class Jobs:
    """``uf.jobs``: render jobs by id."""

    def __init__(self, client: "Ultrafast") -> None:
        self._client = client

    def get(self, id: str) -> Job:
        return Job(self._client._get_json(f"/v1/jobs/{quote(id, safe='')}", retry=True), self._client)

    def list(self, limit: Optional[int] = None) -> List[Job]:
        """The caller's most recent jobs first."""
        data = self._client._get_json("/v1/jobs", query={"limit": limit}, retry=True)
        return [Job(j, self._client) for j in (data.get("jobs") or [])]

    def cancel(self, id: str) -> Job:
        """Cancel a queued job (``Conflict`` once it has started)."""
        return Job({"id": id}, self._client).cancel()

    def create(self, doc: DocumentLike, *, webhook: Optional[str] = None, **output: Any) -> Job:
        return self._client.submit(doc, webhook=webhook, **output)

    def __iter__(self) -> Iterator[Job]:
        return iter(self.list())


class Ultrafast:
    """A client for an ultrafast render service: your own ``ultrafast serve``.

    ``api_key`` defaults to ``ULTRAFAST_API_KEY`` (the key the server
    prints at first start); ``base_url`` to ``ULTRAFAST_BASE_URL``, else
    ``http://127.0.0.1:8080``, a server on this machine. Another machine:
    ``Ultrafast(base_url="http://gpu-box:8080")``.

    Local files a document names (``"src": "footage/a.mp4"``) are uploaded
    before each call, once per client (by sha256 and size), and a copy of
    the document that points at the uploads is sent; your document is
    never changed.
    """

    def __init__(
        self,
        api_key: Optional[str] = None,
        base_url: Optional[str] = None,
        timeout: float = DEFAULT_TIMEOUT,
        max_retries: int = 2,
    ) -> None:
        key = api_key if api_key is not None else os.environ.get("ULTRAFAST_API_KEY") or None
        url = (base_url or os.environ.get("ULTRAFAST_BASE_URL") or DEFAULT_BASE_URL).strip().rstrip("/")
        if url.endswith("/v1"):
            url = url[: -len("/v1")]
        if not re.match(r"^https?://", url):
            raise ValueError(f"base_url {url!r} must start with http:// or https://")
        self._transport = Transport(url, key, timeout, max_retries)
        self._lock = threading.Lock()
        self._digests: Dict[Tuple[str, int, int], str] = {}
        self._assets: Dict[Tuple[str, int], Asset] = {}
        self._sequences: Dict[Tuple[Any, ...], str] = {}
        self.jobs = Jobs(self)

    @property
    def base_url(self) -> str:
        return self._transport.base_url

    def __repr__(self) -> str:
        return f"<Ultrafast {self.base_url}>"

    # Plumbing

    def _request_json(self, method: str, path: str, body: Any = None, *, query: Optional[Mapping[str, Any]] = None, retry: bool = False, auth: bool = True) -> Any:
        return self._transport.request(method, path, json_body=body, query=query, retry=retry, auth=auth).json()

    def _get_json(self, path: str, *, query: Optional[Mapping[str, Any]] = None, retry: bool = True, auth: bool = True) -> Any:
        return self._request_json("GET", path, query=query, retry=retry, auth=auth)

    def _document(self, doc: DocumentLike) -> Tuple[Mapping[str, Any], Path]:
        if isinstance(doc, Document):
            return doc, doc.base
        if isinstance(doc, (str, os.PathLike)):
            d = Document.load(doc)
            return d, d.base
        if isinstance(doc, Mapping):
            return doc, Path.cwd()
        raise TypeError(f"expected a document (dict or path), not {type(doc).__name__}")

    # Documents

    def load(self, source: Any) -> Document:
        """A document from a path, an open file or a dict; relative paths in it resolve against its directory."""
        return Document.load(source)

    def templates(self) -> List[str]:
        """The names :meth:`new` takes."""
        data = self._get_json("/v1/templates")
        items = data.get("templates", data.get("names", [])) if isinstance(data, Mapping) else data
        return [t if isinstance(t, str) else str(t.get("name")) for t in items or []]

    def new(self, template: str = "vertical") -> Document:
        """A starter document (``vertical``, ``title``, ``square``, ``promo``, ``explainer``...)."""
        data = self._get_json(f"/v1/templates/{quote(template, safe='')}")
        if isinstance(data, Mapping) and isinstance(data.get("document"), Mapping) and "nodes" not in data:
            data = data["document"]
        return Document(data)

    def prepare(self, doc: DocumentLike) -> Dict[str, Any]:
        """The document as it is sent: a copy with local files uploaded and replaced by ``asset://`` uris."""
        d, base = self._document(doc)
        return _files.localize(d, base, self._upload_file_cached, self._upload_sequence_cached)

    # Uploads

    def upload(self, file: Union[str, "os.PathLike[str]", IO[bytes], bytes], filename: Optional[str] = None, content_type: Optional[str] = None) -> Asset:
        """Upload a file (a path, an open binary file or bytes); use the asset's ``uri`` in any file param.

        Files stream from disk; nothing is read into memory whole.
        """
        owned: Optional[IO[bytes]] = None
        opener: Callable[[], IO[bytes]]
        close = False
        if isinstance(file, (str, os.PathLike)):
            path = Path(os.path.expanduser(os.fspath(file)))
            size = path.stat().st_size
            name = filename or path.name
            opener = lambda: open(path, "rb")  # noqa: E731
            close = True
            start = 0
        elif isinstance(file, (bytes, bytearray, memoryview)):
            import io

            data = bytes(file)
            size = len(data)
            name = filename or "upload.bin"
            opener = lambda: io.BytesIO(data)  # noqa: E731
            close = True
            start = 0
        elif hasattr(file, "read"):
            fobj: IO[bytes] = file  # type: ignore[assignment]
            name = filename or os.path.basename(str(getattr(fobj, "name", "") or "")) or "upload.bin"
            seekable = False
            try:
                seekable = fobj.seekable()
            except Exception:  # noqa: BLE001
                seekable = False
            if not seekable:
                owned = tempfile.TemporaryFile()
                shutil.copyfileobj(fobj, owned, CHUNK)
                fobj = owned
                fobj.seek(0)
            start = fobj.tell()
            fobj.seek(0, os.SEEK_END)
            size = fobj.tell() - start
            fobj.seek(start)
            src = fobj

            def opener() -> IO[bytes]:
                src.seek(start)
                return src
        else:
            raise TypeError("upload takes a path, an open binary file or bytes")

        ctype = content_type or _content_type(name)
        boundary = "ultrafast-" + uuid.uuid4().hex
        safe = name.replace("\\", "\\\\").replace('"', '\\"').replace("\r", "").replace("\n", "")
        head = (
            f"--{boundary}\r\n"
            f'Content-Disposition: form-data; name="file"; filename="{safe}"\r\n'
            f"Content-Type: {ctype}\r\n\r\n"
        ).encode("utf-8")
        tail = f"\r\n--{boundary}--\r\n".encode("ascii")
        state: Dict[str, Any] = {}

        def body() -> Iterator[bytes]:
            h = hashlib.sha256()
            state["h"] = h

            def gen() -> Iterator[bytes]:
                yield head
                f = opener()
                try:
                    left = size
                    while left > 0:
                        chunk = f.read(min(CHUNK, left))
                        if not chunk:
                            raise IOError(f"{name} got shorter while it was uploading")
                        h.update(chunk)
                        left -= len(chunk)
                        yield chunk
                finally:
                    if close:
                        f.close()
                yield tail

            return gen()

        try:
            resp = self._transport.request(
                "POST",
                "/v1/uploads",
                body=body,
                headers={
                    "Content-Type": f"multipart/form-data; boundary={boundary}",
                    "Content-Length": str(len(head) + size + len(tail)),
                },
                retry=True,
            )
        finally:
            if owned is not None:
                owned.close()
        asset = Asset.from_dict(resp.json())
        local = state["h"].hexdigest()
        if asset.sha256 and asset.sha256.lower() != local:
            raise IntegrityError(f"uploaded {name} with sha256 {local}, the server stored {asset.sha256}")
        with self._lock:
            self._assets[(local, size)] = asset
        return asset

    def asset(self, id: str) -> Asset:
        """An upload by id (``asset_...``)."""
        return Asset.from_dict(self._get_json(f"/v1/uploads/{quote(id, safe='')}"))

    def _upload_file_cached(self, path: Path) -> str:
        st = path.stat()
        key = (str(path.resolve()), st.st_size, st.st_mtime_ns)
        with self._lock:
            digest = self._digests.get(key)
        if digest is None:
            digest = _sha256_file(path)
            with self._lock:
                self._digests[key] = digest
        with self._lock:
            asset = self._assets.get((digest, st.st_size))
        if asset is None:
            asset = self.upload(path)
        return asset.uri

    def _upload_sequence_cached(self, directory: Path, pattern: str, files: List[Path]) -> str:
        """An image sequence goes up as one zip; the document names its frames as members."""
        stats = tuple((f.name, f.stat().st_size, f.stat().st_mtime_ns) for f in files)
        key = (str(directory.resolve()), pattern, stats)
        with self._lock:
            uri = self._sequences.get(key)
        if uri:
            return uri
        stem = _files.FRAME.sub("", os.path.splitext(pattern)[0]).strip("_-. ") or "frames"
        with tempfile.TemporaryDirectory() as tmp:
            zpath = Path(tmp) / f"{stem}.zip"
            with zipfile.ZipFile(zpath, "w", zipfile.ZIP_STORED, allowZip64=True) as zf:
                for f, (fname, fsize, _) in zip(files, stats):
                    info = zipfile.ZipInfo(fname, date_time=(1980, 1, 1, 0, 0, 0))
                    info.external_attr = 0o644 << 16
                    info.file_size = fsize
                    with open(f, "rb") as src, zf.open(info, "w", force_zip64=fsize >= 1 << 31) as dst:
                        shutil.copyfileobj(src, dst, CHUNK)
            uri = self._upload_file_cached(zpath)
        with self._lock:
            self._sequences[key] = uri
        return uri

    # Rendering

    def _render_body(
        self,
        doc: DocumentLike,
        *,
        inline: Optional[bool],
        uri: Optional[str],
        codec: Optional[str],
        bitrate: Optional[Union[int, str]],
        audio: Optional[bool],
        range: Optional[Sequence[float]],
        name: Optional[str],
        attempt: Optional[Dict[str, str]],
        deadline: Any,
        settings: Optional[Mapping[str, Any]] = None,
    ) -> Dict[str, Any]:
        output = _drop_none(
            {
                "inline": inline,
                "uri": uri,
                "name": name,
                "codec": codec,
                "bitrate": bitrate,
                "audio": audio,
                "range": list(range) if range is not None else None,
                **_settings(settings or {}),
            }
        )
        body: Dict[str, Any] = {"document": self.prepare(doc)}
        if output:
            body["output"] = output
        if attempt:
            body["attempt"] = attempt
        dl = _deadline(deadline)
        if dl:
            body["deadline"] = dl
        return body

    @overload
    def render(
        self,
        doc: DocumentLike,
        *,
        inline: Optional[bool] = ...,
        uri: Optional[str] = ...,
        codec: Optional[str] = ...,
        bitrate: Optional[Union[int, str]] = ...,
        audio: Optional[bool] = ...,
        format: Optional[str] = ...,
        quality: Optional[str] = ...,
        size: Optional[SizeLike] = ...,
        scale: Optional[float] = ...,
        fps: Optional[float] = ...,
        crf: Optional[float] = ...,
        prores_profile: Optional[str] = ...,
        alpha: Optional[bool] = ...,
        audio_codec: Optional[str] = ...,
        audio_bitrate: Optional[Union[int, str]] = ...,
        loops: Optional[int] = ...,
        range: Optional[Sequence[float]] = ...,
        name: Optional[str] = ...,
        webhook: None = ...,
        attempt: Any = ...,
        deadline: Any = ...,
    ) -> Render: ...

    @overload
    def render(
        self,
        doc: DocumentLike,
        *,
        inline: Optional[bool] = ...,
        uri: Optional[str] = ...,
        codec: Optional[str] = ...,
        bitrate: Optional[Union[int, str]] = ...,
        audio: Optional[bool] = ...,
        format: Optional[str] = ...,
        quality: Optional[str] = ...,
        size: Optional[SizeLike] = ...,
        scale: Optional[float] = ...,
        fps: Optional[float] = ...,
        crf: Optional[float] = ...,
        prores_profile: Optional[str] = ...,
        alpha: Optional[bool] = ...,
        audio_codec: Optional[str] = ...,
        audio_bitrate: Optional[Union[int, str]] = ...,
        loops: Optional[int] = ...,
        range: Optional[Sequence[float]] = ...,
        name: Optional[str] = ...,
        webhook: str,
        attempt: Any = ...,
        deadline: Any = ...,
    ) -> Job: ...

    def render(
        self,
        doc: DocumentLike,
        *,
        inline: Optional[bool] = None,
        uri: Optional[str] = None,
        codec: Optional[str] = None,
        bitrate: Optional[Union[int, str]] = None,
        audio: Optional[bool] = None,
        format: Optional[str] = None,
        quality: Optional[str] = None,
        size: Optional[SizeLike] = None,
        scale: Optional[float] = None,
        fps: Optional[float] = None,
        crf: Optional[float] = None,
        prores_profile: Optional[str] = None,
        alpha: Optional[bool] = None,
        audio_codec: Optional[str] = None,
        audio_bitrate: Optional[Union[int, str]] = None,
        loops: Optional[int] = None,
        range: Optional[Sequence[float]] = None,
        name: Optional[str] = None,
        webhook: Optional[str] = None,
        attempt: Any = None,
        deadline: Any = None,
    ) -> Union[Render, Job]:
        """Render a document: MP4 by default, or any format the server writes.

        Without ``webhook`` this waits for the video and returns a
        :class:`Render` (``.save(path)`` downloads it): stored on the server
        with a signed ``url`` by default, in the response with
        ``inline=True`` (up to 32 MiB), or created at ``uri``
        (``s3://``, ``r2://``, ``gs://``) in your bucket. With ``webhook`` it
        creates a job and returns a :class:`Job` at once.

        The export settings are ``ultrafast export``'s: ``format`` (``mp4``,
        ``mov``, ``mkv``, ``webm``, ``gif``, ``png``/``exr`` sequences as a
        zip, or ``wav``/``mp3``/``m4a``/``flac``/``opus`` sound alone; default
        from ``name``'s extension), ``codec`` (``h264``, ``hevc``, ``av1``,
        ``vp9``, ``prores``), ``quality`` (``draft``, ``standard``, ``high``,
        ``master``), ``size`` (``"720p"``, ``"1080p"``, ``"4k"``, ``"WxH"`` or
        ``(w, h)``) or ``scale``, ``fps``, ``crf`` or ``bitrate`` (``"8M"``),
        ``prores_profile``, ``alpha`` (keep transparency), ``audio_codec``,
        ``audio_bitrate`` and ``loops`` (GIF). For example
        ``uf.render(doc, codec="prores", alpha=True, size="4k")``.

        ``range`` is ``[from, to]`` seconds. ``attempt`` (``True``, a render
        id, or ``{"render_id", "attempt_id"}``) is sent with the request and
        makes it retried on ``429``/``503`` and dropped connections; with
        ``uri`` a retry of a finished render answers with the stored receipt.
        """
        if webhook is not None:
            return self.submit(
                doc, webhook=webhook, uri=uri, codec=codec, bitrate=bitrate, audio=audio,
                range=range, name=name, attempt=attempt, deadline=deadline, inline=inline,
                format=format, quality=quality, size=size, scale=scale, fps=fps, crf=crf, prores_profile=prores_profile, alpha=alpha, audio_codec=audio_codec, audio_bitrate=audio_bitrate, loops=loops,
            )
        att = _attempt(attempt)
        settings = dict(format=format, quality=quality, size=size, scale=scale, fps=fps, crf=crf, prores_profile=prores_profile, alpha=alpha, audio_codec=audio_codec, audio_bitrate=audio_bitrate, loops=loops)
        body = self._render_body(
            doc, inline=inline, uri=uri, codec=codec, bitrate=bitrate, audio=audio,
            range=range, name=name, attempt=att, deadline=deadline, settings=settings,
        )
        try:
            resp = self._transport.request("POST", "/v1/render", json_body=body, retry=att is not None)
        except Conflict as e:
            if e.body.get("status") == "already_rendered":
                return Render(e.body, client=self)
            raise
        if resp.content_type == "application/json":
            return Render(resp.json(), client=self)
        receipt = _receipt_header(resp.headers.get("X-Ultrafast-Receipt"))
        sha = resp.headers.get("X-Ultrafast-Sha256")
        if sha and not receipt.get("sha256"):
            receipt["sha256"] = sha
        return Render(receipt, content=resp.body, client=self)

    def submit(
        self,
        doc: DocumentLike,
        *,
        webhook: Optional[str] = None,
        uri: Optional[str] = None,
        codec: Optional[str] = None,
        bitrate: Optional[Union[int, str]] = None,
        audio: Optional[bool] = None,
        range: Optional[Sequence[float]] = None,
        name: Optional[str] = None,
        attempt: Any = None,
        deadline: Any = None,
        inline: Optional[bool] = None,
        format: Optional[str] = None,
        quality: Optional[str] = None,
        size: Optional[SizeLike] = None,
        scale: Optional[float] = None,
        fps: Optional[float] = None,
        crf: Optional[float] = None,
        prores_profile: Optional[str] = None,
        alpha: Optional[bool] = None,
        audio_codec: Optional[str] = None,
        audio_bitrate: Optional[Union[int, str]] = None,
        loops: Optional[int] = None,
    ) -> Job:
        """Create a render job (``POST /v1/jobs``) and return it queued; ``job.wait()`` for the video.

        With ``webhook`` the server POSTs the finished job there, signed
        (verify with :func:`ultrafastmov.webhooks.verify`).
        """
        if inline:
            raise ValueError("inline output is not allowed for jobs")
        att = _attempt(attempt)
        settings = dict(format=format, quality=quality, size=size, scale=scale, fps=fps, crf=crf, prores_profile=prores_profile, alpha=alpha, audio_codec=audio_codec, audio_bitrate=audio_bitrate, loops=loops)
        body = self._render_body(
            doc, inline=None, uri=uri, codec=codec, bitrate=bitrate, audio=audio,
            range=range, name=name, attempt=att, deadline=deadline, settings=settings,
        )
        if webhook is not None:
            body["webhook"] = webhook
        data = self._transport.request("POST", "/v1/jobs", json_body=body, retry=att is not None).json()
        return Job(data, self)

    def job(self, id: str) -> Job:
        """A job by id."""
        return self.jobs.get(id)

    # Looking

    def frame(self, doc: DocumentLike, at: float = 0, format: str = "png") -> Image:
        """The frame at ``at`` seconds, as PNG (or ``format="jpeg"``)."""
        body = {"document": self.prepare(doc), "at": at, "format": format}
        resp = self._transport.request("POST", "/v1/frame", json_body=body, retry=True)
        return Image(resp.body, resp.content_type or "image/png", _receipt_header(resp.headers.get("X-Ultrafast-Receipt")))

    def sheet(
        self,
        doc: DocumentLike,
        count: int = 9,
        at: Optional[Sequence[float]] = None,
        columns: Optional[int] = None,
        format: str = "png",
    ) -> Image:
        """A contact sheet: ``count`` frames spread over the film (or the times in ``at``), labeled."""
        body = _drop_none(
            {
                "document": self.prepare(doc),
                "count": count,
                "at": list(at) if at is not None else None,
                "columns": columns,
                "format": format,
            }
        )
        resp = self._transport.request("POST", "/v1/sheet", json_body=body, retry=True)
        return Image(resp.body, resp.content_type or "image/png", _receipt_header(resp.headers.get("X-Ultrafast-Receipt")))

    def check(self, doc: DocumentLike) -> Check:
        """Problems in the document, static and render-time; ``.ok`` is false when any is an error."""
        try:
            data = self._request_json("POST", "/v1/check", {"document": self.prepare(doc)}, retry=True)
        except InvalidDocument as e:
            return Check({"ok": False, "issues": e.error.get("issues") or []})
        return Check(data)

    def review(self, doc: DocumentLike, safe: Optional[str] = None, step: Optional[float] = None) -> Review:
        """Render the film and report what a viewer would trip on; iterate it for issues.

        ``safe`` is ``"margin"`` (default) or ``"social"``; ``step`` the
        seconds between text probes.
        """
        body = _drop_none({"document": self.prepare(doc), "safe": safe, "step": step})
        return Review(self._request_json("POST", "/v1/review", body, retry=True))

    def estimate(self, doc: DocumentLike, range: Optional[Sequence[float]] = None, **settings: Any) -> Estimate:
        """What rendering will cost; ``print(estimate)`` shows ``8 s of 1080p, 2.1 GPU seconds``.

        Pass the export settings you will render with (``size="4k"``,
        ``quality="high"``, ``fps=60``, ...): minutes count the output size
        and frames are timed at that quality and frame rate.
        """
        output = _settings(settings)
        body = _drop_none(
            {
                "document": self.prepare(doc),
                "range": list(range) if range is not None else None,
                "output": output or None,
            }
        )
        return Estimate.from_dict(self._request_json("POST", "/v1/estimate", body, retry=True))

    # Account and server

    def me(self) -> Dict[str, Any]:
        """``{"caller", "auth", "webhook_secret"}``."""
        return dict(self._get_json("/v1/me"))

    def health(self) -> Dict[str, Any]:
        """The server's health (no key needed)."""
        return dict(self._get_json("/v1/health", auth=False, retry=False))
