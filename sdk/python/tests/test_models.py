import copy
import json
import os
import tempfile
import unittest
from pathlib import Path

import ultrafastmov
from ultrafastmov import Document, Estimate, Issue, Render, errors
from ultrafastmov.models import resolution_label

FIXTURES = Path(__file__).resolve().parents[3] / "docs" / "service" / "fixtures"


class EstimateTest(unittest.TestCase):
    def test_format(self):
        cases = [
            (dict(usd=0.004, seconds=8, width=1920, height=1080), "$0.004, 8 s of 1080p"),
            (dict(usd=0.006, seconds=11.6, width=1080, height=1920), "$0.006, 11.6 s of 1080p"),
            (dict(usd=None, seconds=8, width=1920, height=1080), "8 s of 1080p"),
            (dict(usd=0.25, seconds=60, width=1280, height=720), "$0.25, 60 s of 720p"),
            (dict(usd=1.5, seconds=90.04, width=2560, height=1440), "$1.5, 90 s of 1440p"),
            (dict(usd=12.3456, seconds=600, width=3840, height=2160), "$12.346, 600 s of 4K"),
            (dict(usd=0.0004, seconds=1, width=2160, height=3840), "$0, 1 s of 4K".replace("$0", "<$0.001")),
            (dict(usd=0.0, seconds=2, width=640, height=360), "$0, 2 s of 640x360"),
            (dict(usd=0.0006, seconds=2, width=1080, height=1080), "$0.001, 2 s of 1080p"),
            (dict(usd=None, seconds=3), "3 s"),
        ]
        for fields, text in cases:
            self.assertEqual(str(Estimate(**fields)), text, fields)

    def test_from_api(self):
        body = {"minutes": 0.2, "usd": 0.006, "gpu_seconds": 2.1, "first_byte_s": 3.0, "width": 1080, "height": 1920,
                "fps": 30, "seconds": 11.6, "frames": 348, "device": "NVIDIA L4", "sampled_frames": 24, "new_field": 1}
        e = Estimate.from_dict(body)
        self.assertEqual(str(e), "$0.006, 11.6 s of 1080p")
        self.assertEqual(e.device, "NVIDIA L4")
        self.assertEqual(e.raw["new_field"], 1)

    def test_labels(self):
        self.assertEqual(resolution_label(1920, 1080), "1080p")
        self.assertEqual(resolution_label(1080, 1920), "1080p")
        self.assertEqual(resolution_label(1080, 1350), "1080p")
        self.assertEqual(resolution_label(720, 1280), "720p")
        self.assertEqual(resolution_label(1000, 500), "1000x500")
        self.assertIsNone(resolution_label(None, 1080))


class IssueTest(unittest.TestCase):
    def test_review_issue(self):
        i = Issue.from_dict({"severity": "warning", "kind": "brief", "node": "title", "from": 1.2, "to": 1.6, "message": "on screen too briefly"})
        self.assertEqual((i.kind, i.node, i.from_, i.start, i.to, i.end), ("brief", "title", 1.2, 1.2, 1.6, 1.6))
        self.assertEqual(str(i), "warning brief title 1.2-1.6 s: on screen too briefly")
        self.assertEqual(hash(i), hash(Issue.from_dict(i.raw)))

    def test_check_issue(self):
        i = Issue.from_dict({"node": "stack", "severity": "error", "message": "input `fg` references missing node `subjct`"})
        self.assertIsNone(i.kind)
        self.assertEqual(str(i), "error stack: input `fg` references missing node `subjct`")


