"""Errors raised by the client.

Every error the server answers is ``{"error": {"status", "code", "message",
"retry", ...}}``; it becomes an :class:`UltrafastError` subclass chosen by
``code`` (then by HTTP status) that keeps the whole body. Extra fields of an
error (``location``, ``node``, ``param``, ``limit``...) read as attributes.
"""

from __future__ import annotations

import json
from typing import Any, Dict, List, Mapping, Optional, Type

__all__ = [
    "UltrafastError",
    "APIConnectionError",
    "BadRequest",
    "InvalidDocument",
    "Unauthorized",
    "PaymentRequired",
    "Forbidden",
    "NotFound",
    "Conflict",
    "TooLarge",
    "InputUnavailable",
    "Busy",
    "RateLimited",
    "ServerError",
    "Unavailable",
    "DeadlineExceeded",
    "JobFailed",
    "JobCanceled",
    "JobTimeout",
    "IntegrityError",
    "WebhookVerificationError",
]


class UltrafastError(Exception):
    """Base class of every error this package raises.

    ``status`` is the HTTP status (None when no response came back),
    ``code`` the API's error code, ``retry`` whether the same request may
    succeed later, ``body`` the full parsed response body and ``error`` its
    ``"error"`` object.
    """

    def __init__(
        self,
        message: str,
        *,
        status: Optional[int] = None,
        code: Optional[str] = None,
        retry: bool = False,
        body: Optional[Mapping[str, Any]] = None,
        headers: Optional[Mapping[str, str]] = None,
    ) -> None:
        super().__init__(message)
        self.message = message
        self.status = status
        self.code = code
        self.retry = retry
        self.body: Dict[str, Any] = dict(body or {})
        err = self.body.get("error")
        self.error: Dict[str, Any] = dict(err) if isinstance(err, Mapping) else {}
        self.headers: Dict[str, str] = dict(headers or {})

    def __getattr__(self, name: str) -> Any:
        # Extra fields of the error object: e.location, e.node, e.limit...
        if name.startswith("_"):
            raise AttributeError(name)
        err = self.__dict__.get("error") or {}
        if name in err:
            return err[name]
        raise AttributeError(f"{type(self).__name__} has no attribute {name!r}")

    def __str__(self) -> str:
        head = []
        if self.status is not None:
            head.append(str(self.status))
        if self.code:
            head.append(self.code)
        return f"{' '.join(head)}: {self.message}" if head else self.message

    def __repr__(self) -> str:
        return f"{type(self).__name__}(status={self.status!r}, code={self.code!r}, message={self.message!r})"


class APIConnectionError(UltrafastError):
    """No answer: the connection failed, was reset or timed out."""


class BadRequest(UltrafastError):
    """400: malformed JSON or missing or invalid request fields."""


class InvalidDocument(BadRequest):
    """400 ``invalid_document``: the document fails its check; see ``issues``."""

    @property
    def issues(self) -> List[Any]:
        from .models import Issue

        return [Issue.from_dict(i) for i in self.error.get("issues") or []]

    def __str__(self) -> str:
        lines = [super().__str__()]
        for issue in self.issues:
            lines.append(f"  {issue}")
        return "\n".join(lines)


class Unauthorized(UltrafastError):
    """401: missing, unknown or expired key or token."""


class PaymentRequired(UltrafastError):
    """402 (Cloud): out of free minutes and no card on file."""


class Forbidden(UltrafastError):
    """403: an input or output outside the caller's prefixes, or a local path."""


class NotFound(UltrafastError):
    """404: no such route, job, upload or template (or not the caller's)."""


class Conflict(UltrafastError):
    """409: ``output_conflict`` or ``job_started``."""


class TooLarge(UltrafastError):
    """413 ``over_limit``: ``limit``, ``value`` and ``max`` say which."""


class InputUnavailable(UltrafastError):
    """422: an input could not be fetched, is not at that version or failed its check."""


class Busy(UltrafastError):
    """429: the server's queue or the caller's concurrency is full."""


RateLimited = Busy


