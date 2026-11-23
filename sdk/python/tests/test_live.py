"""End-to-end checks against a running server; skipped unless ULTRAFAST_LIVE=1.

    ULTRAFAST_LIVE=1 ULTRAFAST_BASE_URL=http://127.0.0.1:8080 ULTRAFAST_API_KEY=... \\
        python3 -m unittest tests.test_live -v

Documents are tiny (640x360, 1 s) so they cost next to nothing. Against a
self-hosted `ultrafast serve`, what only ultrafast Cloud has (jobs and
webhooks, bucket output and inputs) must raise `CloudOnly`; against Cloud
it must work.
"""

import hashlib
import os
import struct
import tempfile
import threading
import time
import unittest
import wave
import zlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from unittest import mock

import ultrafastmov
from ultrafastmov import Ultrafast, webhooks

LIVE = os.environ.get("ULTRAFAST_LIVE") == "1"


def png(w, h, rgb):
    raw = b"".join(b"\x00" + bytes(rgb) * w for _ in range(h))

    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)

    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b"")


def tiny(extra_nodes=None, top="comp"):
    nodes = {
        "bg": {"op": "solid", "params": {"color": "#101418"}},
        "pic": {"op": "image", "params": {"src": "art/red.png", "fit": "contain"}},
        "title": {"op": "text", "params": {"text": "Tiny", "size": 14, "color": "#ffffff", "position": [20, 40]}},
        "layer": {"op": "over", "inputs": {"fg": "pic", "bg": "bg"}},
        "comp": {"op": "over", "inputs": {"fg": "title", "bg": "layer"}},
    }
    nodes.update(extra_nodes or {})
    return {"version": 1, "format": {"width": 640, "height": 360, "fps": 30, "duration": 1}, "nodes": nodes, "output": top}


