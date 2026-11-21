"""ultrafast: the GPU video engine. Documents are JSON; this client renders them to MP4.

    from ultrafastmov import Ultrafast

    uf = Ultrafast()  # your ultrafast serve: ULTRAFAST_BASE_URL, ULTRAFAST_API_KEY
    doc = uf.load("film.json")
    print(uf.estimate(doc))      # 8 s of 1080p, 2.1 GPU seconds
    for issue in uf.review(doc):
        print(issue.kind, issue.node, issue.message)
    uf.render(doc).save("film.mp4")
"""

from . import types, webhooks
from ._version import __version__
from .client import DEFAULT_BASE_URL, Jobs, Ultrafast
from .errors import (
    APIConnectionError,
    BadRequest,
    Busy,
    CloudOnly,
    Conflict,
    DeadlineExceeded,
    Forbidden,
    InputUnavailable,
    IntegrityError,
    InvalidDocument,
    JobCanceled,
    JobFailed,
    JobTimeout,
    NotFound,
    PaymentRequired,
    RateLimited,
    ServerError,
    TooLarge,
    UltrafastError,
    Unauthorized,
    Unavailable,
    WebhookVerificationError,
)
from .models import Asset, Check, Document, Estimate, Image, Issue, Job, Render, Review, Video, load

__all__ = [
    "__version__",
    "Ultrafast",
    "Jobs",
    "DEFAULT_BASE_URL",
    "Document",
    "load",
    "Render",
    "Video",
    "Job",
    "Image",
    "Issue",
    "Check",
    "Review",
    "Estimate",
    "Asset",
    "types",
    "webhooks",
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
    "CloudOnly",
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
