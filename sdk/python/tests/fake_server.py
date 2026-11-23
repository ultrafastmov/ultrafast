"""An in-process stand-in for `ultrafast serve`, following docs/service/API.md.

It renders nothing: videos are deterministic fake bytes and frames a 1x1
PNG. It checks what a real server checks at the edges the client touches:
the bearer key, local paths in file params (403 local_path_not_allowed),
asset ids, create-only outputs and attempts, multipart and raw uploads,
signed download URLs, jobs and signed webhooks.
"""

from __future__ import annotations

import hashlib
import hmac
import json
import re
import threading
import time
import urllib.request
import uuid
from dataclasses import dataclass, field
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any, Dict, List, Optional, Tuple
from urllib.parse import parse_qs, urlparse

PNG_1X1 = bytes.fromhex(
    "89504e470d0a1a0a0000000d4948445200000001000000010806000000"
    "1f15c4890000000d49444154789c6360f8cfc0f01f0005000201e2"
    "21bc330000000049454e44ae426082"
)
JPEG_STUB = b"\xff\xd8\xff\xe0" + b"\x00" * 16 + b"\xff\xd9"
REFS = ("asset://", "https://", "gs://", "s3://", "r2://", "data:")
FILE_PARAMS = {
    "image": ["src"], "video": ["src"], "audio": ["src"], "svg": ["src"], "lottie": ["src"],
    "lut": ["src"], "mesh": ["src", "font"], "captions": ["src", "font"], "clip_camera": ["src", "faces"],
    "block": ["logo", "display_font", "text_font", "mono_font"], "text": ["font", "em_font"],
    "swarm": ["font"], "code": ["font"],
}


def now_rfc3339(offset: float = 0.0) -> str:
    t = datetime.fromtimestamp(time.time() + offset, timezone.utc)
    return t.strftime("%Y-%m-%dT%H:%M:%S.") + f"{t.microsecond // 1000:03d}Z"