@unittest.skipUnless(LIVE, "set ULTRAFAST_LIVE=1 (and ULTRAFAST_BASE_URL, ULTRAFAST_API_KEY) to run against a server")
class LiveTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        d = Path(cls.tmp.name)
        (d / "art").mkdir()
        (d / "art" / "red.png").write_bytes(png(64, 64, (230, 57, 70)))
        (d / "seq").mkdir()
        for i, rgb in enumerate([(230, 57, 70), (69, 123, 157)]):
            (d / "seq" / f"f_{i:03}.png").write_bytes(png(32, 32, rgb))
        with wave.open(str(d / "beat.wav"), "wb") as w:
            w.setnchannels(1)
            w.setsampwidth(2)
            w.setframerate(8000)
            w.writeframes(b"".join(struct.pack("<h", 8000 if (i // 400) % 2 else 0) for i in range(8000)))
        cls.dir = d
        cls.uf = Ultrafast()
        # Cloud gives each caller a webhook secret; `ultrafast serve` has no webhooks.
        cls.cloud = "webhook_secret" in cls.uf.me()
        ultrafastmov.Document(tiny(), base=d).save(d / "film.json")

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    def doc(self):
        return self.uf.load(self.dir / "film.json")

    def test_01_account_and_templates(self):
        self.assertTrue(self.uf.health()["ok"])
        me = self.uf.me()
        self.assertTrue(me["caller"])
        if self.cloud:
            self.assertTrue(me["webhook_secret"].startswith("whsec_"))
        names = self.uf.templates()
        self.assertIn("vertical", names)
        doc = self.uf.new("vertical")
        self.assertIsInstance(doc, ultrafastmov.Document)
        self.assertEqual(doc["version"], 1)

    def test_02_the_sites_sample(self):
        uf = self.uf
        doc = uf.load(str(self.dir / "film.json"))
        est = uf.estimate(doc)
        print("\n  estimate:", est)
        self.assertTrue(str(est).endswith("1 s of 640x360"), str(est))
        check = uf.check(doc)
        self.assertTrue(check.ok, list(check))
        for issue in uf.review(doc):
            print("  review:", issue.kind, issue.node, issue.message)
        r = uf.render(doc)
        out = r.save(self.dir / "film.mp4")
        data = out.read_bytes()
        self.assertEqual(hashlib.sha256(data).hexdigest(), r.sha256)
        self.assertEqual(len(data), r.bytes)
        self.assertEqual(data[4:8], b"ftyp")
        self.assertEqual(r.frames, 30)
        self.assertEqual(doc["nodes"]["pic"]["params"]["src"], "art/red.png")

    def test_03_frames_sheets_inline_and_cache(self):
        doc = self.doc()
        img = self.uf.frame(doc, at=0.5)
        self.assertEqual(bytes(img[:8]), b"\x89PNG\r\n\x1a\n")
        jpg = self.uf.frame(doc, at=0.5, format="jpeg")
        self.assertEqual(jpg.format, "jpeg")
        sheet = self.uf.sheet(doc, count=4, columns=2)
        self.assertEqual(sheet.format, "png")
        sheet.save(self.dir / "sheet.png")
        assets = len(self.uf._assets)
        r = self.uf.render(doc, inline=True, range=[0, 0.5], audio=False)
        self.assertEqual(r.content[4:8], b"ftyp")
        self.assertEqual(hashlib.sha256(r.content).hexdigest(), r.sha256)
        self.assertEqual(len(self.uf._assets), assets, "local files are uploaded once per client")

    def test_04_job_and_webhook(self):
        if not self.cloud:
            for call, feature in [
                (lambda: self.uf.render(self.doc(), webhook="https://example.com/hook"), "jobs"),
                (lambda: self.uf.job("job_01NOPE"), "jobs"),
                (lambda: self.uf.render(self.doc(), uri="s3://my-bucket/film.mp4"), "bucket_output"),
                (lambda: self.uf.frame(tiny({"pic": {"op": "image", "params": {"src": "gs://my-bucket/red.png#1"}}})),
                 "bucket_inputs"),
            ]:
                with self.assertRaises(ultrafastmov.CloudOnly) as cm:
                    call()
                self.assertEqual((cm.exception.status, cm.exception.feature), (501, feature))
            print("\n  cloud only:", cm.exception)
            return
        got = []

        class H(BaseHTTPRequestHandler):
            def log_message(self, *a):
                pass

            def do_POST(self):
                got.append((self.rfile.read(int(self.headers["Content-Length"])), dict(self.headers.items())))
                self.send_response(204)
                self.end_headers()

        httpd = ThreadingHTTPServer(("127.0.0.1", 0), H)
        threading.Thread(target=httpd.serve_forever, daemon=True).start()
        try:
            job = self.uf.render(self.doc(), webhook=f"http://127.0.0.1:{httpd.server_address[1]}/hook")
            self.assertIn(job.status, ("queued", "rendering", "rendered"))
            r = job.wait(poll=0.5, timeout=120)
            path = job.save(self.dir / "job.mp4")
            self.assertEqual(hashlib.sha256(path.read_bytes()).hexdigest(), r.sha256)
            for _ in range(100):
                if got:
                    break
                time.sleep(0.1)
            self.assertTrue(got, "no webhook delivery")
            raw, headers = got[0]
            headers = {k.lower(): v for k, v in headers.items()}  # header names are case-insensitive
            print("\n  webhook headers:", sorted(k for k in headers if k.startswith("ultrafast")))
            event = webhooks.verify(raw, headers.get("ultrafast-signature"), self.uf.me()["webhook_secret"])
            self.assertIn(headers.get("ultrafast-event"), ("job.rendered",))
            self.assertEqual(event["id"], job.id)
            self.assertEqual(event["status"], "rendered")
            self.assertEqual(self.uf.job(job.id).status, "rendered")
            self.assertIn(job.id, [j.id for j in self.uf.jobs.list(limit=50)])
            with self.assertRaises(ultrafastmov.Conflict):
                job.cancel()
        finally:
            httpd.shutdown()
            httpd.server_close()

    def test_05_failed_job_raises_its_error(self):
        if not self.cloud:
            self.skipTest("jobs are ultrafast Cloud's")
        bad = tiny()
        bad["nodes"]["comp"]["inputs"]["fg"] = "missing"
        job = self.uf.submit(ultrafastmov.Document(bad, base=self.dir))
        with self.assertRaises(ultrafastmov.UltrafastError) as cm:
            job.wait(poll=0.5, timeout=60)
        print("\n  failed job:", type(cm.exception).__name__, cm.exception)

    def test_06_errors(self):
        with self.assertRaises(ultrafastmov.Unauthorized):
            Ultrafast(api_key="example-wrong-key").me()
        bad = tiny()
        bad["nodes"]["comp"]["inputs"]["fg"] = "missing"
        with self.assertRaises(ultrafastmov.InvalidDocument) as cm:
            self.uf.render(ultrafastmov.Document(bad, base=self.dir))
        self.assertTrue(cm.exception.issues)
        self.assertFalse(self.uf.check(ultrafastmov.Document(bad, base=self.dir)).ok)
        with self.assertRaises(ultrafastmov.NotFound if self.cloud else ultrafastmov.CloudOnly):
            self.uf.job("job_01NOPE")
        with self.assertRaises(ultrafastmov.NotFound):
            self.uf.new("no_such_template")
        with mock.patch.object(Ultrafast, "prepare", lambda self, d: dict(d)):
            with self.assertRaises(ultrafastmov.Forbidden) as cm:
                self.uf.frame(self.doc())
        self.assertEqual(cm.exception.code, "local_path_not_allowed")

    def test_07_attempt_and_upload(self):
        r = self.uf.render(self.doc(), attempt=True, range=[0, 0.5])
        self.assertEqual(r.status, "rendered")
        a = self.uf.upload(self.dir / "art" / "red.png")
        self.assertEqual(a.sha256, hashlib.sha256((self.dir / "art" / "red.png").read_bytes()).hexdigest())
        self.assertEqual(self.uf.asset(a.id).uri, a.uri)

    def test_08_image_sequence(self):
        doc = ultrafastmov.Document(tiny({"pic": {"op": "image", "params": {"src": "seq/f_{frame:03}.png", "fps": 2, "fit": "contain"}}}), base=self.dir)
        sent = self.uf.prepare(doc)
        self.assertRegex(sent["nodes"]["pic"]["params"]["src"], r"^asset://\w+!/f_\{frame:03\}\.png$")
        self.assertEqual(bytes(self.uf.frame(doc, at=0.75)[:4]), b"\x89PNG")

    def test_09_audio_driver(self):
        doc = ultrafastmov.Document(tiny({"title": {"op": "text", "params": {
            "text": "Beat", "size": 40, "color": "#ffffff", "position": [20, 60],
            "opacity": {"audio": {"src": "beat.wav", "band": "all"}}}}}), base=self.dir)
        sent = self.uf.prepare(doc)
        self.assertTrue(sent["nodes"]["title"]["params"]["opacity"]["audio"]["src"].startswith("asset://"))
        self.assertEqual(bytes(self.uf.frame(doc, at=0.5)[:4]), b"\x89PNG")


if __name__ == "__main__":
    unittest.main()
