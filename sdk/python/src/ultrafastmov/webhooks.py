"""Verify the webhooks the server sends when a job finishes.

The server POSTs the finished job as JSON with an ``Ultrafast-Signature``
header: ``sha256=`` and the hex HMAC-SHA256 of the raw body, keyed with your
webhook secret (``Ultrafast().me()["webhook_secret"]``)::

    from ultrafastmov import webhooks

    job = webhooks.verify(request.body, request.headers["Ultrafast-Signature"], secret)
    if job["status"] == "rendered":
        print(job["receipt"]["url"])

Verify over the raw bytes before parsing them; ``verify`` also refuses a
delivery whose ``sent_at`` is more than ``tolerance`` seconds away.
"""

from __future__ import annotations

import hashlib
import hmac
import json
import time
from datetime import datetime, timezone
from typing import Any, Dict, Optional, Union

from .errors import WebhookVerificationError

__all__ = ["verify", "sign", "SIGNATURE_HEADER", "EVENT_HEADER", "DELIVERY_HEADER"]

SIGNATURE_HEADER = "Ultrafast-Signature"
EVENT_HEADER = "Ultrafast-Event"
DELIVERY_HEADER = "Ultrafast-Delivery"


def _bytes(x: Union[bytes, bytearray, memoryview, str]) -> bytes:
    return x.encode("utf-8") if isinstance(x, str) else bytes(x)


def sign(body: Union[bytes, str], secret: str) -> str:
    """The ``Ultrafast-Signature`` value for ``body`` (for tests of your handler)."""
    return "sha256=" + hmac.new(_bytes(secret), _bytes(body), hashlib.sha256).hexdigest()


def _parse_time(s: str) -> float:
    text = s.strip()
    if text.endswith(("Z", "z")):
        text = text[:-1] + "+00:00"
    # fromisoformat before 3.11 takes at most 6 fractional digits
    if "." in text:
        head, rest = text.split(".", 1)
        digits = ""
        while rest and rest[0].isdigit():
            digits, rest = digits + rest[0], rest[1:]
        text = f"{head}.{digits[:6].ljust(6, '0')}{rest}"
    dt = datetime.fromisoformat(text)
    if dt.tzinfo is None:
        dt = dt.replace(tzinfo=timezone.utc)
    return dt.timestamp()


def verify(
    body: Union[bytes, str],
    signature_header: Optional[str],
    secret: str,
    tolerance: Optional[float] = 300,
    *,
    now: Optional[float] = None,
) -> Dict[str, Any]:
    """The job a webhook delivered, after checking its signature and age.

    ``body`` is the raw request body, ``signature_header`` the
    ``Ultrafast-Signature`` header and ``secret`` your webhook secret.
    Raises :class:`~ultrafastmov.errors.WebhookVerificationError` on a
    missing or wrong signature, a body that is not a JSON object, or a
    ``sent_at`` more than ``tolerance`` seconds from now (``None`` skips
    the age check).
    """
    if not secret:
        raise WebhookVerificationError("no webhook secret given")
    if not signature_header:
        raise WebhookVerificationError(f"no {SIGNATURE_HEADER} header")
    raw = _bytes(body)
    expected = hmac.new(_bytes(secret), raw, hashlib.sha256).hexdigest()
    ok = False
    for part in signature_header.split(","):
        scheme, _, value = part.strip().partition("=")
        if scheme.strip().lower() == "sha256" and hmac.compare_digest(value.strip().lower().encode("utf-8"), expected.encode("ascii")):
            ok = True
    if not ok:
        raise WebhookVerificationError("webhook signature does not match")
    try:
        payload = json.loads(raw.decode("utf-8"))
    except (ValueError, UnicodeDecodeError) as e:
        raise WebhookVerificationError(f"webhook body is not JSON: {e}") from None
    if not isinstance(payload, dict):
        raise WebhookVerificationError("webhook body is not a JSON object")
    if tolerance is not None:
        sent_at = payload.get("sent_at")
        if not isinstance(sent_at, str):
            raise WebhookVerificationError("webhook body has no sent_at")
        try:
            sent = _parse_time(sent_at)
        except ValueError:
            raise WebhookVerificationError(f"webhook sent_at {sent_at!r} is not an RFC 3339 time") from None
        current = time.time() if now is None else now
        if abs(current - sent) > tolerance:
            raise WebhookVerificationError(
                f"webhook sent_at {sent_at} is {abs(current - sent):.0f} s from now (tolerance {tolerance:g} s)"
            )
    return payload
