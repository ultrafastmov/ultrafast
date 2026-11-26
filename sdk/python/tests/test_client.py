import hashlib
import io
import json
import os
import tempfile
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from unittest import mock

import ultrafastmov
from ultrafastmov import (
    APIConnectionError,
    Busy,
    Conflict,
    Forbidden,
    InvalidDocument,
    JobCanceled,
    JobTimeout,
    NotFound,
    Render,
    ServerError,
    Ultrafast,
    Unauthorized,
    Unavailable,
    webhooks,
)

from .fake_server import FakeServer, PNG_1X1

FILM = {
    "version": 1,
    "format": {"width": 1920, "height": 1080, "fps": 30, "duration": 8},
    "nodes": {
        "bg": {"op": "video", "params": {"src": "footage/a.mp4", "fit": "cover"}},
        "title": {"op": "text", "params": {"text": "Launch", "size": 18, "font": "fonts/Brand.otf"}},
        "comp": {"op": "over", "inputs": {"fg": "title", "bg": "bg"}},
    },
    "output": "comp",
}


class Receiver:
    """Collects webhook deliveries."""

    def __init__(self):
        self.got = []
        outer = self

        class H(BaseHTTPRequestHandler):
            def log_message(self, *a):
                pass

            def do_POST(self):
                raw = self.rfile.read(int(self.headers["Content-Length"]))
                outer.got.append((raw, dict(self.headers.items())))
                self.send_response(204)
                self.end_headers()

        self.httpd = ThreadingHTTPServer(("127.0.0.1", 0), H)
        threading.Thread(target=self.httpd.serve_forever, kwargs={"poll_interval": 0.02}, daemon=True).start()
        self.url = f"http://127.0.0.1:{self.httpd.server_address[1]}/hook"

    def close(self):
        self.httpd.shutdown()
        self.httpd.server_close()