class ServerError(UltrafastError):
    """5xx: ``render_failed`` (500) and other server-side failures."""


class Unavailable(ServerError):
    """503: not ready, shutting down, or the store is unreachable."""


class DeadlineExceeded(ServerError):
    """504: the deadline passed before the output was written."""


class CloudOnly(UltrafastError):
    """501 ``cloud_only``: a feature only ultrafast Cloud has, asked of a
    self-hosted ``ultrafast serve``. ``e.feature`` names it: ``jobs`` (jobs
    and webhooks), ``bucket_output`` (``uri=``) or ``bucket_inputs``
    (``gs://``, ``s3://``, ``r2://`` references)."""


class JobFailed(UltrafastError):
    """A job finished as ``failed`` and its error has no more specific class."""


class JobCanceled(UltrafastError):
    """A job was canceled before it rendered."""


class JobTimeout(UltrafastError, TimeoutError):
    """``Job.wait`` gave up before the job finished."""


class IntegrityError(UltrafastError):
    """A downloaded or uploaded file's sha256 does not match the server's."""


class WebhookVerificationError(UltrafastError, ValueError):
    """A webhook's signature is wrong or its ``sent_at`` is too old."""


_BY_CODE: Dict[str, Type[UltrafastError]] = {
    "bad_request": BadRequest,
    "invalid_document": InvalidDocument,
    "unauthorized": Unauthorized,
    "payment_required": PaymentRequired,
    "input_not_allowed": Forbidden,
    "output_not_allowed": Forbidden,
    "local_path_not_allowed": Forbidden,
    "not_found": NotFound,
    "output_conflict": Conflict,
    "job_started": Conflict,
    "over_limit": TooLarge,
    "input_unavailable": InputUnavailable,
    "busy": Busy,
    "render_failed": ServerError,
    "unavailable": Unavailable,
    "deadline_exceeded": DeadlineExceeded,
    "cloud_only": CloudOnly,
}

_BY_STATUS: Dict[int, Type[UltrafastError]] = {
    400: BadRequest,
    401: Unauthorized,
    402: PaymentRequired,
    403: Forbidden,
    404: NotFound,
    409: Conflict,
    413: TooLarge,
    422: InputUnavailable,
    429: Busy,
    500: ServerError,
    501: CloudOnly,
    503: Unavailable,
    504: DeadlineExceeded,
}


def error_from_body(
    body: Any,
    status: Optional[int] = None,
    headers: Optional[Mapping[str, str]] = None,
    default: Type[UltrafastError] = UltrafastError,
) -> UltrafastError:
    """The exception for an error body (``{"error": {...}}`` or the inner object)."""
    if isinstance(body, Mapping) and "error" not in body and "code" in body:
        body = {"error": dict(body)}
    err = body.get("error") if isinstance(body, Mapping) else None
    if isinstance(err, Mapping):
        code = err.get("code")
        status = err.get("status", status)
        message = str(err.get("message") or code or "error")
        retry = bool(err.get("retry", False))
    else:
        code = None
        message = ""
        if isinstance(body, Mapping) and body:
            message = json.dumps(body)[:500]
        elif isinstance(body, str):
            message = body.strip()[:500]
        message = message or f"HTTP {status}"
        retry = status in (429, 502, 503)
        body = body if isinstance(body, Mapping) else {}
    if isinstance(status, str) and status.isdigit():
        status = int(status)
    cls = _BY_CODE.get(code or "")
    if cls is None and isinstance(status, int):
        cls = _BY_STATUS.get(status) or (ServerError if status >= 500 else None)
    cls = cls or default
    return cls(message, status=status, code=code, retry=retry, body=body, headers=headers)


def error_from_response(status: int, raw: bytes, headers: Optional[Mapping[str, str]] = None) -> UltrafastError:
    """The exception for a non-2xx HTTP response."""
    body: Any
    try:
        body = json.loads(raw.decode("utf-8")) if raw else {}
    except (ValueError, UnicodeDecodeError):
        body = raw.decode("utf-8", "replace")
    return error_from_body(body, status, headers)
