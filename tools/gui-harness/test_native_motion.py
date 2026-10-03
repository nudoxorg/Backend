#!/usr/bin/env python3
"""Pure source checks for the native capture contract; no app/owner is seeded."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
import shutil
from PIL import Image

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("native_motion", HERE / "native_motion.py")
motion = importlib.util.module_from_spec(spec)
spec.loader.exec_module(motion)
spec_matrix = importlib.util.spec_from_file_location("native_motion_matrix", HERE / "native_motion_matrix.py")
matrix = importlib.util.module_from_spec(spec_matrix)
spec_matrix.loader.exec_module(matrix)


class NativeMotionTests(unittest.TestCase):
    def plan(self, directory, **edits):
        plan = json.loads((HERE / "plans/ask-open-close.json").read_text())
        plan.update(edits)
        path = directory / "plan.json"
        path.write_text(json.dumps(plan))
        return path

    def test_real_plan_contract_and_reduced_motion_mismatch(self):
        with tempfile.TemporaryDirectory() as root:
            directory = Path(root)
            resolved = motion.require_plan(self.plan(directory))
            self.assertEqual(resolved["capture_fps"], 30)
            bad = dict(resolved, expected_reduce_motion=True)
            with self.assertRaisesRegex(ValueError, "case motion"):
                motion.require_plan(self.plan(directory, **bad))
            with self.assertRaisesRegex(ValueError, "unknown plan fields"):
                motion.require_plan(self.plan(directory, accidental_route_seed=True))

    def test_native_pts_selects_cfr_only_for_measured_constant_cadence(self):
        self.assertEqual(motion.cadence([0, 33.3, 66.7, 100.0])["encoding"], "cfr")
        self.assertEqual(motion.cadence([0, 16.6, 100.0, 116.6])["encoding"], "vfr")
        with self.assertRaisesRegex(ValueError, "strictly"):
            motion.cadence([0, 16.6, 16.6])

    def test_delayed_pixels_fail_even_if_ax_focus_and_input_arrive(self):
        with tempfile.TemporaryDirectory() as root:
            out = Path(root)
            (out / "frames").mkdir()
            frames = []
            for index, at in enumerate([0, 40, 80, 120, 160, 200, 240, 280, 320, 360, 400, 440]):
                image = Image.new("RGB", (20, 20), "black")
                if at >= 400:
                    for x in range(10):
                        for y in range(10):
                            image.putpixel((x, y), (255, 255, 255))
                path = f"frames/frame-{index:06d}.png"
                image.save(out / path)
                frames.append({"index": index, "time_ms": at, "width_px": 20, "height_px": 20,
                               "file": path, "ax": {"focused": {"title": "Ask anything, or find a package"},
                                                    "window": {"bounds_pt": {"x": 0, "y": 0, "width": 20, "height": 20}}}})
            plan = {"duration_ms": 440, "max_frame_gap_ms": 50,
                    "actions": [{"kind": "key", "label": "open Ask", "at_ms": 80,
                                 "expect_visual_ms": 200, "min_visual_fraction": .02,
                                 "expect_focus_title": "Ask anything, or find a package"}]}
            actions = [{"phase": "action", "kind": "key", "label": "open Ask", "actual_ms": 80, "posted": {"keycode": 40}}]
            result = motion.analyze_frames(out, frames, actions, plan)
            self.assertTrue(any("no native pixel change" in finding for finding in result["failures"]))
            self.assertEqual(result["actions"][0]["first_visual_change_ms"], 400)
            self.assertEqual(len(result["window_geometry_changes"]), 1)
            self.assertEqual(motion.changed_fraction(out / frames[0]["file"], out / frames[-1]["file"]), .25)

    @unittest.skipUnless(shutil.which("ffmpeg") and shutil.which("ffprobe"), "ffmpeg/ffprobe unavailable")
    def test_vfr_movie_pts_retain_irregular_native_intervals(self):
        with tempfile.TemporaryDirectory() as root:
            out = Path(root)
            (out / "frames").mkdir()
            times = [0.0, 16.6, 83.3, 100.0, 250.0]
            frames = []
            for index, at in enumerate(times):
                file = f"frames/frame-{index:06d}.png"
                Image.new("RGB", (32, 32), (index * 40, 0, 0)).save(out / file)
                frames.append({"file": file, "time_ms": at})
            result = motion.encode(out, frames, motion.cadence(times), shutil.which("ffmpeg"))
            self.assertEqual(result["timing"]["encoding"], "vfr")
            self.assertLess(result["max_pts_error_ms"], 1)
            self.assertEqual(result["encoded_frame_count"], len(frames) + 1)

    def test_exact_crop_rejects_out_of_bounds_instead_of_guessing(self):
        with tempfile.TemporaryDirectory() as root:
            out = Path(root)
            (out / "frames").mkdir()
            image = out / "frames/frame-000000.png"
            Image.new("RGB", (20, 12), "red").save(image)
            frame = {"file": "frames/frame-000000.png", "time_ms": 0, "width_px": 20, "height_px": 12}
            with self.assertRaisesRegex(ValueError, "outside actual"):
                motion.crop_frames(out, {"crops": [{"label": "wrong", "at_ms": 0, "rect_px": [15, 0, 10, 10]}]}, [frame])
            result = motion.crop_frames(out, {"crops": [{"label": "exact", "at_ms": 0, "rect_px": [2, 3, 4, 5]}]}, [frame])
            self.assertEqual(result[0]["rect_px"], [2, 3, 4, 5])
            with Image.open(out / result[0]["file"]) as crop:
                self.assertEqual(crop.size, (4, 5))

    def test_incomplete_historical_receipt_is_unproven_binary_source(self):
        with tempfile.TemporaryDirectory() as root:
            directory = Path(root)
            binary = directory / "desktop"
            binary.write_bytes(b"historical binary")
            receipt = directory / "receipt.json"
            receipt.write_text(json.dumps({"head": "some-old-head", "binaries": {str(binary): motion.sha256(binary)}}))
            source = {"head": "current-head", "tree": "current-tree", "status_porcelain": "",
                      "cargo_lock_sha256": "current-lock"}
            result = motion.compiler_admission(receipt, binary, source)
            self.assertEqual(result["state"], "UnprovenBinarySource")
            self.assertIn("receipt HEAD/tree differs", "; ".join(result["reasons"]))
            self.assertIn("compiler run did not finish", "; ".join(result["reasons"]))

    def test_matrix_never_promotes_native_source_pixels_to_live_owner_proof(self):
        catalog = json.loads((HERE / "native_motion_matrix.json").read_text())
        source = next(case for case in catalog["required"] if case["id"] == "source_full")
        manifest = {"case": source, "class": catalog["evidence_class"], "passed_native_checks": True,
                    "inputs": [], "owner_receipt": None}
        result = matrix.assess({"required": [source], "evidence_class": catalog["evidence_class"]}, [manifest])
        self.assertEqual(result["rows"][0]["status"], "failed")
        self.assertFalse(result["native_coverage_complete"])
        self.assertIn("independent", result["live_index_acceptance"])


if __name__ == "__main__":
    unittest.main()