class ClientTest(unittest.TestCase):
    def setUp(self):
        self.server = FakeServer()
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)
        (self.dir / "footage").mkdir()
        (self.dir / "fonts").mkdir()
        self.video_bytes = os.urandom(3 << 20)  # bigger than one upload chunk
        (self.dir / "footage" / "a.mp4").write_bytes(self.video_bytes)
        (self.dir / "fonts" / "Brand.otf").write_bytes(b"OTTO" + b"\0" * 100)
        (self.dir / "film.json").write_text(json.dumps(FILM))
        env = {"ULTRAFAST_API_KEY": self.server.key, "ULTRAFAST_BASE_URL": self.server.url}
        self.env = mock.patch.dict(os.environ, env)
        self.env.start()
        self.uf = Ultrafast()
        self.uf._transport.sleep = lambda s: None

    def tearDown(self):
        self.env.stop()
        self.server.close()
        self.tmp.cleanup()

    def doc(self):
        return self.uf.load(self.dir / "film.json")

    def test_the_sites_sample(self):
        uf = Ultrafast()  # ULTRAFAST_API_KEY
        uf._transport.sleep = lambda s: None
        doc = uf.load(str(self.dir / "film.json"))
        self.assertEqual(str(uf.estimate(doc)), "$0.003, 8 s of 1080p")
        issues = list(uf.review(doc))
        self.assertEqual([(i.kind, i.node) for i in issues], [("tiny_text", "title")])
        self.assertEqual(issues[0].from_, 0.0)
        self.assertEqual(issues[0].to, 8.0)
        self.assertIn("24 px", issues[0].message)
        out = uf.render(doc).save(self.dir / "film.mp4")
        data = out.read_bytes()
        self.assertTrue(data.startswith(b"\x00\x00\x00\x18ftyp"))
        self.assertEqual(len(data), self.server.video_size)
        # the document as loaded is unchanged
        self.assertEqual(doc["nodes"]["bg"]["params"]["src"], "footage/a.mp4")

    def test_headers_and_user_agent(self):
        self.uf.me()
        seen = self.server.seen("/v1/me")[0]
        self.assertEqual(seen.headers["Authorization"], f"Bearer {self.server.key}")
        self.assertEqual(seen.headers["User-Agent"], f"ultrafastmov-python/{ultrafastmov.__version__}")

    def test_uploads_rewrite_and_cache(self):
        doc = self.doc()
        r1 = self.uf.render(doc)
        sent = self.server.seen("/v1/render")[0].json["document"]
        src = sent["nodes"]["bg"]["params"]["src"]
        font = sent["nodes"]["title"]["params"]["font"]
        self.assertTrue(src.startswith("asset://") and font.startswith("asset://"))
        self.assertEqual(len(self.server.uploads), 2)
        asset = self.server.uploads["asset_" + src[len("asset://"):]]
        self.assertEqual(asset.data, self.video_bytes)
        self.assertEqual(asset.filename, "a.mp4")
        self.assertEqual(asset.content_type, "video/mp4")
        # frame, sheet, check, review and a second render reuse the uploads
        self.uf.frame(doc, at=1)
        self.uf.sheet(doc)
        self.uf.check(doc)
        self.uf.render(doc)
        self.assertEqual(len(self.server.seen("/v1/uploads")), 2)
        self.assertEqual(r1.status, "rendered")
        # a copy of the file elsewhere has the same sha256 and size: no new upload
        other = self.dir / "copy.mp4"
        other.write_bytes(self.video_bytes)
        self.uf.frame({**FILM, "nodes": {"bg": {"op": "video", "params": {"src": str(other)}}}, "output": "bg"})
        self.assertEqual(len(self.server.seen("/v1/uploads")), 2)
        # a new client uploads again
        Ultrafast().frame(doc)
        self.assertEqual(len(self.server.seen("/v1/uploads")), 4)

    def test_upload_kinds(self):
        a = self.uf.upload(self.dir / "fonts" / "Brand.otf")
        self.assertTrue(a.uri.startswith("asset://"))
        self.assertEqual(a.bytes, 104)
        self.assertEqual(a.sha256, hashlib.sha256(b"OTTO" + b"\0" * 100).hexdigest())
        b = self.uf.upload(b"hello", filename='we"ird.txt')
        self.assertEqual(self.server.uploads[b.id].data, b"hello")
        self.assertEqual(self.server.uploads[b.id].filename, 'we"ird.txt')

        class Pipe(io.RawIOBase):  # not seekable
            def __init__(self, data):
                self.buf = io.BytesIO(data)

            def readable(self):
                return True

            def readinto(self, b):
                return self.buf.readinto(b)

        c = self.uf.upload(io.BufferedReader(Pipe(b"x" * 5000)), filename="pipe.bin")
        self.assertEqual(self.server.uploads[c.id].data, b"x" * 5000)
        f = io.BytesIO(b"skipTHIS")
        f.seek(4)
        d = self.uf.upload(f, filename="tail.bin")
        self.assertEqual(self.server.uploads[d.id].data, b"THIS")
        self.assertEqual(self.uf.asset(d.id).sha256, hashlib.sha256(b"THIS").hexdigest())

    def test_missing_local_file_fails_before_any_request(self):
        doc = self.doc()
        doc["nodes"]["bg"]["params"]["src"] = "footage/nope.mp4"
        with self.assertRaises(FileNotFoundError):
            self.uf.render(doc)
        self.assertEqual(self.server.seen("/v1/render"), [])

    def test_server_refuses_what_the_client_did_not_upload(self):
        # bypass the client's uploads to see the server's answer mapped
        with mock.patch.object(Ultrafast, "prepare", lambda self, d: dict(d)):
            with self.assertRaises(Forbidden) as cm:
                self.uf.render(FILM)
        e = cm.exception
        self.assertEqual((e.status, e.code, e.node, e.param, e.value), (403, "local_path_not_allowed", "bg", "src", "footage/a.mp4"))

    def test_inline_render(self):
        r = self.uf.render(self.doc(), inline=True, codec="hevc", range=(0, 2), audio=False, name="x.mp4")
        self.assertEqual(self.server.seen("/v1/render")[0].json["output"],
                         {"inline": True, "name": "x.mp4", "codec": "hevc", "audio": False, "range": [0, 2]})
        self.assertIsNotNone(r.content)
        self.assertIsNone(r.url)
        self.assertEqual(r.sha256, hashlib.sha256(r.content).hexdigest())
        self.assertEqual(r.bytes, len(r.content))
        p = r.save(self.dir)  # a directory: film.mp4 inside it
        self.assertEqual(p.name, "film.mp4")
        self.assertEqual(p.read_bytes(), r.content)

    def test_export_settings_reach_the_output(self):
        r = self.uf.render(self.doc(), inline=True, codec="prores", alpha=True, size="4k", fps=60,
                           quality="master", prores_profile="4444", audio_codec="pcm-24")
        self.assertEqual(self.server.seen("/v1/render")[0].json["output"],
                         {"inline": True, "codec": "prores", "alpha": True, "size": "4k", "fps": 60,
                          "quality": "master", "prores_profile": "4444", "audio_codec": "pcm-24"})
        # a size as a pair, a bitrate as text
        self.uf.render(self.doc(), size=(1280, 720), bitrate="8M", format="webm")
        self.assertEqual(self.server.seen("/v1/render")[1].json["output"],
                         {"size": [1280, 720], "bitrate": "8M", "format": "webm"})
        # saved under the format's name
        r = Render({"receipt_version": 1, "video": {"format": "webm"}}, content=b"x")
        self.assertEqual(r.filename, "film.webm")
        self.assertEqual(Render({"video": {"format": "png"}}, content=b"x").filename, "frames.zip")
        self.assertEqual(Render({"url": "https://h/v1/files/renders/r/overlay.mov?sig=1"}).filename, "overlay.mov")
        with self.assertRaises(TypeError):
            self.uf.estimate(self.doc(), qualty="high")

    def test_stored_render_streams_and_verifies(self):
        self.server.video_size = 5 << 20
        r = self.uf.render(self.doc())
        self.assertTrue(r.url.startswith(self.server.url + "/v1/files/"))
        self.assertEqual(r.bytes, 5 << 20)
        self.assertEqual(r.duration, 8.0)
        self.assertEqual(r.frames, 240)
        out = r.save(self.dir / "out.mp4")
        self.assertEqual(hashlib.sha256(out.read_bytes()).hexdigest(), r.sha256)
        # a tampered receipt fails the check and leaves nothing behind
        bad = Render({**r.receipt, "sha256": "0" * 64}, client=self.uf)
        with self.assertRaises(ultrafastmov.IntegrityError):
            bad.save(self.dir / "bad.mp4")
        self.assertFalse((self.dir / "bad.mp4").exists())
        self.assertEqual([p.name for p in self.dir.iterdir() if p.name.endswith(".part")], [])

    def test_bucket_output_and_replay(self):
        doc = self.doc()
        r = self.uf.render(doc, uri="s3://bucket/out.mp4", attempt={"render_id": "r1", "attempt_id": "a1"})
        self.assertEqual(r.status, "rendered")
        self.assertIsNone(r.url)
        with self.assertRaises(ultrafastmov.UltrafastError):
            r.save(self.dir / "x.mp4")
        again = self.uf.render(doc, uri="s3://bucket/out.mp4", attempt={"render_id": "r1", "attempt_id": "a1"})
        self.assertEqual(again.status, "already_rendered")
        self.assertEqual(again.sha256, r.sha256)
        with self.assertRaises(Conflict) as cm:
            self.uf.render(doc, uri="s3://bucket/out.mp4", attempt="r2")
        self.assertEqual(cm.exception.code, "output_conflict")

    def test_retries(self):
        doc = self.doc()
        busy = (503, {"error": {"status": 503, "code": "unavailable", "message": "GPU not initialized", "retry": True}})
        self.server.fail["/v1/frame"] = [busy, busy]
        img = self.uf.frame(doc, at=2.5, format="png")
        self.assertEqual(bytes(img), PNG_1X1)
        self.assertEqual(len(self.server.seen("/v1/frame")), 3)
        # give up after max_retries
        self.server.fail["/v1/sheet"] = [busy, busy, busy]
        with self.assertRaises(Unavailable):
            self.uf.sheet(doc)
        # not retried when the server says retry: false
        self.server.fail["/v1/estimate"] = [(429, {"error": {"status": 429, "code": "busy", "message": "x", "retry": False}})]
        with self.assertRaises(Busy):
            self.uf.estimate(doc)
        self.assertEqual(len(self.server.seen("/v1/estimate")), 1)
        # renders are not retried without an attempt...
        self.server.fail["/v1/render"] = [busy]
        with self.assertRaises(Unavailable):
            self.uf.render(doc)
        # ...and are with one, sending the same attempt ids each time
        self.server.fail["/v1/render"] = [busy]
        r = self.uf.render(doc, attempt=True)
        sent = [s.json["attempt"] for s in self.server.seen("/v1/render")[-2:]]
        self.assertEqual(sent[0], sent[1])
        self.assertTrue(sent[0]["render_id"].startswith("render_"))
        self.assertEqual(r.receipt["attempt"], sent[0])
        # a 500 render_failed is not retried
        self.server.fail["/v1/check"] = [(500, {"error": {"status": 500, "code": "render_failed", "message": "GPU lost", "retry": True}})]
        with self.assertRaises(ServerError):
            self.uf.check(doc)

    def test_connection_errors(self):
        uf = Ultrafast(base_url="http://127.0.0.1:9", api_key="k", timeout=2)
        uf._transport.sleep = lambda s: None
        with self.assertRaises(APIConnectionError):
            uf.me()

    def test_errors_map_to_classes(self):
        with self.assertRaises(Unauthorized) as cm:
            Ultrafast(api_key="wrong").me()
        self.assertEqual(cm.exception.status, 401)
        with mock.patch.dict(os.environ, {"ULTRAFAST_API_KEY": ""}):
            with self.assertRaises(Unauthorized) as cm:
                Ultrafast().me()
            self.assertIn("ULTRAFAST_API_KEY", str(cm.exception))
        with self.assertRaises(InvalidDocument) as cm:
            self.uf.render({"version": 1, "nodes": {}, "output": "x"})
        self.assertEqual(cm.exception.code, "invalid_document")
        self.assertEqual({i.message for i in cm.exception.issues}, {"`format` is missing", "output names a missing node"})
        with self.assertRaises(NotFound):
            self.uf.job("job_nope")
        with self.assertRaises(NotFound):
            self.uf.new("nope")
        from ultrafastmov.errors import CloudOnly, error_from_body

        e = error_from_body({"error": {"status": 501, "code": "cloud_only", "feature": "jobs",
                                       "message": "jobs and their webhooks are an ultrafast Cloud feature"}})
        self.assertIsInstance(e, CloudOnly)
        self.assertEqual((e.status, e.feature, e.retry), (501, "jobs", False))
        self.assertIn("ultrafast Cloud", str(e))

    def test_check(self):
        good = self.uf.check(self.doc())
        self.assertTrue(good.ok)
        doc = self.doc()
        doc["nodes"]["title"]["params"]["text"] = ""
        doc["output"] = "missing"
        bad = self.uf.check(doc)
        self.assertFalse(bad.ok)
        self.assertEqual([(i.severity, i.node) for i in bad], [("error", "missing"), ("warning", "title")])
        self.assertEqual(len(bad.errors), 1)

    def test_frame_sheet_jpeg(self):
        img = self.uf.sheet(self.doc(), count=4, columns=2, format="jpeg")
        body = self.server.seen("/v1/sheet")[0].json
        self.assertEqual({k: v for k, v in body.items() if k != "document"}, {"count": 4, "columns": 2, "format": "jpeg"})
        self.assertEqual(img.format, "jpeg")
        self.assertEqual(img.receipt["request"]["count"], 4)
        p = img.save(self.dir / "sheet.jpg")
        self.assertEqual(p.read_bytes(), bytes(img))
        self.uf.sheet(self.doc(), at=[0, 1.5])
        self.assertEqual(self.server.seen("/v1/sheet")[1].json["at"], [0, 1.5])

    def test_frame_alpha_is_sent_only_when_asked_for(self):
        self.uf.frame(self.doc(), at=0.5)
        self.uf.frame(self.doc(), at=0.5, alpha=True)
        plain, alpha = (r.json for r in self.server.seen("/v1/frame"))
        self.assertNotIn("alpha", plain)
        self.assertIs(alpha["alpha"], True)

    def test_estimate_without_price(self):
        self.server.usd_per_minute = None
        doc = self.doc()
        doc["format"].update(width=1080, height=1920)
        self.assertEqual(str(self.uf.estimate(doc, range=[0, 2.5])), "2.5 s of 1080p, 1 GPU second")
        self.assertEqual(self.server.seen("/v1/estimate")[0].json["range"], [0, 2.5])
        self.uf.estimate(doc, size="4k", quality="high")
        self.assertEqual(self.server.seen("/v1/estimate")[1].json["output"], {"size": "4k", "quality": "high"})

    def test_new_and_templates(self):
        self.assertEqual(self.uf.templates(), ["vertical", "title"])
        doc = self.uf.new("vertical")
        self.assertIsInstance(doc, ultrafastmov.Document)
        self.assertEqual(doc["format"]["height"], 1920)
        self.assertEqual(str(self.uf.estimate(doc)), "$0.003, 8 s of 1080p")

    def test_job_with_webhook(self):
        recv = Receiver()
        try:
            secret = self.uf.me()["webhook_secret"]
            job = self.uf.render(self.doc(), webhook=recv.url)
            self.assertIsInstance(job, ultrafastmov.Job)
            self.assertEqual(job.status, "queued")
            self.assertTrue(job.id.startswith("job_"))
            r = job.wait(poll=0.02, timeout=10)
            self.assertEqual(job.status, "rendered")
            self.assertTrue(r.url)
            path = job.save(self.dir / "job.mp4")
            self.assertEqual(hashlib.sha256(path.read_bytes()).hexdigest(), r.sha256)
            for _ in range(200):
                if recv.got:
                    break
                time.sleep(0.01)
            raw, headers = recv.got[0]
            event = webhooks.verify(raw, headers["Ultrafast-Signature"], secret)
            self.assertEqual(event["id"], job.id)
            self.assertEqual(event["event"], "job.rendered")
            self.assertEqual(headers["Ultrafast-Event"], "job.rendered")
            self.assertEqual(event["receipt"]["sha256"], r.sha256)
            with self.assertRaises(ultrafastmov.WebhookVerificationError):
                webhooks.verify(raw + b" ", headers["Ultrafast-Signature"], secret)
            with self.assertRaises(ultrafastmov.WebhookVerificationError):
                webhooks.verify(raw, headers["Ultrafast-Signature"], "whsec_other")
            # listed and fetched by id
            self.assertEqual(self.uf.jobs.list(limit=5)[0].id, job.id)
            self.assertEqual(self.uf.jobs.get(job.id).status, "rendered")
        finally:
            recv.close()

    def test_submit_cancel_fail_and_timeout(self):
        self.server.job_start_delay = 5
        job = self.uf.submit(self.doc())
        self.assertNotIn("webhook", self.server.seen("/v1/jobs")[0].json)
        with self.assertRaises(JobTimeout):
            job.wait(poll=0.01, timeout=0.05)
        job.cancel()
        self.assertEqual(job.status, "canceled")
        with self.assertRaises(JobCanceled):
            job.wait()
        # a started job cannot be canceled
        self.server.job_start_delay = 0
        self.server.job_render_time = 1
        started = self.uf.submit(self.doc())
        for _ in range(100):
            if started.refresh().status == "rendering":
                break
            time.sleep(0.01)
        with self.assertRaises(Conflict) as cm:
            self.uf.jobs.cancel(started.id)
        self.assertEqual(cm.exception.code, "job_started")
        # a failed job raises its error
        self.server.job_render_time = 0
        self.server.job_error = {"status": 422, "code": "input_unavailable", "message": "gone", "retry": False, "reason": "not_found"}
        failed = self.uf.submit(self.doc())
        with self.assertRaises(ultrafastmov.InputUnavailable) as cm:
            failed.wait(poll=0.01, timeout=5)
        self.assertEqual(cm.exception.reason, "not_found")
        self.assertIs(cm.exception.job, failed)
        with self.assertRaises(ValueError):
            self.uf.submit(self.doc(), inline=True)

    def test_attempt_ids_are_validated(self):
        with self.assertRaises(ValueError):
            self.uf.render(self.doc(), attempt="has spaces")

    def test_base_url_forms(self):
        self.assertEqual(Ultrafast(base_url="http://localhost:8080/v1/").base_url, "http://localhost:8080")
        with mock.patch.dict(os.environ, {"ULTRAFAST_BASE_URL": ""}):
            self.assertEqual(Ultrafast().base_url, "http://127.0.0.1:8080")
        with self.assertRaises(ValueError):
            Ultrafast(base_url="localhost:8080")

    def test_health_needs_no_key(self):
        self.assertTrue(Ultrafast(api_key="").health()["ok"])
        self.assertNotIn("Authorization", self.server.seen("/v1/health")[0].headers)


if __name__ == "__main__":
    unittest.main()
