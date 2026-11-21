"""HTTP over the standard library: JSON calls, retries, streamed uploads and downloads."""

from __future__ import annotations

import hashlib
import http.client
import json
import os
import random
import socket
import time
import urllib.error
import urllib.request
from email.message import Message
from pathlib import Path
from typing import Any, Callable, Dict, Iterable, Mapping, Optional, Tuple, Union
from urllib.parse import urlencode

from ._version import __version__
from .errors import APIConnectionError, IntegrityError, UltrafastError, error_from_response

CHUNK = 1 << 20
USER_AGENT = f"ultrafastmov-python/{__version__}"

Body = Union[None, bytes, Iterable[bytes]]


class Response:
    def __init__(self, status: int, headers: Message, body: bytes) -> None:
        self.status = status
        self.headers = headers
        self.body = body

    @property
    def content_type(self) -> str:
        return (self.headers.get("Content-Type") or "").split(";")[0].strip().lower()

    def json(self) -> Any:
        return json.loads(self.body.decode("utf-8")) if self.body else {}


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    """API calls do not follow redirects (a redirected POST would lose its body and keep the key)."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):  # type: ignore[no-untyped-def]
        return None


def _headers_dict(h: Optional[Message]) -> Dict[str, str]:
    return {k: v for k, v in (h.items() if h is not None else [])}


def _retry_after(headers: Optional[Message]) -> Optional[float]:
    value = headers.get("Retry-After") if headers is not None else None
    try:
        return max(0.0, float(value)) if value else None
    except ValueError:
        return None


class Transport:
    def __init__(self, base_url: str, api_key: Optional[str], timeout: float, max_retries: int) -> None:
        self.base_url = base_url.rstrip("/")
        self.api_key = api_key
        self.timeout = timeout
        self.max_retries = max_retries
        self._api = urllib.request.build_opener(_NoRedirect())
        self._files = urllib.request.build_opener()
        self.sleep = time.sleep

    def url(self, path: str, query: Optional[Mapping[str, Any]] = None) -> str:
        url = self.base_url + path
        if query:
            q = {k: v for k, v in query.items() if v is not None}
            if q:
                url += "?" + urlencode(q)
        return url

    def _backoff(self, attempt: int, headers: Optional[Message]) -> float:
        after = _retry_after(headers)
        if after is not None:
            return min(after, 30.0)
        return min(8.0, 0.5 * (2**attempt)) * (0.5 + random.random() / 2)

    def request(
        self,
        method: str,
        path: str,
        *,
        json_body: Any = None,
        body: Optional[Callable[[], Body]] = None,
        headers: Optional[Mapping[str, str]] = None,
        query: Optional[Mapping[str, Any]] = None,
        retry: bool = False,
        auth: bool = True,
    ) -> Response:
        """One API call. ``body`` is a factory so a retry can send it again."""
        hdrs = {"User-Agent": USER_AGENT, "Accept": "application/json, */*"}
        if auth and self.api_key:
            hdrs["Authorization"] = f"Bearer {self.api_key}"
        data_factory: Optional[Callable[[], Body]] = body
        if json_body is not None:
            payload = json.dumps(json_body, ensure_ascii=False, separators=(",", ":"), allow_nan=False).encode("utf-8")
            hdrs["Content-Type"] = "application/json"
            data_factory = lambda: payload  # noqa: E731
        hdrs.update(headers or {})
        url = self.url(path, query)
        attempts = self.max_retries + 1 if retry else 1
        for attempt in range(attempts):
            data = data_factory() if data_factory is not None else None
            req = urllib.request.Request(url, data=data, headers=hdrs, method=method)  # type: ignore[arg-type]
            last = attempt == attempts - 1
            try:
                with self._api.open(req, timeout=self.timeout) as resp:
                    return Response(resp.status, resp.headers, resp.read())
            except urllib.error.HTTPError as e:
                try:
                    raw = e.read()
                except Exception:  # noqa: BLE001
                    raw = b""
                finally:
                    e.close()
                err = error_from_response(e.code, raw, _headers_dict(e.headers))
                if 300 <= e.code < 400:
                    err = UltrafastError(
                        f"unexpected redirect to {e.headers.get('Location')}; check ULTRAFAST_BASE_URL",
                        status=e.code,
                        headers=_headers_dict(e.headers),
                    )
                if not last and err.retry and e.code in (429, 502, 503):
                    self.sleep(self._backoff(attempt, e.headers))
                    continue
                if isinstance(err, UltrafastError) and err.status == 401 and not self.api_key:
                    err.message += " (no API key: set ULTRAFAST_API_KEY or pass api_key=)"
                raise err from None
            except (urllib.error.URLError, http.client.HTTPException, ConnectionError, socket.timeout, TimeoutError) as e:
                if not last:
                    self.sleep(self._backoff(attempt, None))
                    continue
                reason = getattr(e, "reason", e)
                raise APIConnectionError(f"{method} {url}: {reason}") from e
        raise AssertionError("unreachable")  # pragma: no cover

    def download(self, url: str, target: Path, *, sha256: Optional[str] = None) -> Tuple[int, str]:
        """Stream ``url`` to ``target`` (via a temporary file beside it), checking ``sha256``.

        Signed URLs carry their own authorization, so no key is sent.
        """
        target = Path(target)
        tmp = target.with_name(f".{target.name}.{os.getpid()}.part")
        attempts = self.max_retries + 1
        try:
            for attempt in range(attempts):
                last = attempt == attempts - 1
                req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
                h = hashlib.sha256()
                size = 0
                try:
                    with self._files.open(req, timeout=self.timeout) as resp, open(tmp, "wb") as f:
                        while True:
                            chunk = resp.read(CHUNK)
                            if not chunk:
                                break
                            h.update(chunk)
                            size += len(chunk)
                            f.write(chunk)
                except urllib.error.HTTPError as e:
                    try:
                        err = error_from_response(e.code, e.read() if e.fp else b"", _headers_dict(e.headers))
                    finally:
                        e.close()
                    if not last and e.code in (429, 500, 502, 503, 504):
                        self.sleep(self._backoff(attempt, e.headers))
                        continue
                    raise err from None
                except (urllib.error.URLError, http.client.HTTPException, ConnectionError, socket.timeout, TimeoutError) as e:
                    if not last:
                        self.sleep(self._backoff(attempt, None))
                        continue
                    raise APIConnectionError(f"GET {url.split('?')[0]}: {getattr(e, 'reason', e)}") from e
                digest = h.hexdigest()
                if sha256 and digest != sha256.lower():
                    raise IntegrityError(f"downloaded {size} bytes with sha256 {digest}, the receipt says {sha256}")
                os.replace(tmp, target)
                return size, digest
        finally:
            if tmp.exists():
                tmp.unlink()
        raise AssertionError("unreachable")  # pragma: no cover