@unittest.skipUnless(FIXTURES.is_dir(), "API fixtures are in the ultrafast repository")
class FixtureTest(unittest.TestCase):
    def test_error_fixtures_map_to_classes(self):
        want = {
            "400-bad-request": errors.BadRequest,
            "400-invalid-document": errors.InvalidDocument,
            "403-input-not-allowed": errors.Forbidden,
            "403-local-path-not-allowed": errors.Forbidden,
            "403-output-not-allowed": errors.Forbidden,
            "409-output-conflict": errors.Conflict,
            "413-over-limit": errors.TooLarge,
            "422-input-unavailable": errors.InputUnavailable,
            "429-busy": errors.Busy,
            "500-render-failed": errors.ServerError,
            "503-unavailable": errors.Unavailable,
            "504-deadline-exceeded": errors.DeadlineExceeded,
        }
        found = sorted(p.stem for p in (FIXTURES / "errors").glob("*.json"))
        self.assertTrue(set(want) <= set(found), found)
        for stem in found:
            raw = (FIXTURES / "errors" / f"{stem}.json").read_bytes()
            status = int(stem.split("-")[0])
            e = errors.error_from_response(status, raw)
            if stem in want:
                self.assertIs(type(e), want[stem], stem)
            self.assertIsInstance(e, ultrafastmov.UltrafastError)
            body = json.loads(raw)["error"]
            self.assertEqual((e.status, e.code, e.message, e.retry), (body["status"], body["code"], body["message"], body["retry"]))
        e = errors.error_from_response(400, (FIXTURES / "errors" / "400-invalid-document.json").read_bytes())
        self.assertEqual([i.node for i in e.issues], ["title", "stack"])
        self.assertIn("references missing node", str(e))
        e = errors.error_from_response(413, (FIXTURES / "errors" / "413-over-limit.json").read_bytes())
        self.assertEqual((e.limit, e.value, e.max), ("duration_s", 240, 180))

    def test_receipt_fixture(self):
        receipt = json.loads((FIXTURES / "receipt.json").read_text())
        r = Render(receipt)
        self.assertEqual(r.status, "rendered")
        self.assertEqual(r.sha256, receipt["object"]["sha256"])
        self.assertEqual(r.bytes, receipt["object"]["size"])
        self.assertEqual(r.duration, receipt["video"]["duration"])
        self.assertEqual(r.frames, receipt["video"]["frames"])

    def test_documents_from_fixtures_are_left_alone(self):
        # Every file param in the fixtures is already a reference.
        from ultrafastmov import _files

        for name in ("render-request.json", "frame-request.json"):
            doc = json.loads((FIXTURES / name).read_text())["document"]
            out = _files.localize(doc, Path("/nonexistent"), lambda p: self.fail(p), lambda *a: self.fail(a))
            self.assertEqual(out, doc)


class ErrorTest(unittest.TestCase):
    def test_non_json_and_proxy_errors(self):
        e = errors.error_from_response(502, b"<html>Bad Gateway</html>")
        self.assertIsInstance(e, errors.ServerError)
        self.assertTrue(e.retry)
        self.assertIn("Bad Gateway", e.message)
        e = errors.error_from_response(418, b"")
        self.assertIs(type(e), errors.UltrafastError)
        self.assertEqual(e.message, "HTTP 418")

    def test_unknown_attribute(self):
        e = errors.error_from_response(400, b'{"error": {"status": 400, "code": "bad_request", "message": "m", "retry": false}}')
        with self.assertRaises(AttributeError):
            e.nothing
        import pickle

        again = pickle.loads(pickle.dumps(e))
        self.assertEqual((str(again), again.code, again.status), ("400 bad_request: m", "bad_request", 400))


class DocumentTest(unittest.TestCase):
    def test_load_paths_files_and_dicts(self):
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / "sub" / "film.json"
            p.parent.mkdir()
            p.write_text('{"version": 1, "format": {"width": 1, "height": 2}, "nodes": {}, "output": "x"}')
            doc = ultrafastmov.load(str(p))
            self.assertEqual(doc.base, p.parent.resolve())
            self.assertEqual(doc.path, p.resolve())
            with open(p) as f:
                self.assertEqual(Document.load(f).base, p.parent)
            c = doc.copy()
            c["nodes"]["n"] = {}
            self.assertEqual(doc["nodes"], {})
            self.assertEqual(c.base, doc.base)
            self.assertEqual(copy.deepcopy(doc).base, doc.base)
            out = doc.save(Path(d) / "out.json")
            self.assertEqual(json.loads(out.read_text()), doc)
            self.assertIn("1x2 0 nodes", repr(doc))
        self.assertEqual(Document.load({"a": 1}).base, Path.cwd())
        with self.assertRaises(TypeError):
            Document.load(42)


class TypesTest(unittest.TestCase):
    def test_generated_types(self):
        from ultrafastmov import types

        self.assertIn("text", types.OPS)
        self.assertIs(types.PARAMS["text"], types.TextParams)
        self.assertIn("font", types.TextParams.__annotations__)
        self.assertEqual(set(types.INPUTS["over"]), {"fg", "bg"})
        import typing

        for name in types.__all__:
            obj = getattr(types, name)
            if isinstance(obj, type) and issubclass(obj, dict):
                typing.get_type_hints(obj)

    def test_generator_is_current(self):
        repo = Path(__file__).resolve().parents[3]
        schema = os.environ.get("ULTRAFAST_SCHEMA") or str(repo / "docs" / "schema" / "v1.json")
        gen = Path(__file__).resolve().parents[1] / "tools" / "gen_types.py"
        if not Path(schema).is_file() or not gen.is_file():
            self.skipTest("no local schema")
        import importlib.util

        spec = importlib.util.spec_from_file_location("gen_types", gen)
        mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(mod)
        text = mod.generate(mod.load(schema), mod.DEFAULT_SCHEMA)
        current = (Path(ultrafastmov.__file__).parent / "types.py").read_text()
        self.assertEqual(text, current, "types.py is out of date: python3 tools/gen_types.py " + schema)


if __name__ == "__main__":
    unittest.main()
