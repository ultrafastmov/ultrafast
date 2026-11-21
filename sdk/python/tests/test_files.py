import copy
import tempfile
import unittest
import zipfile
from pathlib import Path

from ultrafastmov import _files


class Uploads:
    def __init__(self):
        self.files = []
        self.sequences = []

    def file(self, path):
        self.files.append(Path(path))
        return f"asset://f{len(self.files)}"

    def sequence(self, directory, pattern, files):
        self.sequences.append((Path(directory), pattern, [f.name for f in files]))
        return f"asset://z{len(self.sequences)}"


class LocalizeTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)
        for name in ("footage/a.mp4", "logo.svg", "fonts/Brand.otf", "music.mp3", "faces.json", "Inter.ttf", "seq/f_000001.png", "seq/f_000002.png", "seq/other.png"):
            p = self.dir / name
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_bytes(name.encode())
        self.up = Uploads()

    def tearDown(self):
        self.tmp.cleanup()

    def run_(self, doc):
        return _files.localize(doc, self.dir, self.up.file, self.up.sequence)

    def test_file_params_are_uploaded_and_the_input_is_not_changed(self):
        doc = {
            "nodes": {
                "clip": {"op": "video", "params": {"src": "footage/a.mp4", "start": 3}},
                "same": {"op": "audio", "params": {"src": "footage/a.mp4"}},
                "logo": {"op": "svg", "params": {"src": str(self.dir / "logo.svg")}},
                "title": {"op": "text", "params": {"text": "logo.svg", "font": "fonts/Brand.otf", "em_font": "ultrafast:serif"}},
                "name": {"op": "text", "params": {"text": "x", "font": "Inter"}},
                "file_font": {"op": "text", "params": {"text": "x", "font": "Inter.ttf"}},
                "remote": {"op": "image", "params": {"src": "https://example.com/a.png"}},
                "asset": {"op": "image", "params": {"src": "asset://abc"}},
                "gcs": {"op": "image", "params": {"src": "gs://b/x.png#1"}},
                "s3": {"op": "image", "params": {"src": "s3://b/x.png"}},
                "data": {"op": "image", "params": {"src": "data:image/png;base64,AAAA"}},
                "empty": {"op": "image", "params": {"src": ""}},
                "faces_inline": {"op": "clip_camera", "params": {"src": "footage/a.mp4", "faces": '{"frames": []}'}},
                "faces_file": {"op": "clip_camera", "params": {"faces": "faces.json"}},
            },
            "output": "clip",
        }
        before = copy.deepcopy(doc)
        out = self.run_(doc)
        self.assertEqual(doc, before)
        n = out["nodes"]
        self.assertTrue(n["clip"]["params"]["src"].startswith("asset://"))
        self.assertEqual(n["clip"]["params"]["src"], n["same"]["params"]["src"])
        self.assertEqual(n["clip"]["params"]["start"], 3)
        self.assertTrue(n["logo"]["params"]["src"].startswith("asset://"))
        self.assertEqual(n["title"]["params"]["text"], "logo.svg")  # words, not a file
        self.assertTrue(n["title"]["params"]["font"].startswith("asset://"))
        self.assertEqual(n["title"]["params"]["em_font"], "ultrafast:serif")
        self.assertEqual(n["name"]["params"]["font"], "Inter")
        self.assertTrue(n["file_font"]["params"]["font"].startswith("asset://"))
        for k, v in (("remote", "https://example.com/a.png"), ("asset", "asset://abc"), ("gcs", "gs://b/x.png#1"),
                     ("s3", "s3://b/x.png"), ("data", "data:image/png;base64,AAAA"), ("empty", "")):
            self.assertEqual(n[k]["params"]["src"], v)
        self.assertEqual(n["faces_inline"]["params"]["faces"], '{"frames": []}')
        self.assertTrue(n["faces_file"]["params"]["faces"].startswith("asset://"))
        uploaded = sorted(p.relative_to(self.dir).as_posix() for p in self.up.files)
        self.assertEqual(uploaded, ["Inter.ttf", "faces.json", "fonts/Brand.otf", "footage/a.mp4", "logo.svg"])

    def test_missing_file_raises_with_where(self):
        doc = {"nodes": {"clip": {"op": "video", "params": {"src": "footage/missing.mp4"}}}}
        with self.assertRaises(FileNotFoundError) as cm:
            self.run_(doc)
        self.assertIn("nodes.clip.params.src", str(cm.exception))

    def test_keyframes_components_meta_and_drivers(self):
        doc = {
            "nodes": {
                "flip": {"op": "image", "params": {"src": {"keys": [{"t": 0, "v": "logo.svg", "ease": "out_cubic"}, {"t": 1, "v": "footage/a.mp4"}]}}},
                "lt": {"op": "lower_third", "params": {"title": "Q3 / Q4", "logo": "logo.svg", "accent": "#22d3ee", "pos": [0, 820]}},
                "pulse": {"op": "shape", "params": {"scale": {"audio": {"src": "music.mp3", "band": "low"}}}},
            },
            "components": {
                "lower_third": {
                    "params": {"title": "Title", "logo": "logo.svg", "bed": "music.mp3"},
                    "nodes": {
                        "mark": {"op": "svg", "params": {"src": "$logo"}},
                        "pic": {"op": "image", "params": {"src": "footage/a.mp4"}},
                        "t": {"op": "text", "params": {"text": "$title", "font": "$font"}},
                    },
                    "output": "mark",
                }
            },
            "meta": {"fonts": [{"family": "Brand", "src": "fonts/Brand.otf"}, {"family": "X", "src": "gs://b/x.ttf#2"}]},
        }
        out = self.run_(doc)
        keys = out["nodes"]["flip"]["params"]["src"]["keys"]
        self.assertTrue(keys[0]["v"].startswith("asset://") and keys[1]["v"].startswith("asset://"))
        self.assertEqual(keys[0]["ease"], "out_cubic")
        lt = out["nodes"]["lt"]["params"]
        self.assertEqual(lt["title"], "Q3 / Q4")
        self.assertEqual(lt["accent"], "#22d3ee")
        self.assertEqual(lt["pos"], [0, 820])
        self.assertTrue(lt["logo"].startswith("asset://"))
        self.assertTrue(out["nodes"]["pulse"]["params"]["scale"]["audio"]["src"].startswith("asset://"))
        self.assertEqual(out["nodes"]["pulse"]["params"]["scale"]["audio"]["band"], "low")
        comp = out["components"]["lower_third"]
        self.assertEqual(comp["params"]["title"], "Title")
        self.assertTrue(comp["params"]["logo"].startswith("asset://"))
        self.assertTrue(comp["params"]["bed"].startswith("asset://"))
        self.assertEqual(comp["nodes"]["mark"]["params"]["src"], "$logo")
        self.assertEqual(comp["nodes"]["t"]["params"]["font"], "$font")
        self.assertTrue(comp["nodes"]["pic"]["params"]["src"].startswith("asset://"))
        self.assertTrue(out["meta"]["fonts"][0]["src"].startswith("asset://"))
        self.assertEqual(out["meta"]["fonts"][1]["src"], "gs://b/x.ttf#2")
        # the same file is uploaded once per call
        self.assertEqual(len(self.up.files), len(set(self.up.files)))

    def test_image_sequences_go_up_as_one_archive(self):
        doc = {"nodes": {"seq": {"op": "image", "params": {"src": "seq/f_{frame:06}.png", "fps": 30}}}}
        out = self.run_(doc)
        self.assertEqual(out["nodes"]["seq"]["params"]["src"], "asset://z1!/f_{frame:06}.png")
        self.assertEqual(self.up.sequences[0][2], ["f_000001.png", "f_000002.png"])
        with self.assertRaises(FileNotFoundError):
            self.run_({"nodes": {"seq": {"op": "image", "params": {"src": "seq/g_{frame}.png"}}}})

    def test_family_names(self):
        for name in ("Inter", "Helvetica Neue", "ultrafast:mono", "font"):
            self.assertTrue(_files.is_family_name(name), name)
        for path in ("fonts/Inter.ttf", "Inter.TTF", "./Inter", "~/f.otf", "a\\b", "X.woff2"):
            self.assertFalse(_files.is_family_name(path), path)

    def test_references(self):
        for ref in ("asset://x", "https://a/b", "http://a/b", "gs://b/o#1", "s3://b/k", "r2://b/k", "data:x", "blob://x"):
            self.assertTrue(_files.is_reference(ref), ref)
        for local in ("a.mp4", "/abs/a.mp4", "file:///abs/a.mp4", "C:\\x.mp4"):
            self.assertFalse(_files.is_reference(local), local)


class SequenceZipTest(unittest.TestCase):
    def test_client_zips_sequences_deterministically(self):
        from ultrafastmov import Ultrafast

        with tempfile.TemporaryDirectory() as d:
            for i in range(3):
                (Path(d) / f"frame_{i:04}.webp").write_bytes(bytes([i]) * 10)
            uf = Ultrafast(api_key="k", base_url="http://127.0.0.1:9")
            seen = []

            def fake_upload(path):
                with zipfile.ZipFile(path) as zf:
                    seen.append((zf.namelist(), Path(path).read_bytes()))
                return "asset://zip"

            uf._upload_file_cached = fake_upload
            files = _files.sequence_files(Path(d) / "frame_{frame:04}.webp")
            self.assertEqual(uf._upload_sequence_cached(Path(d), "frame_{frame:04}.webp", files), "asset://zip")
            uf._sequences.clear()
            uf._upload_sequence_cached(Path(d), "frame_{frame:04}.webp", files)
            self.assertEqual(seen[0][0], ["frame_0000.webp", "frame_0001.webp", "frame_0002.webp"])
            self.assertEqual(seen[0][1], seen[1][1])


if __name__ == "__main__":
    unittest.main()