def canonical_sha(doc: Any) -> str:
    return hashlib.sha256(json.dumps(doc, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()).hexdigest()


def err(status: int, code: str, message: str, retry: bool = False, **extra: Any) -> Tuple[int, Dict[str, Any]]:
    return status, {"error": {"status": status, "code": code, "message": message, "retry": retry, **extra}}


@dataclass
class Seen:
    method: str
    path: str
    query: Dict[str, List[str]]
    headers: Dict[str, str]
    json: Any
    size: int


@dataclass
class Asset:
    id: str
    uri: str
    data: bytes
    filename: str
    content_type: str


@dataclass
class FakeJob:
    data: Dict[str, Any]
    request: Dict[str, Any]
    deliveries: List[Dict[str, Any]] = field(default_factory=list)


class FakeServer:
    def __init__(self, key: str = "example-key", secret: str = "whsec_example", usd_per_minute: Optional[float] = 0.02) -> None:
        self.key = key
        self.secret = secret
        self.usd_per_minute = usd_per_minute
        self.lock = threading.Lock()
        self.requests: List[Seen] = []
        self.fail: Dict[str, List[Tuple[int, Any]]] = {}
        self.uploads: Dict[str, Asset] = {}
        self.files: Dict[str, bytes] = {}
        self.objects: Dict[str, Dict[str, Any]] = {}
        self.jobs: Dict[str, FakeJob] = {}
        self.job_start_delay = 0.05
        self.job_render_time = 0.1
        self.job_error: Optional[Dict[str, Any]] = None
        self.video_size = 4096
        self.renders = 0
        self.httpd = ThreadingHTTPServer(("127.0.0.1", 0), _Handler)
        self.httpd.daemon_threads = True
        self.httpd.fake = self  # type: ignore[attr-defined]
        self.thread = threading.Thread(target=self.httpd.serve_forever, kwargs={"poll_interval": 0.02}, daemon=True)
        self.thread.start()

    @property
    def url(self) -> str:
        return f"http://127.0.0.1:{self.httpd.server_address[1]}"

    def close(self) -> None:
        self.httpd.shutdown()
        self.httpd.server_close()

    def seen(self, path: str, method: Optional[str] = None) -> List[Seen]:
        return [r for r in self.requests if r.path == path and (method is None or r.method == method)]

    # What the server does

    def sign_url(self, name: str) -> str:
        sig = hmac.new(self.secret.encode(), name.encode(), hashlib.sha256).hexdigest()[:16]
        return f"{self.url}/v1/files/{name}?expires=9999999999&sig={sig}"

    def video(self, doc: Dict[str, Any]) -> bytes:
        seed = canonical_sha(doc).encode()
        head = b"\x00\x00\x00\x18ftypmp42\x00\x00\x00\x00mp42isom"
        body = (seed * (self.video_size // len(seed) + 1))[: max(0, self.video_size - len(head))]
        return head + body

    def local_paths(self, doc: Dict[str, Any]) -> Optional[Tuple[str, str, str]]:
        """The first file param that is not a reference: (node, param, value)."""
        comps = doc.get("components") or {}

        def bad(s: Any, param: str, in_comp: bool) -> bool:
            if not isinstance(s, str) or not s or s.startswith(REFS):
                return False
            if in_comp and s.startswith("$"):
                return False
            if param.endswith("font") and not re.search(r"[/\\]|^[.~]|\.(ttf|otf|ttc|otc|woff2?|dfont)$", s, re.I):
                return False
            if param == "faces" and s.lstrip()[:1] in "[{":
                return False
            return True

        def strings(v: Any) -> List[str]:
            if isinstance(v, str):
                return [v]
            if isinstance(v, list):
                return [s for x in v for s in strings(x)]
            if isinstance(v, dict):
                return [s for x in v.values() for s in strings(x)]
            return []

        def drivers(v: Any) -> List[str]:
            out = []
            if isinstance(v, dict):
                a = v.get("audio")
                if isinstance(a, dict) and isinstance(a.get("src"), str):
                    out.append(a["src"])
                for x in v.values():
                    out += drivers(x)
            elif isinstance(v, list):
                for x in v:
                    out += drivers(x)
            return out

        def nodes(ns: Dict[str, Any], in_comp: bool) -> Optional[Tuple[str, str, str]]:
            for nid, n in (ns or {}).items():
                params = n.get("params") or {}
                for p, v in params.items():
                    for s in drivers(v):
                        if bad(s, "src", in_comp):
                            return nid, p, s
                    if p in FILE_PARAMS.get(n.get("op"), []):
                        for s in strings(v):
                            if bad(s, p, in_comp):
                                return nid, p, s
            return None

        found = nodes(doc.get("nodes") or {}, False)
        for c in comps.values():
            found = found or nodes(c.get("nodes") or {}, True)
        for f in (doc.get("meta") or {}).get("fonts") or []:
            if not found and bad(f.get("src"), "src", False):
                found = ("meta.fonts", "src", f["src"])
        return found

    def unknown_asset(self, doc: Any) -> Optional[str]:
        text = json.dumps(doc)
        for m in re.finditer(r"asset://([A-Za-z0-9]+)", text):
            if f"asset_{m.group(1)}" not in self.uploads:
                return m.group(0)
        return None

    def validate(self, body: Any) -> Optional[Tuple[int, Dict[str, Any]]]:
        if not isinstance(body, dict) or not isinstance(body.get("document"), dict):
            return err(400, "bad_request", "`document` is required")
        doc = body["document"]
        issues = []
        for k in ("format", "nodes", "output"):
            if k not in doc:
                issues.append({"node": None, "severity": "error", "message": f"`{k}` is missing"})
        if "nodes" in doc and doc.get("output") not in (doc.get("nodes") or {}):
            issues.append({"node": doc.get("output"), "severity": "error", "message": "output names a missing node"})
        if issues:
            return err(400, "invalid_document", f"the document has {len(issues)} problems", issues=issues)
        local = self.local_paths(doc)
        if local:
            return err(403, "local_path_not_allowed", "file params must be references", node=local[0], param=local[1], value=local[2])
        missing = self.unknown_asset(doc)
        if missing:
            return err(403, "input_not_allowed", f"{missing} is not this caller's", location=missing)
        return None

    def receipt(self, doc: Dict[str, Any], data: bytes, attempt: Optional[Dict[str, str]], rid: str) -> Dict[str, Any]:
        fmt = doc.get("format") or {}
        dur = float(fmt.get("duration", 0))
        sha = hashlib.sha256(data).hexdigest()
        return {
            "receipt_version": 1,
            "id": rid,
            "status": "rendered",
            "url": None,
            "url_expires_at": None,
            "duration": dur,
            "frames": int(round(dur * float(fmt.get("fps", 30)))),
            "bytes": len(data),
            "sha256": sha,
            "minutes": round(dur / 60, 4),
            "attempt": attempt,
            "video": {"frames": int(round(dur * float(fmt.get("fps", 30)))), "fps": fmt.get("fps"), "width": fmt.get("width"),
                      "height": fmt.get("height"), "duration": dur, "codec": "h264", "audio": True, "range": [0, dur]},
            "document": {"sha256": canonical_sha(doc), "format_version": 1, "meta": doc.get("meta")},
            "created_at": now_rfc3339(),
        }

    def render(self, body: Dict[str, Any], *, job: bool = False) -> Tuple[int, Any, Dict[str, str]]:
        invalid = self.validate(body)
        if invalid:
            return invalid[0], invalid[1], {}
        doc = body["document"]
        output = body.get("output") or {}
        attempt = body.get("attempt") or {"render_id": "render_" + uuid.uuid4().hex[:12], "attempt_id": "attempt_1"}
        rid = "render_" + uuid.uuid4().hex[:20]
        uri = output.get("uri")
        if uri:
            with self.lock:
                existing = self.objects.get(uri)
            if existing:
                same = existing["attempt"] == attempt and existing["doc_sha"] == canonical_sha(doc)
                if same:
                    return 409, {**existing["receipt"], "status": "already_rendered"}, {}
                return (*err(409, "output_conflict", "the output object exists and is not this request's render",
                             object={"uri": uri, "size": existing["receipt"]["bytes"]}), {})
        data = self.video(doc)
        with self.lock:
            self.renders += 1
        receipt = self.receipt(doc, data, attempt, rid)
        if output.get("inline") and not job:
            headers = {"Content-Type": "video/mp4", "X-Ultrafast-Receipt": json.dumps(receipt, separators=(",", ":")),
                       "X-Ultrafast-Sha256": receipt["sha256"]}
            return 200, data, headers
        if uri:
            receipt["object"] = {"uri": uri, "size": len(data), "sha256": receipt["sha256"], "content_type": "video/mp4"}
            with self.lock:
                self.objects[uri] = {"attempt": attempt, "doc_sha": canonical_sha(doc), "receipt": receipt}
            return 200, receipt, {}
        name = f"renders/{rid}/{output.get('name') or 'film.mp4'}"
        with self.lock:
            self.files[name] = data
        receipt["url"] = self.sign_url(name)
        receipt["url_expires_at"] = "2099-01-01T00:00:00Z"
        receipt["object"] = {"uri": f"file:///data/{name}", "size": len(data), "sha256": receipt["sha256"]}
        return 200, receipt, {}

    def run_job(self, job_id: str) -> None:
        time.sleep(self.job_start_delay)
        with self.lock:
            job = self.jobs[job_id]
            if job.data["status"] != "queued":
                return
            job.data.update(status="rendering", started_at=now_rfc3339(), progress={"frames": 0, "of": 30})
        time.sleep(self.job_render_time)
        if self.job_error:
            with self.lock:
                job.data.update(status="failed", finished_at=now_rfc3339(), error=self.job_error)
        else:
            status, receipt, _ = self.render(job.request, job=True)
            with self.lock:
                if status == 200:
                    job.data.update(status="rendered", finished_at=now_rfc3339(), receipt=receipt,
                                    progress={"frames": 30, "of": 30})
                else:
                    job.data.update(status="failed", finished_at=now_rfc3339(), error=receipt["error"])
        hook = job.data.get("webhook")
        if hook:
            payload = {**job.data, "event": f"job.{job.data['status']}", "sent_at": now_rfc3339()}
            raw = json.dumps(payload).encode()
            sig = "sha256=" + hmac.new(self.secret.encode(), raw, hashlib.sha256).hexdigest()
            req = urllib.request.Request(hook, data=raw, method="POST", headers={
                "Content-Type": "application/json", "Ultrafast-Event": payload["event"],
                "Ultrafast-Delivery": "dlv_" + uuid.uuid4().hex[:12], "Ultrafast-Signature": sig})
            try:
                with urllib.request.urlopen(req, timeout=5) as r:
                    job.deliveries.append({"status": r.status})
            except Exception as e:  # noqa: BLE001
                job.deliveries.append({"error": str(e)})

    def upload(self, raw: bytes, content_type: str, query: Dict[str, List[str]]) -> Tuple[int, Any]:
        if content_type.startswith("multipart/form-data"):
            m = re.search(r"boundary=([^;]+)", content_type)
            if not m:
                return err(400, "bad_request", "multipart without a boundary")
            boundary = m.group(1).strip('"').encode()
            data = filename = ctype = None
            for part in raw.split(b"--" + boundary):
                if b"\r\n\r\n" not in part:
                    continue
                head, _, content = part.partition(b"\r\n\r\n")
                head_text = head.decode("utf-8", "replace")
                if 'name="file"' not in head_text:
                    continue
                fn = re.search(r'filename="((?:[^"\\]|\\.)*)"', head_text)
                filename = fn.group(1).replace('\\"', '"').replace("\\\\", "\\") if fn else "file"
                ct = re.search(r"Content-Type:\s*([^\r\n]+)", head_text, re.I)
                ctype = ct.group(1).strip() if ct else "application/octet-stream"
                data = content[:-2] if content.endswith(b"\r\n") else content
            if data is None:
                return err(400, "bad_request", "no part named file")
        else:
            data, filename, ctype = raw, (query.get("filename") or ["file"])[0], content_type
        hexid = uuid.uuid4().hex[:16]
        asset = Asset(f"asset_{hexid}", f"asset://{hexid}", data, filename, ctype or "application/octet-stream")
        with self.lock:
            self.uploads[asset.id] = asset
        return 201, self.asset_json(asset)

    def asset_json(self, a: Asset) -> Dict[str, Any]:
        return {"id": a.id, "uri": a.uri, "bytes": len(a.data), "sha256": hashlib.sha256(a.data).hexdigest(),
                "filename": a.filename, "content_type": a.content_type, "created_at": now_rfc3339()}


class _Handler(BaseHTTPRequestHandler):
    server_version = "fake-ultrafast/0"

    def log_message(self, *args: Any) -> None:  # quiet
        pass

    def do_GET(self) -> None:
        self.route("GET")

    def do_POST(self) -> None:
        self.route("POST")

    def do_DELETE(self) -> None:
        self.route("DELETE")

    def send(self, status: int, body: Any, headers: Optional[Dict[str, str]] = None) -> None:
        if isinstance(body, (bytes, bytearray)):
            data = bytes(body)
            ctype = (headers or {}).get("Content-Type", "application/octet-stream")
        else:
            data = json.dumps(body).encode()
            ctype = "application/json"
        self.send_response(status)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(data)))
        for k, v in (headers or {}).items():
            if k != "Content-Type":
                self.send_header(k, v)
        self.end_headers()
        self.wfile.write(data)

    def route(self, method: str) -> None:
        fake: FakeServer = self.server.fake  # type: ignore[attr-defined]
        url = urlparse(self.path)
        path, query = url.path, parse_qs(url.query)
        length = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(length) if length else b""
        ctype = self.headers.get("Content-Type") or ""
        body = None
        if ctype.startswith("application/json") and raw:
            try:
                body = json.loads(raw)
            except ValueError:
                return self.send(*err(400, "bad_request", "malformed JSON"))
        with fake.lock:
            fake.requests.append(Seen(method, path, query, dict(self.headers.items()), body, len(raw)))
            scripted = fake.fail.get(path)
            failure = scripted.pop(0) if scripted else None
        if failure:
            status, fbody = failure
            return self.send(status, fbody, {"Retry-After": "0"} if status in (429, 503) else None)

        if path == "/v1/health":
            return self.send(200, {"ok": True, "ready": True, "busy": False, "device": "fake", "backend": "none", "version": "0.1.0"})
        if path.startswith("/v1/files/"):
            name = path[len("/v1/files/"):]
            sig = hmac.new(fake.secret.encode(), name.encode(), hashlib.sha256).hexdigest()[:16]
            if (query.get("sig") or [""])[0] != sig or name not in fake.files:
                return self.send(*err(404, "not_found", "no such file"))
            if self.headers.get("Authorization"):
                return self.send(*err(400, "bad_request", "signed URLs take no Authorization header"))
            return self.send(200, fake.files[name], {"Content-Type": "video/mp4"})

        if self.headers.get("Authorization") != f"Bearer {fake.key}":
            return self.send(*err(401, "unauthorized", "missing or unknown key"), {"WWW-Authenticate": "Bearer"})

        if method == "POST" and path == "/v1/uploads":
            return self.send(*fake.upload(raw, ctype, query))
        m = re.fullmatch(r"/v1/uploads/(asset_\w+)", path)
        if method == "GET" and m:
            a = fake.uploads.get(m.group(1))
            return self.send(200, fake.asset_json(a)) if a else self.send(*err(404, "not_found", "no such upload"))
        if method == "POST" and path == "/v1/render":
            status, out, headers = fake.render(body)
            return self.send(status, out, headers)
        if method == "POST" and path == "/v1/jobs":
            if isinstance(body, dict) and (body.get("output") or {}).get("inline"):
                return self.send(*err(400, "bad_request", "`output.inline` is not allowed for jobs"))
            invalid = fake.validate(body)
            if invalid:
                return self.send(*invalid)
            hook = body.get("webhook")
            if hook is not None and not str(hook).startswith(("https://", "http://")):
                return self.send(*err(400, "bad_request", "webhook must be an http(s) URL"))
            jid = "job_" + uuid.uuid4().hex[:20]
            data = {"id": jid, "status": "queued", "created_at": now_rfc3339(), "started_at": None, "finished_at": None,
                    "webhook": hook, "progress": None, "receipt": None, "error": None}
            with fake.lock:
                fake.jobs[jid] = FakeJob(data, body)
            threading.Thread(target=fake.run_job, args=(jid,), daemon=True).start()
            return self.send(202, dict(data))
        if method == "GET" and path == "/v1/jobs":
            limit = int((query.get("limit") or ["20"])[0])
            with fake.lock:
                jobs = [dict(j.data) for j in reversed(list(fake.jobs.values()))][:limit]
            return self.send(200, {"jobs": jobs})
        m = re.fullmatch(r"/v1/jobs/(job_\w+)", path)
        if m:
            with fake.lock:
                job = fake.jobs.get(m.group(1))
                if job is None:
                    return self.send(*err(404, "not_found", "no such job"))
                if method == "DELETE":
                    if job.data["status"] != "queued":
                        return self.send(*err(409, "job_started", "the job has started"))
                    job.data.update(status="canceled", finished_at=now_rfc3339())
                return self.send(200, dict(job.data))
        if method == "POST" and path in ("/v1/estimate", "/v1/frame", "/v1/sheet", "/v1/check", "/v1/review"):
            if path == "/v1/check" and isinstance(body, dict) and isinstance(body.get("document"), dict):
                doc = body["document"]
                issues = []
                if doc.get("output") not in (doc.get("nodes") or {}):
                    issues.append({"node": doc.get("output"), "severity": "error", "message": "output names a missing node"})
                for nid, n in (doc.get("nodes") or {}).items():
                    if n.get("op") == "text" and not (n.get("params") or {}).get("text"):
                        issues.append({"node": nid, "severity": "warning", "message": "empty text"})
                local = fake.local_paths(doc)
                if local:
                    return self.send(*err(403, "local_path_not_allowed", "file params must be references",
                                          node=local[0], param=local[1], value=local[2]))
                return self.send(200, {"ok": not any(i["severity"] == "error" for i in issues), "issues": issues})
            invalid = fake.validate(body)
            if invalid:
                return self.send(*invalid)
            doc = body["document"]
            fmt = doc["format"]
            if path == "/v1/estimate":
                rng = body.get("range") or [0, fmt["duration"]]
                seconds = float(rng[1]) - float(rng[0])
                short = min(fmt["width"], fmt["height"])
                factor = 0.5 if short <= 720 else (1 if short <= 1080 else 3)
                minutes = seconds / 60 * factor
                usd = None if fake.usd_per_minute is None else round(minutes * fake.usd_per_minute, 6)
                return self.send(200, {"minutes": round(minutes, 4), "usd": usd, "gpu_seconds": 1.0, "first_byte_s": 2.0,
                                       "width": fmt["width"], "height": fmt["height"], "fps": fmt["fps"], "seconds": seconds,
                                       "frames": int(seconds * fmt["fps"]), "device": "fake", "sampled_frames": 8})
            if path in ("/v1/frame", "/v1/sheet"):
                jpeg = body.get("format") == "jpeg"
                receipt = json.dumps({"timings": {"total_s": 0.01}, "request": {k: v for k, v in body.items() if k != "document"}})
                return self.send(200, JPEG_STUB if jpeg else PNG_1X1,
                                 {"Content-Type": "image/jpeg" if jpeg else "image/png", "X-Ultrafast-Receipt": receipt})
            issues = []
            for nid, n in doc["nodes"].items():
                p = n.get("params") or {}
                if n.get("op") == "text" and isinstance(p.get("size"), (int, float)) and p["size"] < 24:
                    issues.append({"severity": "warning", "kind": "tiny_text", "node": nid, "from": 0.0,
                                   "to": float(fmt["duration"]), "message": f"text is {p['size']} px; make it 24 px or more"})
            texts = sum(1 for n in doc["nodes"].values() if n.get("op") == "text")
            return self.send(200, {"issues": issues, "frames": int(fmt["duration"] * fmt["fps"]), "texts": texts,
                                   "seconds": float(fmt["duration"]), "safe": body.get("safe") or "margin"})
        if method == "GET" and path == "/v1/templates":
            return self.send(200, {"templates": ["vertical", "title"]})
        m = re.fullmatch(r"/v1/templates/(\w+)", path)
        if method == "GET" and m:
            if m.group(1) not in ("vertical", "title"):
                return self.send(*err(404, "not_found", "no such template"))
            return self.send(200, {"version": 1, "format": {"width": 1080, "height": 1920, "fps": 30, "duration": 8},
                                   "nodes": {"bg": {"op": "solid", "params": {"color": "#101418"}}}, "output": "bg"})
        if method == "GET" and path == "/v1/me":
            return self.send(200, {"caller": "test", "auth": "key", "webhook_secret": fake.secret})
        return self.send(*err(404, "not_found", f"no route {method} {path}"))
