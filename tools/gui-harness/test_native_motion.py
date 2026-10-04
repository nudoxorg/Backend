#!/usr/bin/env python3
"""Pure source checks for the native capture contract; no app/owner is seeded."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import shutil
import subprocess
import sys
import socket
import threading
import time
from copy import deepcopy
import plistlib
from PIL import Image

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
spec = importlib.util.spec_from_file_location("native_motion", HERE / "native_motion.py")
motion = importlib.util.module_from_spec(spec)
spec.loader.exec_module(motion)
spec_matrix = importlib.util.spec_from_file_location("native_motion_matrix", HERE / "native_motion_matrix.py")
matrix = importlib.util.module_from_spec(spec_matrix)
spec_matrix.loader.exec_module(matrix)
spec_drawer = importlib.util.spec_from_file_location("derive_native_drawer_plan", HERE / "derive_native_drawer_plan.py")
drawer_plan = importlib.util.module_from_spec(spec_drawer)
spec_drawer.loader.exec_module(drawer_plan)


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

    def test_plan_snapshot_binds_parsed_bytes_and_detects_source_mutation(self):
        with tempfile.TemporaryDirectory() as root:
            directory = Path(root)
            source_path = self.plan(directory)
            source, raw = motion.plan_file_observation(source_path)
            resolved = motion.require_plan(source_path, raw)
            source_copy = motion.write_once_readonly(directory / "source-plan.snapshot.json", raw)
            resolved_path = directory / "resolved-plan.json"
            resolved_observation = motion.write_once_readonly(
                resolved_path, (json.dumps(resolved, indent=2) + "\n").encode())
            before, before_receipt = motion.plan_phase_receipt(
                directory, "prelaunch", source, source_copy, resolved_observation)
            self.assertEqual(before["state"], "Stable")
            self.assertEqual(source["sha256"], source_copy["sha256"])
            self.assertNotEqual(source["sha256"], resolved_observation["sha256"])
            self.assertEqual(before_receipt["nlink"], 1)
            self.assertEqual((directory / "PLAN-PRELAUNCH.json").stat().st_mode & 0o222, 0)
            with self.assertRaises(FileExistsError):
                motion.write_once_readonly(directory / "PLAN-PRELAUNCH.json", b"replacement")

            source_path.write_text(json.dumps(dict(json.loads(raw), name="changed after parsing")))
            self.assertEqual(resolved["name"], json.loads(raw)["name"])
            after, _ = motion.plan_phase_receipt(directory, "postlaunch", source,
                                                 source_copy, resolved_observation)
            self.assertEqual(after["state"], "Changed")
            self.assertEqual(after["checks"]["source"]["state"], "Changed")
            self.assertEqual(after["checks"]["source_copy"]["state"], "Stable")
            self.assertEqual(after["checks"]["resolved"]["state"], "Stable")
            receipt_path = directory / "PLAN-PRELAUNCH.json"
            receipt_path.chmod(0o644)
            receipt_path.write_text('{"substituted":true}\n')
            self.assertEqual(motion.plan_snapshot_check(before_receipt)["state"], "Changed")

    def test_resolved_plan_mutation_is_detected_and_active_capture_is_unqualified(self):
        with tempfile.TemporaryDirectory() as root:
            directory = Path(root)
            source_path = self.plan(directory)
            source, raw = motion.plan_file_observation(source_path)
            plan = motion.require_plan(source_path, raw)
            source_copy = motion.write_once_readonly(directory / "source-plan.snapshot.json", raw)
            resolved_path = directory / "resolved-plan.json"
            resolved = motion.write_once_readonly(resolved_path, (json.dumps(plan) + "\n").encode())
            before, _ = motion.plan_phase_receipt(directory, "prelaunch", source, source_copy, resolved)
            self.assertEqual(before["state"], "Stable")
            resolved_path.chmod(0o644)
            resolved_path.write_text('{"substituted":true}\n')
            after, _ = motion.plan_phase_receipt(directory, "postlaunch", source, source_copy, resolved)
            self.assertEqual(after["checks"]["source"]["state"], "Stable")
            self.assertEqual(after["checks"]["resolved"]["state"], "Changed")
            self.assertEqual(after["state"], "Changed")
            self.assertFalse(motion.plan_consumption_qualification(plan)["launch_admissible"])
            self.assertFalse(motion.plan_consumption_qualification(plan)["exact_bytes_attested"])
            self.assertTrue(motion.plan_consumption_qualification(dict(plan, actions=[]))["launch_admissible"])
            self.assertEqual(motion.plan_consumption_qualification(plan, attest_v1=True)["state"], "PendingV1")
            self.assertTrue(motion.plan_consumption_qualification(plan, attest_v1=True)["launch_admissible"])

    def test_versioned_recorder_argv_is_explicit_and_legacy_is_unchanged(self):
        plan = Path("/private/tmp/capture/resolved-plan.json")
        out = Path("/private/tmp/capture")
        digest = "a" * 64
        self.assertEqual(motion.recorder_arguments(43542, plan, out),
                         ["43542", str(plan), str(out)])
        self.assertEqual(motion.recorder_arguments(43542, plan, out, digest),
                         ["43542", str(plan), str(out), "--expected-plan-sha256-v1", digest])
        with self.assertRaisesRegex(ValueError, "lowercase SHA-256"):
            motion.recorder_arguments(43542, plan, out, digest.upper())
        self.assertEqual(motion.launch_services_command(Path("/tmp/recorder.app"),
            43542, plan, out, digest)[-5:],
            ["43542", str(plan), str(out), "--expected-plan-sha256-v1", digest])

    def test_consumed_receipt_requires_exact_bytes_identity_and_callback(self):
        with tempfile.TemporaryDirectory() as root:
            out = Path(root)
            expected = {"path": str(out / "resolved-plan.json"), "bytes": 57, "sha256": "b" * 64}
            recorder = out / "NudoxMotionRecorder"
            identity = {"identifier": "dev.nudox.audit.motion-recorder", "executable_sha256": "a" * 64}
            self.assertEqual(motion.consumed_plan_receipt(out, expected, recorder, identity, 43542)["state"], "Missing")
            row = {"schema": 1, "kind": "native-motion-plan-consumption-v1", "state": "MatchedV1",
                   "pid": 43542, "plan_path": expected["path"], "consumed_bytes": expected["bytes"],
                   "consumed_sha256": expected["sha256"], "expected_sha256": expected["sha256"],
                   "recorder_bundle_identifier": identity["identifier"],
                   "recorder_executable": str(recorder),
                   "recorder_executable_sha256": identity["executable_sha256"],
                   "stream_output_callback_ready": True}
            receipt = out / "plan-consumption.jsonl"
            motion.write_once_readonly(receipt, (json.dumps(row) + "\n").encode())
            verified = motion.consumed_plan_receipt(out, expected, recorder, identity, 43542)
            self.assertEqual(verified["state"], "VerifiedV1")
            self.assertTrue(verified["exact_bytes_attested"])
            receipt.chmod(0o644)
            receipt.write_text(json.dumps(dict(row, consumed_sha256="c" * 64)) + "\n")
            rejected = motion.consumed_plan_receipt(out, expected, recorder, identity, 43542)
            self.assertEqual(rejected["state"], "InvalidV1")  # Writable receipt is never admitted.
            receipt.chmod(0o444)
            rejected = motion.consumed_plan_receipt(out, expected, recorder, identity, 43542)
            self.assertEqual(rejected["state"], "RejectedV1")
            self.assertIn("consumed_sha256", rejected["mismatched_fields"])
            self.assertFalse(rejected["exact_bytes_attested"])
            receipt.chmod(0o644)
            receipt.write_text(json.dumps(dict(row, stream_output_callback_ready=False)) + "\n")
            receipt.chmod(0o444)
            rejected = motion.consumed_plan_receipt(out, expected, recorder, identity, 43542)
            self.assertIn("stream_output_callback_ready", rejected["mismatched_fields"])
            receipt.chmod(0o644)
            receipt.write_text(json.dumps(dict(row, state="MismatchV1", consumed_sha256="c" * 64)) + "\n")
            receipt.chmod(0o444)
            (out / "actions.jsonl").write_text("")
            (out / "frames.jsonl").write_text("")
            mismatch = motion.consumed_plan_receipt(out, expected, recorder, identity, 43542)
            self.assertFalse(mismatch["verified"])
            self.assertEqual(motion.failed_recorder_evidence(out)["action_rows"], 0)
            self.assertEqual(motion.failed_recorder_evidence(out)["plan_consumption"]["state"], "MismatchV1")

    def test_actual_resolved_plan_mutations_cannot_qualify_a_consumption_receipt(self):
        for mutated in (b'{"active":false}\n', b'{"active":oops}\n'):
            with self.subTest(mutated=mutated), tempfile.TemporaryDirectory() as root:
                out = Path(root)
                plan_path = out / "resolved-plan.json"
                expected = motion.write_once_readonly(plan_path, b'{"active":true}\n')
                plan_path.chmod(0o644)
                plan_path.write_bytes(mutated)
                actual, actual_bytes = motion.plan_file_observation(plan_path)
                self.assertEqual(actual_bytes, mutated)
                self.assertNotEqual(actual["sha256"], expected["sha256"])
                self.assertEqual(motion.plan_snapshot_check(expected)["state"], "Changed")
                recorder = out / "NudoxMotionRecorder"
                identity = {"identifier": "dev.nudox.audit.motion-recorder",
                            "executable_sha256": "a" * 64}
                receipt = {"schema": 1, "kind": "native-motion-plan-consumption-v1",
                           "state": "MismatchV1", "pid": 43542,
                           "plan_path": str(plan_path), "consumed_bytes": len(actual_bytes),
                           "consumed_sha256": actual["sha256"],
                           "expected_sha256": expected["sha256"],
                           "recorder_bundle_identifier": identity["identifier"],
                           "recorder_executable": str(recorder),
                           "recorder_executable_sha256": identity["executable_sha256"],
                           "stream_output_callback_ready": True}
                motion.write_once_readonly(out / "plan-consumption.jsonl",
                                           (json.dumps(receipt) + "\n").encode())
                (out / "actions.jsonl").write_bytes(b"")
                (out / "frames.jsonl").write_bytes(b"")
                qualification = motion.consumed_plan_receipt(out, expected, recorder,
                                                              identity, 43542)
                self.assertEqual(qualification["state"], "RejectedV1")
                self.assertFalse(qualification["exact_bytes_attested"])
                self.assertEqual(motion.failed_recorder_evidence(out)["action_rows"], 0)
                self.assertEqual(motion.failed_recorder_evidence(out)["plan_consumption"], receipt)

    def test_swift_bounded_hashes_same_data_and_rejects_mismatch_before_decode(self):
        source = (HERE / "native_motion.swift").read_text()
        read = source.index("let planData = try readBoundedPlan(")
        digest = source.index("let consumedPlanSHA256 = sha256Hex(planData)", read)
        mismatch = source.index('if let expectedPlanSHA256, expectedPlanSHA256 != consumedPlanSHA256', digest)
        mismatch_receipt = source.index('try writeConsumption("MismatchV1"', mismatch)
        decode = source.index("JSONDecoder().decode(Plan.self, from: planData)", mismatch_receipt)
        receipt = source.index("try writeConsumption(consumptionState", decode)
        capture = source.index("SCShareableContent.current", receipt)
        self.assertLess(read, digest)
        self.assertIn("Darwin.open(url.path, O_RDONLY | O_NOFOLLOW)", source)
        self.assertIn("limit + 1 - data.count", source)
        self.assertLess(digest, mismatch)
        self.assertLess(mismatch_receipt, decode)
        self.assertLess(decode, receipt)
        self.assertLess(receipt, capture)

    def test_swift_source_binds_optional_screen_output_selector(self):
        # SCStreamOutput's frame callback is optional. A wrong Swift external
        # label compiles but never receives pixels, so compile-time references
        # and the runtime selector admission must both remain present.
        source = (HERE / "native_motion.swift").read_text()
        self.assertRegex(source, r"@objc\s+func stream\(_ stream: SCStream, "
                         r"didOutputSampleBuffer sampleBuffer: CMSampleBuffer, "
                         r"of type: SCStreamOutputType\)")
        self.assertIn("#selector(SCStreamOutput.stream(_:didOutputSampleBuffer:of:))", source)
        self.assertIn("#selector(Recorder.stream(_:didOutputSampleBuffer:of:))", source)
        self.assertIn("recorder.responds(to: outputSelector)", source)
        self.assertIn('preflightFailures.append("SCStreamOutputCallbackUnavailable")', source)
        self.assertNotRegex(source, r"func stream\(_ stream: SCStream, sampleBuffer:")

    def test_failed_recorder_preserves_preflight_without_claiming_frames(self):
        with tempfile.TemporaryDirectory() as root:
            out = Path(root)
            preflight = {"schema": 1, "state": "Rejected", "pid": 14636,
                         "frontmost_pid": 10250, "ax_trusted": False,
                         "screen_recording_preflight_granted": False,
                         "failures": ["ScreenRecordingPreflightDenied", "AccessibilityTrustDenied",
                                      "TargetNotFrontmost"]}
            (out / "preflight.jsonl").write_text(json.dumps(preflight) + "\n")
            (out / "actions.jsonl").write_text("")
            (out / "frames.jsonl").write_text("")
            evidence = motion.failed_recorder_evidence(out)
            self.assertEqual(evidence["state"], "RecorderFailedBeforeAnalysis")
            self.assertEqual(evidence["preflight"], preflight)
            self.assertEqual((evidence["frame_rows"], evidence["action_rows"]), (0, 0))
            self.assertNotIn("window", evidence)

            (out / "preflight.jsonl").write_text(json.dumps(dict(preflight, state="Admitted",
                failures=[], ax_trusted=True, screen_recording_preflight_granted=True,
                frontmost_pid=14636)) + "\n")
            (out / "window.jsonl").write_text('{"window_id":29995}\n')
            (out / "stream-state.jsonl").write_text('{"state":"NoFirstFrameAfterStartCapture",'
                '"captured_frames":0,"stream_delegate_failure":""}\n')
            evidence = motion.failed_recorder_evidence(out)
            self.assertEqual(evidence["stream_state"]["state"], "NoFirstFrameAfterStartCapture")
            self.assertEqual(evidence["window"]["window_id"], 29995)
            self.assertEqual((evidence["frame_rows"], evidence["action_rows"]), (0, 0))

    def test_recorder_admits_team_stable_or_one_frozen_adhoc_content_address(self):
        with tempfile.TemporaryDirectory() as root:
            app = Path(root) / "NudoxMotionRecorder.app"
            executable = app / "Contents/MacOS/NudoxMotionRecorder"
            executable.parent.mkdir(parents=True)
            executable.write_bytes(b"recorder fixture")
            info = app / "Contents/Info.plist"
            info.write_bytes(plistlib.dumps({"CFBundleIdentifier": "dev.nudox.audit.motion-recorder.run19",
                "CFBundleExecutable": executable.name, "NSScreenCaptureUsageDescription": "QA capture"}))
            def codesign(command, **_kwargs):
                if "--verify" in command:
                    return subprocess.CompletedProcess(command, 0, "valid")
                if "--requirements" in command:
                    return subprocess.CompletedProcess(command, 0,
                        'designated => identifier "dev.nudox.audit.motion-recorder.run19" and anchor apple generic')
                return subprocess.CompletedProcess(command, 0,
                    "Identifier=dev.nudox.audit.motion-recorder.run19\nTeamIdentifier=TESTTEAM\n"
                    "Authority=Apple Development: QA Tester (TESTTEAM)\nCDHash=" + "b" * 40 + "\n")
            with patch.object(motion.subprocess, "run", side_effect=codesign):
                identity = motion.recorder_identity(executable)
                self.assertEqual(identity["kind"], "TeamSignedStableAcrossBuilds")
                self.assertEqual(identity["team_identifier"], "TESTTEAM")
                self.assertEqual(identity["identifier"], "dev.nudox.audit.motion-recorder.run19")
            digest = "a" * 40
            frozen_app = Path(root) / f"NudoxMotionRecorder-{digest}.app"
            shutil.copytree(app, frozen_app)
            frozen_executable = frozen_app / "Contents/MacOS/NudoxMotionRecorder"
            def adhoc(command, **_kwargs):
                if "--verify" in command:
                    return subprocess.CompletedProcess(command, 0, "valid")
                if "--requirements" in command:
                    return subprocess.CompletedProcess(command, 0, f'# designated => cdhash H"{digest}"')
                if "--verbose=4" in command:
                    return subprocess.CompletedProcess(command, 0,
                        f"Signature=adhoc\nTeamIdentifier=not set\nCDHash={digest}\n")
                raise AssertionError(command)
            with patch.object(motion.subprocess, "run", side_effect=adhoc):
                frozen = motion.recorder_identity(frozen_executable)
                self.assertEqual(frozen["kind"], "AdHocFrozenContentAddressed")
                self.assertEqual(frozen["cdhash"], digest)
                self.assertEqual(frozen["path"], str(frozen_app.resolve()))
                with self.assertRaisesRegex(ValueError, "CDHash-versioned"):
                    motion.recorder_identity(executable)
                frozen_executable.write_bytes(b"changed after admission")
                self.assertNotEqual(motion.recorder_identity(frozen_executable), frozen)

    def test_passive_plan_records_foreground_without_requiring_it(self):
        passive = motion.require_plan(HERE / "plans/settings-15s-settle.json")
        self.assertFalse(passive["foreground_required"])
        active = motion.require_plan(HERE / "plans/settings-fast-open-close.json")
        self.assertTrue(active["foreground_required"])
        with tempfile.TemporaryDirectory() as root:
            directory = Path(root)
            required = dict(passive, require_frontmost=True, foreground_required=True)
            self.assertTrue(motion.require_plan(self.plan(directory, **required))["foreground_required"])
            with self.assertRaisesRegex(ValueError, "foreground_required must match"):
                motion.require_plan(self.plan(directory, **dict(passive, foreground_required=True)))
        with tempfile.TemporaryDirectory() as root:
            directory = Path(root)
            probe_only = json.loads((HERE / "plans/ask-open-close.json").read_text())
            probe_only["actions"] = [{"at_ms": 100, "kind": "probe", "label": "read native AX"}]
            self.assertFalse(motion.require_plan(self.plan(directory, **probe_only))["foreground_required"])

    def test_launch_services_binds_exact_recorder_and_zero_input(self):
        with tempfile.TemporaryDirectory() as root:
            app = Path(root) / "NudoxMotionRecorder-" / "NudoxMotionRecorder.app"
            recorder = app / "Contents/MacOS/NudoxMotionRecorder"
            recorder.parent.mkdir(parents=True)
            recorder.write_bytes(b"frozen fixture")
            identity = {"path": str(app.resolve()), "identifier": "dev.nudox.audit.motion-recorder"}
            plan = {"actions": [], "foreground_required": False}
            self.assertEqual(motion.require_launch_bundle(app, recorder.resolve(), identity, plan), app.resolve())
            with self.assertRaisesRegex(ValueError, "zero-input"):
                motion.require_launch_bundle(app, recorder.resolve(), identity,
                                             {"actions": [{"kind": "key"}]})
            with self.assertRaisesRegex(ValueError, "does not match"):
                motion.require_launch_bundle(app, recorder.resolve(), dict(identity, path="/wrong.app"), plan)
            self.assertEqual(motion.require_launch_bundle(app, recorder.resolve(), identity,
                {"actions": [{"kind": "key"}]}, supervised=True), app.resolve())

    def test_supervised_held_cancel_only_permits_planned_up(self):
        actions = [{"kind": "mouse_down"}, {"kind": "key"}, {"kind": "mouse_up"}]
        policy = motion.supervision.ControlPolicy(actions)
        driver, recorder = socket.socketpair()
        checked_held = threading.Event()
        resume = threading.Event()
        outcome = {}

        def fake_recorder():
            channel = motion.supervision.LineChannel(recorder)
            channel.send(kind="Hello", nonce="one-run")
            self.assertEqual(channel.receive(2)["kind"], "Arm")
            channel.send(kind="permit", index=0, action_kind="mouse_down", held=False)
            self.assertEqual(channel.receive(2)["kind"], "Permit")
            channel.send(kind="done", index=0, action_kind="mouse_down", posted=True, held=True)
            self.assertEqual(channel.receive(2)["kind"], "Continue")
            channel.send(kind="check", held=True)
            self.assertEqual(channel.receive(2)["kind"], "Continue")
            checked_held.set()
            self.assertTrue(resume.wait(2))
            # Cancel lands after the recorder's checkpoint, before Permit.
            channel.send(kind="permit", index=1, action_kind="key", held=True)
            self.assertEqual(channel.receive(2)["kind"], "HeldAwaitRelease")
            channel.send(kind="permit", index=2, action_kind="mouse_up", held=True)
            self.assertEqual(channel.receive(2)["kind"], "PermitRelease")
            channel.send(kind="done", index=2, action_kind="mouse_up", posted=True, held=False)
            self.assertEqual(channel.receive(2)["kind"], "Cancel")
            channel.send(kind="stopped", held=False)
            self.assertEqual(channel.receive(2)["kind"], "StopAck")
            recorder.close()

        def serve():
            outcome.update(motion.supervision.serve(driver, policy, time.monotonic() + 2,
                                                    lambda row: self.assertEqual(row["nonce"], "one-run")))

        producer = threading.Thread(target=fake_recorder)
        controller = threading.Thread(target=serve)
        producer.start(); controller.start()
        self.assertTrue(checked_held.wait(2))
        policy.cancel()
        resume.set()
        producer.join(3); controller.join(3)
        driver.close()
        self.assertFalse(producer.is_alive() or controller.is_alive())
        self.assertEqual(outcome["state"], "Stopped")
        self.assertTrue(outcome["cancel_ack"])
        self.assertFalse(outcome["held_unreleased"])
        self.assertNotIn("Permit", [row["reply"] for row in outcome["transcript"]
                                    if row["request"].get("index") == 1])

    def test_supervised_wrong_release_and_held_stop_do_not_release(self):
        policy = motion.supervision.ControlPolicy([
            {"kind": "mouse_down"}, {"kind": "mouse_up"}, {"kind": "key"}])
        self.assertEqual(policy.handle({"kind": "permit", "index": 0,
            "action_kind": "mouse_down", "held": False}), "Permit")
        self.assertEqual(policy.handle({"kind": "done", "index": 0,
            "action_kind": "mouse_down", "posted": True, "held": True}), "Continue")
        policy.cancel()
        self.assertEqual(policy.handle({"kind": "permit", "index": 2,
            "action_kind": "key", "held": True}), "HeldAwaitRelease")
        self.assertEqual(policy.handle({"kind": "stopped", "held": True}), "HeldUnreleased")
        self.assertFalse(policy.cancel_ack)
        self.assertEqual(policy.state, "StoppedHeldUnreleased")

    def test_supervised_cancel_racing_inflight_post_waits_for_done(self):
        policy = motion.supervision.ControlPolicy([{"kind": "key"}, {"kind": "click"}])
        self.assertEqual(policy.handle({"kind": "permit", "index": 0,
            "action_kind": "key", "held": False}), "Permit")
        policy.cancel()
        self.assertFalse(policy.cancel_ack)
        self.assertEqual(policy.handle({"kind": "done", "index": 0,
            "action_kind": "key", "posted": True, "held": False}), "Cancel")
        self.assertEqual(policy.handle({"kind": "permit", "index": 1,
            "action_kind": "click", "held": False}), "Cancel")
        self.assertEqual(policy.handle({"kind": "stopped", "held": False}), "StopAck")
        self.assertTrue(policy.cancel_ack)

    def test_supervised_message_deadline_cannot_be_extended_by_byte_trickle(self):
        class Clock:
            now = 100.0
            def monotonic(self):
                return self.now
        clock = Clock()
        class Trickle:
            timeout = None
            def settimeout(self, value):
                self.timeout = value
            def recv(self, size):
                delay = 0.08
                if self.timeout < delay:
                    clock.now += self.timeout
                    raise socket.timeout("next byte arrives too late")
                clock.now += delay
                return b"x"  # Every byte arrives before a reset timeout.
        channel = motion.supervision.LineChannel(Trickle())
        with patch.object(motion.supervision.time, "monotonic", clock.monotonic):
            with self.assertRaisesRegex(motion.supervision.ProtocolError, "partial native control"):
                channel.receive(0.20)
            with self.assertRaisesRegex(motion.supervision.ProtocolError, "partial native control"):
                channel.receive(0.20)  # A second call cannot reset this line.
        self.assertAlmostEqual(clock.now, 100.20, places=5)

    def test_supervised_late_permit_line_is_cancelled_before_policy_handles_it(self):
        class Clock:
            now = 0.0
            def monotonic(self):
                return self.now
        clock = Clock()
        class DelayedChannel:
            def __init__(self, connection):
                self.replies = []
                self.buffer = bytearray()
            def receive(self, timeout):
                if not self.replies:
                    return {"schema": 1, "kind": "Hello"}
                if len(self.replies) == 1:
                    clock.now = 11.0  # Complete Permit after deadline 10.
                    return {"schema": 1, "kind": "permit", "index": 0,
                            "action_kind": "key", "held": False}
                return {"schema": 1, "kind": "stopped", "held": False}
            def send(self, **fields):
                self.replies.append(fields["kind"])
        channel = DelayedChannel(None)
        with patch.object(motion.supervision, "LineChannel", return_value=channel), \
             patch.object(motion.supervision.time, "monotonic", clock.monotonic):
            result = motion.supervision.serve(None,
                motion.supervision.ControlPolicy([{"kind": "key"}]),
                10.0, lambda row: self.assertEqual(row["kind"], "Hello"))
        self.assertEqual(channel.replies, ["Arm", "Cancel", "StopAck"])
        self.assertTrue(result["cancel_ack"])

    def test_kernel_exit_watch_is_bound_to_registered_child_lifetime(self):
        # This child performs no GUI or recorder work. It tests the actual
        # macOS kqueue NOTE_EXIT path instead of a mocked process-status row.
        child = subprocess.Popen(["/bin/sleep", "2"])
        watcher = None
        try:
            watcher = motion.supervision.ExactProcessExit(child.pid)
            self.assertFalse(watcher.wait(0.01))
            self.assertIsNone(child.poll())
            self.assertTrue(watcher.wait(3.0))
            child.wait(timeout=3)
            other = subprocess.Popen(["/bin/sleep", "0.1"])
            try:
                # The first watch is already latched regardless of what PID
                # the kernel assigns this unrelated later process.
                self.assertTrue(watcher.wait(0.0))
            finally:
                other.wait(timeout=3)
        finally:
            if watcher is not None:
                watcher.close()
            if child.poll() is None:
                child.terminate()
                child.wait(timeout=3)

    def test_socket_eof_is_separate_from_stop_and_exit_receipts(self):
        a, b = socket.socketpair()
        try:
            self.assertFalse(motion.supervision.peer_eof_after_exit(a))
            b.close()
            self.assertTrue(motion.supervision.peer_eof_after_exit(a))
        finally:
            a.close()
            b.close()

    def test_supervised_hello_mismatch_never_arms(self):
        driver, recorder = socket.socketpair()
        recorder.settimeout(1)
        policy = motion.supervision.ControlPolicy([{"kind": "key"}])
        channel = motion.supervision.LineChannel(recorder)
        channel.send(kind="Hello", nonce="wrong")
        def reject(row):
            if row["nonce"] != "expected":
                raise motion.supervision.ProtocolError("wrong nonce")
        with self.assertRaisesRegex(motion.supervision.ProtocolError, "wrong nonce"):
            motion.supervision.serve(driver, policy, time.monotonic() + 1,
                                     reject)
        driver.close()
        self.assertEqual(recorder.recv(1), b"")
        recorder.close()

    def test_supervised_launch_requires_native_stop_and_kernel_exit(self):
        with tempfile.TemporaryDirectory() as root:
            out = Path(root)
            recorder_path = out / "NudoxMotionRecorder"
            recorder_path.write_bytes(b"fixture")
            identity = {"identifier": "dev.nudox.audit.motion-recorder",
                        "executable_sha256": "a" * 64}
            expected = {"path": str(out / "resolved-plan.json"), "bytes": 10,
                        "sha256": "b" * 64}
            plan = {"duration_ms": 500, "actions": [{"kind": "key"}]}

            class Launcher:
                returncode = 0
                def __init__(self, argv):
                    self.argv = argv
                    self.worker = threading.Thread(target=self.recorder)
                    self.worker.start()
                def recorder(self):
                    path, nonce_path = self.argv[-2:]
                    nonce = Path(nonce_path).read_text()
                    with socket.socket(socket.AF_UNIX) as sock:
                        sock.connect(path)
                        channel = motion.supervision.LineChannel(sock)
                        channel.send(kind="Hello", nonce=nonce, recorder_pid=2222,
                            target_pid=43542, recorder_executable=str(recorder_path),
                            recorder_executable_sha256=identity["executable_sha256"],
                            recorder_bundle_identifier=identity["identifier"],
                            consumed_sha256=expected["sha256"])
                        assert channel.receive(3)["kind"] == "Arm"
                        channel.send(kind="permit", index=0, action_kind="key", held=False)
                        assert channel.receive(3)["kind"] == "Permit"
                        channel.send(kind="done", index=0, action_kind="key", posted=True, held=False)
                        assert channel.receive(3)["kind"] == "Continue"
                        channel.send(kind="stopped", held=False)
                        assert channel.receive(3)["kind"] == "StopAck"
                    if self.write_receipt:
                        motion.write_once_readonly(out / "native-control-stop.jsonl",
                            (json.dumps({"schema": 1, "kind": "native-motion-control-stop-v1",
                                "recorder_pid": 2222, "held_unreleased": False,
                                "controller_reply": "StopAck"}) + "\n").encode())
                def communicate(self, timeout):
                    self.worker.join(timeout)
                    if self.worker.is_alive():
                        raise subprocess.TimeoutExpired("open", timeout)
                    return ("launcher waited for recorder\n", None)
                def poll(self):
                    return None if self.worker.is_alive() else 0
                def terminate(self):
                    raise AssertionError("verified completion must not signal launcher")

            class Watcher:
                def __init__(self, pid):
                    self.pid = pid
                def wait(self, seconds):
                    return self.exit_seen
                def close(self):
                    pass

            for exit_seen, stop_present in ((True, True), (False, True), (True, False)):
                (out / "native-control-stop.jsonl").unlink(missing_ok=True)
                (out / "SUPERVISION.json").unlink(missing_ok=True)
                (out / "INPUT-OWNERSHIP-UNRESOLVED.json").unlink(missing_ok=True)
                (out / "recorder.stdout").unlink(missing_ok=True)
                (out / "recorder.stderr").unlink(missing_ok=True)
                (out / "launcher.log").unlink(missing_ok=True)
                Watcher.exit_seen = exit_seen
                Launcher.write_receipt = stop_present
                with patch.object(motion.subprocess, "Popen", side_effect=lambda argv, **kw: Launcher(argv)), \
                     patch.object(motion, "process_executable", return_value=recorder_path), \
                     patch.object(motion, "recorder_identity", return_value=identity), \
                     patch.object(motion, "consumed_plan_receipt", return_value={"verified": True}), \
                     patch.object(motion.supervision, "peer_pid", return_value=2222), \
                     patch.object(motion.supervision, "ExactProcessExit", Watcher):
                    result = motion.launch_services_supervised(out / "bundle.app", 43542,
                        out / "resolved-plan.json", out, plan, recorder_path, identity, expected)
                self.assertEqual(result["supervision"]["exact_process_exit"], exit_seen)
                self.assertEqual(result["supervision"]["input_ownership_release_admitted"],
                                 exit_seen and stop_present)
                if stop_present:
                    self.assertEqual(result["supervision"]["native_stop_receipt"]["row"]["controller_reply"], "StopAck")
                else:
                    self.assertIsNone(result["supervision"]["native_stop_receipt"])
                self.assertEqual((out / "INPUT-OWNERSHIP-UNRESOLVED.json").exists(),
                                 not (exit_seen and stop_present))
                self.assertTrue(result["argv"][-1].endswith("/nonce"))
                self.assertFalse(Path(result["argv"][-1]).exists())
                self.assertNotIn(result["supervision"]["nonce_sha256"], (out / "launcher.log").read_text())

    def test_supervised_setup_error_preserves_unresolved_receipt_without_launch(self):
        with tempfile.TemporaryDirectory() as root:
            out = Path(root)
            with patch.object(motion.supervision, "private_socket", side_effect=OSError("bind rejected")), \
                 patch.object(motion.subprocess, "Popen") as popen:
                result = motion.launch_services_supervised(out / "bundle.app", 43542,
                    out / "resolved-plan.json", out, {"duration_ms": 500, "actions": []},
                    out / "recorder", {"identifier": "dev.nudox.audit.motion-recorder",
                        "executable_sha256": "a" * 64}, {"sha256": "b" * 64})
            popen.assert_not_called()
            self.assertFalse(result["supervision"]["input_ownership_release_admitted"])
            self.assertIn("bind rejected", "; ".join(result["supervision"]["failures"]))
            self.assertEqual(json.loads((out / "INPUT-OWNERSHIP-UNRESOLVED.json").read_text())["state"],
                             "Unresolved")

    def test_launch_services_wait_does_not_promote_launcher_exit_to_recorder_exit(self):
        with tempfile.TemporaryDirectory() as root:
            out = Path(root)
            app = out / "NudoxMotionRecorder.app"
            plan_path = out / "resolved-plan.json"
            plan_path.write_text("{}")
            class Finished:
                pid = 1234
                returncode = 0
                def communicate(self, timeout):
                    return ("open waited for app close\n", None)
            with patch.object(motion.subprocess, "Popen", return_value=Finished()) as popen:
                launch = motion.launch_services_wait(app, 43542, plan_path, out, 30)
            argv = popen.call_args.args[0]
            self.assertEqual(argv[:6], ["/usr/bin/open", "-n", "-g", "-W", "-a", str(app)])
            self.assertEqual(argv[-4:], ["--args", "43542", str(plan_path), str(out)])
            self.assertTrue(popen.call_args.kwargs["start_new_session"])
            self.assertIsNone(launch["recorder_exit"])
            self.assertTrue(launch["recorder_may_continue"])
            self.assertEqual(launch["launcher_exit_code"], 0)
            self.assertIn("Unverified", launch["tcc_responsibility"])
            self.assertEqual(launch["logs"]["recorder.stderr"]["size"], 0)
            with self.assertRaises(FileExistsError):
                motion.launch_services_wait(app, 43542, plan_path, out, 30)
            identity = {"identifier": "dev.nudox.audit.motion-recorder"}
            recorder = app / "Contents/MacOS/NudoxMotionRecorder"
            plan = {"foreground_required": False}
            admitted, _, failures, classification = motion.launch_services_completion(out, launch, recorder,
                                                                        identity, 43542, plan)
            self.assertFalse(admitted)
            self.assertIn("result sidecar absent", "; ".join(failures))
            (out / "result.jsonl").write_text('{"captured_frames":1,"dropped_frames":0}\n')
            preflight = {"schema": 1, "state": "Admitted", "failures": [],
                         "screen_recording_preflight_granted": True, "ax_trusted": True,
                         "frontmost_pid": 43542, "recorder_executable": str(recorder),
                         "recorder_bundle_identifier": identity["identifier"],
                         "pid": 43542, "foreground_required": False,
                         "stream_output_callback_ready": True}
            (out / "preflight.jsonl").write_text(json.dumps(preflight) + "\n")
            admitted, row, failures, classification = motion.launch_services_completion(out, launch, recorder,
                                                                          identity, 43542, plan)
            self.assertTrue(admitted)
            self.assertEqual(row, preflight)
            self.assertEqual(failures, [])
            (out / "preflight.jsonl").write_text(json.dumps(dict(preflight,
                stream_output_callback_ready=False)) + "\n")
            admitted, _, failures, classification = motion.launch_services_completion(out, launch, recorder,
                                                                        identity, 43542, plan)
            self.assertFalse(admitted)
            self.assertIn(classification["state"], ("Invalid", "IdentityMismatch"))
            (out / "preflight.jsonl").write_text(json.dumps({
                key: value for key, value in preflight.items()
                if key != "stream_output_callback_ready"}) + "\n")
            admitted, _, failures, classification = motion.launch_services_completion(out, launch, recorder,
                                                                        identity, 43542, plan)
            self.assertFalse(admitted)
            self.assertIn(classification["state"], ("Invalid", "IdentityMismatch"))
            (out / "preflight.jsonl").write_text(json.dumps(preflight) + "\n")
            admitted, _, failures, classification = motion.launch_services_completion(out,
                dict(launch, launcher_exit_code=1), recorder, identity, 43542, plan)
            self.assertFalse(admitted)
            self.assertIn("launcher exited nonzero", "; ".join(failures))
            (out / "preflight.jsonl").write_text(json.dumps(dict(preflight,
                recorder_executable="/different/recorder")) + "\n")
            admitted, _, failures, classification = motion.launch_services_completion(out, launch, recorder,
                                                                        identity, 43542, plan)
            self.assertFalse(admitted)
            self.assertIn(classification["state"], ("Invalid", "IdentityMismatch"))

    def test_launch_services_timeout_marks_recorder_may_continue(self):
        with tempfile.TemporaryDirectory() as root:
            out = Path(root)
            class TimedOut:
                pid = 5678
                returncode = -15
                calls = 0
                def communicate(self, timeout):
                    self.calls += 1
                    if self.calls == 1:
                        raise subprocess.TimeoutExpired("open", timeout)
                    return ("", None)
            with patch.object(motion.subprocess, "Popen", return_value=TimedOut()), \
                 patch.object(motion.os, "killpg") as killpg:
                launch = motion.launch_services_wait(out / "app.app", 43542,
                    out / "resolved-plan.json", out, 1)
            killpg.assert_called_once_with(5678, motion.signal.SIGTERM)
            self.assertTrue(launch["timed_out"])
            self.assertTrue(launch["recorder_may_continue"])

    def test_identity_bound_refusal_preserves_codes_without_completion_or_exit(self):
        # The real af83 passive refusal has these two denied flags and an
        # available callback, but writes no result.jsonl. open -W returns zero.
        with tempfile.TemporaryDirectory() as root:
            out = Path(root)
            recorder = out / "recorder"
            identity = {"identifier": "dev.nudox.audit.motion-recorder"}
            plan = {"foreground_required": False}
            row = {"schema": 1, "state": "Rejected", "pid": 43542, "frontmost_pid": 62184,
                   "recorder_executable": str(recorder),
                   "recorder_bundle_identifier": identity["identifier"], "foreground_required": False,
                   "screen_recording_preflight_granted": False, "ax_trusted": False,
                   "stream_output_callback_ready": True,
                   "failures": ["ScreenRecordingPreflightDenied", "AccessibilityTrustDenied"]}
            (out / "preflight.jsonl").write_text(json.dumps(row) + "\n")
            launch = {"timed_out": False, "launcher_exit_code": 0,
                      "recorder_exit": None, "recorder_may_continue": True}
            for result in (None, {"captured_frames": 1, "dropped_frames": 0}):
                with self.subTest(result=result):
                    if result is not None:
                        (out / "result.jsonl").write_text(json.dumps(result) + "\n")
                    admitted, observed, failures, classification = motion.launch_services_completion(
                        out, launch, recorder, identity, 43542, plan)
                    self.assertFalse(admitted)
                    self.assertEqual(observed, row)
                    self.assertEqual(classification["state"], "Rejected")
                    self.assertTrue(classification["identity_bound"])
                    self.assertEqual(classification["native_failure_codes"], row["failures"])
                    manifest = {"launcher": dict(launch, completion_failures=failures),
                                "preflight_classification": classification}
                    message = motion.recorder_failure_message(manifest, out)
                    for code in row["failures"]:
                        self.assertIn(code, message)
                    if result is None:
                        self.assertIn("completion unproven", message)
                        self.assertFalse((out / "result.jsonl").exists())
                    self.assertIsNone(launch["recorder_exit"])
                    self.assertTrue(launch["recorder_may_continue"])
                    self.assertNotIn("analysis", manifest)

    def test_preflight_protocol_separates_absence_invalid_identity_and_refusal(self):
        recorder = "/owned/recorder"
        row = {"schema": 1, "state": "Rejected", "pid": 43542, "frontmost_pid": 62184,
               "recorder_executable": recorder, "recorder_bundle_identifier": "owned.recorder",
               "foreground_required": False, "screen_recording_preflight_granted": False,
               "ax_trusted": False, "stream_output_callback_ready": True,
               "failures": ["ScreenRecordingPreflightDenied", "AccessibilityTrustDenied"]}
        encoded = (json.dumps(row) + "\n").encode()
        cases = [(None, "Absent"), (b"", "Invalid"), (encoded[:-1], "Invalid"),
                 (b'{"schema":1,\n', "Invalid"), (encoded + encoded, "Invalid"),
                 (b"x" * (motion.supervision.MAX_MESSAGE + 1), "Invalid"),
                 (b'[]\n', "Invalid"), (b'{"schema":1,"schema":1}\n', "Invalid"),
                 (b'{"schema":NaN}\n', "Invalid"), (b'\xff\n', "Invalid")]
        for changes, state in [({"schema": True}, "Invalid"), ({"schema": 1.0}, "Invalid"),
                ({"pid": True}, "IdentityMismatch"), ({"pid": 43543}, "IdentityMismatch"),
                ({"recorder_executable": "/other/recorder"}, "IdentityMismatch"),
                ({"recorder_bundle_identifier": "other.recorder"}, "IdentityMismatch"),
                ({"foreground_required": 0}, "IdentityMismatch"),
                ({"frontmost_pid": True}, "Invalid"), ({"ax_trusted": 0}, "Invalid"),
                ({"stream_output_callback_ready": 1}, "Invalid"), ({"failures": "denied"}, "Invalid"),
                ({"failures": [False]}, "Invalid"), ({"failures": [[]]}, "Invalid"),
                ({"failures": ["x" * 129]}, "Invalid"),
                ({"failures": row["failures"] * 2}, "Invalid"), ({"failures": []}, "Invalid"),
                ({"state": "Admitted"}, "Invalid")]:
            cases.append(((json.dumps(dict(row, **changes)) + "\n").encode(), state))
        for raw, state in cases:
            with self.subTest(raw=raw):
                result = motion.supervision.classify_preflight(raw, recorder=recorder,
                    identifier="owned.recorder", pid=43542, foreground_required=False)
                self.assertEqual(result["state"], state)
                self.assertFalse(result["identity_bound"])
                self.assertEqual(result["native_failure_codes"], [])
        result = motion.supervision.classify_preflight(encoded, recorder=recorder,
            identifier="owned.recorder", pid=43542, foreground_required=False)
        self.assertEqual(result["state"], "Rejected")
        self.assertTrue(result["identity_bound"])
        self.assertEqual(result["native_failure_codes"], row["failures"])

    def test_completion_sidecars_require_complete_typed_rows_without_promoting_process_exit(self):
        with tempfile.TemporaryDirectory() as root:
            out = Path(root)
            recorder = out / "recorder"
            identity = {"identifier": "owned.recorder"}
            plan = {"foreground_required": False}
            row = {"schema": 1, "state": "Admitted", "pid": 43542, "frontmost_pid": 1,
                   "recorder_executable": str(recorder), "recorder_bundle_identifier": identity["identifier"],
                   "foreground_required": False, "screen_recording_preflight_granted": True,
                   "ax_trusted": True, "stream_output_callback_ready": True, "failures": []}
            (out / "preflight.jsonl").write_text(json.dumps(row) + "\n")
            launch = {"timed_out": False, "launcher_exit_code": 0, "recorder_exit": None,
                      "recorder_may_continue": True}
            valid = {"captured_frames": 1, "dropped_frames": 0, "stream_failure": None}
            cases = [(None, False), (b"", False), (b'{}\n', False), (b'[]\n', False),
                     ((json.dumps(valid)).encode(), False),
                     ((json.dumps(dict(valid, captured_frames=True)) + "\n").encode(), False),
                     ((json.dumps(dict(valid, dropped_frames=-1)) + "\n").encode(), False),
                     ((json.dumps(dict(valid, stream_failure="stopped early")) + "\n").encode(), False),
                     ((json.dumps(valid) + "\n").encode(), True)]
            for raw, expected in cases:
                with self.subTest(raw=raw):
                    if raw is not None:
                        (out / "result.jsonl").write_bytes(raw)
                    admitted, observed, failures, classification = motion.launch_services_completion(
                        out, launch, recorder, identity, 43542, plan)
                    self.assertEqual(admitted, expected)
                    self.assertEqual(observed, row)
                    self.assertEqual(classification["state"], "Admitted")
                    self.assertEqual(bool(failures), not expected)
                    self.assertIsNone(launch["recorder_exit"])
                    self.assertTrue(launch["recorder_may_continue"])
            (out / "preflight.jsonl").write_bytes(b'{"schema":1,\n')
            admitted, _, failures, classification = motion.launch_services_completion(
                out, launch, recorder, identity, 43542, plan)
            self.assertFalse(admitted)
            self.assertEqual(classification["state"], "Invalid")
            (out / "preflight.jsonl").unlink()
            admitted, _, failures, classification = motion.launch_services_completion(
                out, launch, recorder, identity, 43542, plan)
            self.assertFalse(admitted)
            self.assertEqual(classification["state"], "Absent")

    def test_native_sidecar_reader_rejects_links_and_nonregular_files(self):
        with tempfile.TemporaryDirectory() as root:
            out = Path(root)
            record = out / "record"
            record.write_bytes(b"{}\n")
            link = out / "symlink"
            link.symlink_to(record)
            self.assertIsNotNone(motion.native_sidecar_bytes(link)[1])
            hard = out / "hardlink"
            motion.os.link(record, hard)
            self.assertIsNotNone(motion.native_sidecar_bytes(hard)[1])
            fifo = out / "fifo"
            motion.os.mkfifo(fifo)
            self.assertIsNotNone(motion.native_sidecar_bytes(fifo)[1])

    def test_run19_plans_keep_find_drawer_settle_and_retarget_distinct(self):
        names = ["settings-fast-open-close", "ask-interrupted-reopen", "find-fast-open-close",
                 "settings-15s-settle", "settings-resize-retarget", "shelf-hide-reveal"]
        cases = [motion.require_plan(HERE / "plans" / f"{name}.json")["case"] for name in names]
        for name in ["drawer-500-100-survey", "drawer-360-200-survey"]:
            survey = motion.require_plan(HERE / "plans" / f"{name}.json")
            self.assertEqual(survey["case"]["flow"], "drawer")
        catalog = json.loads((HERE / "native_motion_matrix.json").read_text())
        by_id = {row["id"]: row for row in catalog["required"]}
        self.assertEqual(len(by_id), len(catalog["required"]), "independent matrix rows need unique IDs")
        for case in cases:
            self.assertEqual(by_id[case["id"]], case)
        self.assertEqual(cases[2]["flow"], "find")
        find_actions = motion.require_plan(HERE / "plans/find-fast-open-close.json")["actions"]
        self.assertEqual((find_actions[2]["keycode"], find_actions[2]["modifiers"]),
                         (33, ["command"]), "native Shell Back is ⌘[ (Carbon kVK_ANSI_LeftBracket)")
        self.assertEqual(cases[3]["transition"], "settle")
        self.assertEqual(cases[1]["transition"], "retarget")
        self.assertEqual((cases[5]["flow"], cases[5]["transition"]), ("shelf", "hide_reveal"))
        self.assertEqual({by_id["drawer_500_100_full"]["flow"], by_id["drawer_360_200_full"]["flow"]}, {"drawer"})
        self.assertEqual({by_id["drawer_500_100_full"]["text_scale"],
                          by_id["drawer_360_200_full"]["text_scale"]}, {"100", "200"})
        self.assertEqual({by_id["drawer_500_100_retirement"]["transition"],
                          by_id["drawer_360_200_retirement"]["transition"]}, {"underlay_retirement"})

    def test_native_down_up_requires_a_balanced_ordered_gesture(self):
        with tempfile.TemporaryDirectory() as root:
            directory = Path(root)
            base = json.loads((HERE / "plans/ask-open-close.json").read_text())
            down = {"at_ms": 100, "kind": "mouse_down", "x": 120.0, "y": 180.0, "label": "press backdrop"}
            move = {"at_ms": 150, "kind": "move", "x": 30.0, "y": 180.0, "label": "drag into drawer"}
            up = {"at_ms": 200, "kind": "mouse_up", "x": 30.0, "y": 180.0, "label": "release inside drawer"}
            base["actions"] = [down, move, up]
            self.assertEqual(len(motion.require_plan(self.plan(directory, **base))["actions"]), 3)
            for actions, failure in [([up], "without mouse_down"), ([down], "leaves mouse button down"),
                                     ([down, down, up], "nested mouse_down"),
                                     ([down, {"at_ms": 150, "kind": "click", "x": 50, "y": 180,
                                              "label": "ambiguous click"}, up], "only scoped held-input")]:
                with self.assertRaisesRegex(ValueError, failure):
                    motion.require_plan(self.plan(directory, **dict(base, actions=actions)))

    def test_native_unselected_oracle_cannot_pass_by_absence_or_ambiguity(self):
        self.assertTrue(motion.ax_uniquely_unselected([{"title": "Full", "selected": False}], "Full"))
        self.assertFalse(motion.ax_uniquely_unselected([], "Full"))
        self.assertFalse(motion.ax_uniquely_unselected([{"title": "Full"}], "Full"))
        self.assertFalse(motion.ax_uniquely_unselected([{"title": "Full", "selected": True}], "Full"))
        self.assertFalse(motion.ax_uniquely_unselected(
            [{"title": "Full", "selected": False}, {"description": "Full", "selected": False}], "Full"))

    def test_native_probe_admission_requires_explicit_unselected_radio(self):
        with tempfile.TemporaryDirectory() as root:
            out = Path(root)
            (out / "frames").mkdir()
            frames = []
            for index, at in enumerate([0, 40, 80, 120]):
                file = f"frames/frame-{index:06d}.png"
                Image.new("RGB", (12, 12), "black").save(out / file)
                host = 1_000_000_000 + at * 1_000_000
                frames.append({"index": index, "time_ms": at, "pts_seconds": 1 + at / 1000,
                               "host_capture_ns": host, "width_px": 12, "height_px": 12,
                               "file": file, "ax": {"sample_start_host_ns": host - 2_000_000,
                                                     "sample_end_host_ns": host - 1_000_000}})
            plan = {"duration_ms": 120, "max_frame_gap_ms": 50, "actions": [
                {"kind": "probe", "label": "retired radio", "at_ms": 40,
                 "expect_ax_unselected_title": "Full"}]}
            def assessed(nodes):
                event = {"phase": "action", "kind": "probe", "label": "retired radio", "actual_ms": 40,
                         "dispatch_host_ns": 1_040_000_000, "posted_host_ns": 1_040_500_000,
                         "posted": {"disposition": "ReadOnlyProbe", "ax": {"tree": nodes,
                             "sample_start_host_ns": 1_040_100_000,
                             "sample_end_host_ns": 1_040_200_000}}}
                return motion.analyze_frames(out, frames, [event], plan)
            self.assertEqual(assessed([{"title": "Full", "selected": False}])["failures"], [])
            for nodes in [[], [{"title": "Full", "selected": True}],
                          [{"title": "Full", "selected": False}, {"title": "Full", "selected": False}]]:
                result = assessed(nodes)
                self.assertIn("did not uniquely leave 'Full' unselected", "; ".join(result["failures"]))

    def test_drawer_gesture_uses_unique_measured_native_bounds(self):
        with tempfile.TemporaryDirectory() as root:
            directory = Path(root)
            window = {"x": 100, "y": 100, "width": 500, "height": 900}
            survey = {"case": {"id": "drawer_500_100_survey"}, "passed_native_checks": True,
                "capture_inputs_stable": True, "binary_source_admission": {"state": "VerifiedBuildReceipt"},
                "window": {"capture_scope": "app_display", "window_id": 123,
                           "window_frame_pt": window,
                           "display_frame_pt": {"x": 0, "y": 0, "width": 1440, "height": 1200},
                           "requested_width_px": 2880, "requested_height_px": 2400},
                "actions": [
                    {"phase": "initial", "ax": {"tree": [
                        {"title": "Full", "selected": False,
                         "bounds_pt": {"x": 510, "y": 500, "width": 40, "height": 24}},
                        {"title": "100%", "selected": True}]}},
                    {"phase": "action", "label": "measure native Library shelf", "posted": {"ax": {
                        "window": {"bounds_pt": window}, "tree": [
                            {"title": "Library shelf", "bounds_pt": {"x": 100, "y": 140, "width": 400, "height": 800}}]}}}]}
            path = directory / "CAPTURE.json"
            path.write_text(json.dumps(survey))
            generated, derivation = drawer_plan.derive(path, 500, 100)
            self.assertEqual([generated["actions"][2]["x"], generated["actions"][2]["y"]], [530, 512])
            self.assertEqual(generated["actions"][4]["x"], 108)
            self.assertEqual(derivation["down_strategy"], "native Full radio center")
            self.assertEqual(generated["crops"][0]["rect_px"], [200, 200, 1000, 1800])
            self.assertEqual(motion.require_plan(self.plan(directory, **generated))["case"]["flow"], "drawer")
            retirement, retirement_evidence = drawer_plan.derive_retirement(path, 500, 100)
            self.assertEqual(retirement["case"]["transition"], "underlay_retirement")
            self.assertEqual(retirement_evidence["held_radio_point_pt"], [530, 512])
            self.assertEqual([action["kind"] for action in retirement["actions"] if action["kind"] != "probe"],
                             ["mouse_down", "key", "key", "mouse_up"])
            self.assertEqual(motion.require_plan(self.plan(directory, **retirement))["case"],
                             next(row for row in json.loads((HERE / "native_motion_matrix.json").read_text())["required"]
                                  if row["id"] == "drawer_500_100_retirement"))
            with self.assertRaisesRegex(ValueError, "only scoped held-input"):
                wrong_case = dict(retirement["case"], transition="open_close")
                motion.require_plan(self.plan(directory, **dict(retirement, case=wrong_case)))
            with self.assertRaisesRegex(ValueError, "held keyboard cover only admits"):
                wrong_actions = deepcopy(retirement["actions"])
                wrong_actions[2]["keycode"] = 40
                motion.require_plan(self.plan(directory, **dict(retirement, actions=wrong_actions)))
            with self.assertRaisesRegex(ValueError, "requires held"):
                wrong_actions = deepcopy(retirement["actions"])
                wrong_actions.pop(4)
                motion.require_plan(self.plan(directory, **dict(retirement, actions=wrong_actions)))
            with self.assertRaisesRegex(ValueError, "only scoped held-input"):
                wrong_actions = deepcopy(retirement["actions"])
                wrong_actions.insert(3, {"at_ms": 400, "kind": "resize", "width": 500,
                                         "height": 900, "label": "unsafe resize while held"})
                motion.require_plan(self.plan(directory, **dict(retirement, actions=wrong_actions)))
            with self.assertRaisesRegex(ValueError, "expected_window_frame_pt"):
                motion.require_plan(self.plan(directory, **dict(generated, expected_window_frame_pt=[0, 0, -1, 900])))
            survey["actions"][1]["posted"]["ax"]["tree"].append(survey["actions"][1]["posted"]["ax"]["tree"][0])
            path.write_text(json.dumps(survey))
            with self.assertRaisesRegex(ValueError, "matched 2 nodes"):
                drawer_plan.derive(path, 500, 100)

    def test_run19_typed_receipt_requires_provenance_canary_and_separate_copy_proof(self):
        with tempfile.TemporaryDirectory() as root:
            directory = Path(root).resolve()
            target = directory / "target"
            built = target / "debug/backend-desktop"
            built.parent.mkdir(parents=True)
            built.write_bytes(b"exact compiled bytes")
            binary = directory / "Run19.app/Contents/MacOS/Nudox"
            binary.parent.mkdir(parents=True)
            shutil.copy2(built, binary)
            info = directory / "Run19.app/Contents/Info.plist"
            info.write_bytes(plistlib.dumps({"CFBundleIdentifier": "dev.nudox.audit.run19",
                                             "CFBundleExecutable": "Nudox"}))
            bundle = motion.bundle_identity(binary)
            env = directory / "development.sh"
            env.write_text("export RUSTC=rustc\n")
            command = directory / "command.sh"
            command.write_text(f"source {env}\nexec cargo build --locked\n")
            log = directory / "raw.log"
            log.write_text("compiled\n")
            source = {"path": str(directory / "frozen"), "head": "frozen-head", "tree": "frozen-tree",
                      "status_porcelain": "", "cargo_lock_sha256": "frozen-lock"}
            provenance = {"schema": 2, "run_id": "run-19", "workspace_root": str(directory / "worktree"),
                "git_head": source["head"], "cargo_lock_sha256": source["cargo_lock_sha256"],
                "cargo_lock_sha256_after": source["cargo_lock_sha256"], "cargo_exit_status": 0,
                "source_changed_during_build": False, "cargo_target_dir": str(target),
                "outputs": [{"path": "debug/backend-desktop", "sha256": motion.sha256(built)}],
                "toolchain": {"capture_complete": True, "changed_during_build": False,
                              "rustc": "rustc 1.97.1 (test)"}}
            provenance_path = directory / "provenance.json"
            provenance_path.write_text(json.dumps(provenance))
            receipt = {"kind": "desktop-bins-build", "head": source["head"], "tree": source["tree"],
                "lock_sha256": source["cargo_lock_sha256"], "worktree": provenance["workspace_root"],
                "frozen_source_path": source["path"], "frozen_source_head": source["head"],
                "frozen_source_tree": source["tree"], "frozen_source_lock_sha256": source["cargo_lock_sha256"],
                "frozen_source_clean": True, "source_changed_during_build": False, "tracked_status_after": "",
                "exit_status": 0, "census_complete": True, "max_conservative_local_occupancy_sampled": 3,
                "reserved_remote_builds": 1, "command_path": str(command), "command_sha256": motion.sha256(command),
                "raw_log_path": str(log), "raw_log_sha256": motion.sha256(log),
                "development_environment_sha256": motion.sha256(env),
                "provenance_path": str(provenance_path), "provenance_sha256": motion.sha256(provenance_path),
                "provenance_run_id": "run-19", "binaries": {str(built): motion.sha256(built)},
                "qa_binary_path": str(binary), "qa_binary_sha256": motion.sha256(binary),
                "qa_binary_regular_file": True, "qa_bundle_path": bundle["path"],
                "qa_info_plist_sha256": bundle["info_sha256"], "qa_bundle_identifier": bundle["identifier"]}
            receipt_path = directory / "build-receipt.json"
            receipt_path.write_text(json.dumps(receipt))
            canary = {"schema": "root-candidate-verification.v1", "compiler_receipt_path": str(receipt_path),
                "compiler_receipt_sha256": motion.sha256(receipt_path), "head": source["head"],
                "tree": source["tree"], "qa_binary": str(binary), "sha256": motion.sha256(binary),
                "checks": {"copy_sha": True, "provenance_sha": True}}
            (directory / "root-canary-receipt.json").write_text(json.dumps(canary))
            unproven = motion.compiler_admission(receipt_path, binary, source)
            self.assertEqual(unproven["state"], "UnprovenBinarySource")
            self.assertEqual(unproven["reasons"], ["Run19 separate hash-bound QA preservation proof is absent or differs"])
            proof = {"schema": 2, "kind": "run19-qa-preservation",
                "compiler_receipt_sha256": motion.sha256(receipt_path),
                "provenance_sha256": motion.sha256(provenance_path),
                "compiled_artifact_path": str(built), "compiled_artifact_sha256": motion.sha256(built),
                "capture_source_path": source["path"], "source_head": source["head"],
                "source_tree": source["tree"], "cargo_lock_sha256": source["cargo_lock_sha256"],
                "capture_artifact_path": str(binary), "capture_artifact_sha256": motion.sha256(binary),
                "capture_bundle_info_path": bundle["info_path"],
                "capture_bundle_info_sha256": bundle["info_sha256"],
                "capture_bundle_identifier": bundle["identifier"]}
            proof_path = directory / "preservation.json"
            proof_path.write_text(json.dumps(proof))
            admitted = motion.compiler_admission(receipt_path, binary, source, proof_path)
            self.assertEqual(admitted["state"], "VerifiedBuildReceipt", admitted["reasons"])
            proof["source_head"] = "different"
            proof_path.write_text(json.dumps(proof))
            self.assertIn("preservation proof", "; ".join(
                motion.compiler_admission(receipt_path, binary, source, proof_path)["reasons"]))
            proof["source_head"] = source["head"]
            proof_path.write_text(json.dumps(proof))
            canary["checks"]["copy_sha"] = False
            (directory / "root-canary-receipt.json").write_text(json.dumps(canary))
            self.assertIn("root canary", "; ".join(
                motion.compiler_admission(receipt_path, binary, source, proof_path)["reasons"]))

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
                host = 1_000_000_000 + at * 1_000_000
                frames.append({"index": index, "time_ms": at, "pts_seconds": 1 + at / 1000,
                               "host_capture_ns": host, "width_px": 20, "height_px": 20,
                               "file": path, "ax": {"focused": {"title": "Ask anything, or find a package"},
                                   "window": {"bounds_pt": {"x": 0, "y": 0, "width": 20, "height": 20}},
                                   "sample_start_host_ns": host - 10_000_000,
                                   "sample_end_host_ns": host - 5_000_000}})
            plan = {"duration_ms": 440, "max_frame_gap_ms": 50,
                    "actions": [{"kind": "key", "label": "open Ask", "at_ms": 80,
                                 "expect_visual_ms": 200, "min_visual_fraction": .02,
                                 "expect_focus_title": "Ask anything, or find a package"}]}
            actions = [{"phase": "action", "kind": "key", "label": "open Ask", "actual_ms": 80,
                        "dispatch_host_ns": 1_080_000_000, "posted_host_ns": 1_080_100_000,
                        "before": {"focused": {"title": "Ask anything, or find a package"},
                                   "sample_start_host_ns": 1_070_000_000, "sample_end_host_ns": 1_075_000_000},
                        "posted": {"keycode": 40, "disposition": "Posted"}}]
            result = motion.analyze_frames(out, frames, actions, plan)
            self.assertTrue(any("no native pixel change" in finding for finding in result["failures"]), result)
            self.assertIsNone(result["actions"][0]["first_visual_change_ms"])
            self.assertEqual(result["actions"][0]["first_expected_focus_ms"], 80)
            self.assertTrue(result["actions"][0]["expected_focus_already_present"])
            self.assertEqual(result["actions"][0]["visual_coverage"], "covered")
            self.assertEqual(len(result["window_geometry_changes"]), 1)
            self.assertEqual(motion.changed_fraction(out / frames[0]["file"], out / frames[-1]["file"]), .25)

    def test_later_unrelated_input_cannot_supply_earlier_visual_response(self):
        with tempfile.TemporaryDirectory() as root:
            out = Path(root)
            (out / "frames").mkdir()
            frames = []
            for index, at in enumerate(range(0, 401, 40)):
                path = f"frames/frame-{index:06d}.png"
                Image.new("RGB", (20, 20), "white" if at >= 200 else "black").save(out / path)
                host = 2_000_000_000 + at * 1_000_000
                frames.append({"index": index, "time_ms": at, "pts_seconds": 2 + at / 1000,
                               "host_capture_ns": host, "width_px": 20, "height_px": 20,
                               "file": path, "ax": {"sample_start_host_ns": host - 10_000_000,
                                                    "sample_end_host_ns": host - 5_000_000}})
            plan = {"duration_ms": 400, "max_frame_gap_ms": 50,
                    "actions": [{"kind": "key", "label": "first", "at_ms": 80, "expect_visual_ms": 280},
                                {"kind": "key", "label": "second", "at_ms": 160, "expect_visual_ms": 200}]}
            actions = [{"phase": "action", "kind": "key", "label": label, "actual_ms": at,
                        "dispatch_host_ns": 2_000_000_000 + at * 1_000_000,
                        "posted_host_ns": 2_000_100_000 + at * 1_000_000,
                        "posted": {"disposition": "Posted"}}
                       for label, at in [("first", 80), ("second", 160)]]
            result = motion.analyze_frames(out, frames, actions, plan)
            self.assertIsNone(result["actions"][0]["first_visual_change_ms"])
            self.assertEqual(result["actions"][0]["response_interval_end_ms"], 160)
            self.assertEqual(result["actions"][1]["first_visual_change_ms"], 200)
            self.assertIn("first: no native pixel change", "; ".join(result["failures"]))

    def test_stale_ax_and_out_of_scope_input_never_satisfy_an_action(self):
        with tempfile.TemporaryDirectory() as root:
            out = Path(root)
            (out / "frames").mkdir()
            frames = []
            for index, at in enumerate([0, 40, 80, 120, 160]):
                path = f"frames/frame-{index:06d}.png"
                Image.new("RGB", (12, 12), "black").save(out / path)
                frames.append({"index": index, "time_ms": at, "pts_seconds": 3 + at / 1000,
                               "host_capture_ns": 3_000_000_000 + at * 1_000_000,
                               "width_px": 12, "height_px": 12, "file": path,
                               "ax": {"focused": {"title": "Expected control"},
                                      "sample_start_host_ns": 3_000_000_000,
                                      "sample_end_host_ns": 3_001_000_000}})
            plan = {"duration_ms": 160, "max_frame_gap_ms": 50,
                    "actions": [{"kind": "click", "label": "outside", "at_ms": 40,
                                 "expect_visual_ms": 100, "expect_focus_title": "Expected control"}]}
            actions = [{"phase": "action", "kind": "click", "label": "outside", "actual_ms": 40,
                        "dispatch_host_ns": 3_040_000_000, "posted_host_ns": 3_041_000_000,
                        "before": {}, "posted": {"disposition": "ReadOnlyOutOfScope",
                                                 "reason": "outside selected window"}}]
            result = motion.analyze_frames(out, frames, actions, plan)
            self.assertIsNone(result["actions"][0]["first_expected_focus_ms"])
            self.assertIsNone(result["actions"][0]["first_visual_change_ms"])
            self.assertIn("read-only out-of-scope", "; ".join(result["failures"]))
            self.assertNotIn("no native pixel change", "; ".join(result["failures"]))

    def test_missing_clock_or_screen_frames_are_coverage_not_gui_jank(self):
        frames = [{"time_ms": 0, "pts_seconds": 4, "host_capture_ns": 4_000_000_000},
                  {"time_ms": 40, "pts_seconds": 4.04, "host_capture_ns": 4_040_000_000}]
        self.assertTrue(motion.clock_alignment(frames, [])["covered"])
        shifted = [frames[0], dict(frames[1], host_capture_ns=4_240_000_000)]
        self.assertFalse(motion.clock_alignment(shifted, [])["covered"])
        self.assertFalse(motion.clock_alignment([dict(frames[0], host_capture_ns=None), frames[1]], [])["covered"])
        with tempfile.TemporaryDirectory() as root:
            out = Path(root)
            (out / "frames").mkdir()
            sparse = []
            for index, at in enumerate([0, 40, 300]):
                path = f"frames/frame-{index:06d}.png"
                Image.new("RGB", (12, 12), "black").save(out / path)
                sparse.append({"index": index, "time_ms": at, "pts_seconds": 4 + at / 1000,
                               "host_capture_ns": 4_000_000_000 + at * 1_000_000,
                               "width_px": 12, "height_px": 12, "file": path, "ax": {}})
            plan = {"duration_ms": 300, "max_frame_gap_ms": 50,
                    "actions": [{"kind": "key", "label": "unseen", "at_ms": 40, "expect_visual_ms": 200}]}
            actions = [{"phase": "action", "kind": "key", "label": "unseen", "actual_ms": 40,
                        "dispatch_host_ns": 4_040_000_000, "posted_host_ns": 4_041_000_000,
                        "posted": {"disposition": "Posted"}}]
            result = motion.analyze_frames(out, sparse, actions, plan)
            self.assertEqual(result["actions"][0]["visual_coverage"], "uncovered")
            self.assertIn("SC frame coverage gap", "; ".join(result["coverage_findings"]))
            self.assertNotIn("no native pixel change", "; ".join(result["failures"]))

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
            source = {"path": str(directory), "head": "current-head", "tree": "current-tree", "status_porcelain": "",
                      "cargo_lock_sha256": "current-lock"}
            result = motion.compiler_admission(receipt, binary, source)
            self.assertEqual(result["state"], "UnprovenBinarySource")
            self.assertIn("receipt HEAD/tree differs", "; ".join(result["reasons"]))
            self.assertIn("compiler run did not finish", "; ".join(result["reasons"]))

    def test_candidate_source_identity_is_separate_from_the_recorder_checkout(self):
        with tempfile.TemporaryDirectory() as root:
            candidate = Path(root) / "candidate"
            candidate.mkdir()
            subprocess.run(["git", "init", "-q", str(candidate)], check=True)
            (candidate / "Cargo.lock").write_text("frozen lock\n")
            subprocess.run(["git", "-C", str(candidate), "add", "Cargo.lock"], check=True)
            subprocess.run(["git", "-C", str(candidate), "-c", "user.name=Native test",
                            "-c", "user.email=native@example.invalid", "commit", "-qm", "freeze"], check=True)
            source = motion.source_identity(candidate)
            self.assertEqual(source["path"], str(candidate.resolve()))
            self.assertEqual(source["status_porcelain"], "")
            self.assertNotEqual(source["path"], str(motion.ROOT))
            (candidate / "subdirectory").mkdir()
            with self.assertRaisesRegex(ValueError, "exact Git checkout root"):
                motion.source_identity(candidate / "subdirectory")

    def test_completed_receipt_admits_only_its_distinct_candidate_binary(self):
        with tempfile.TemporaryDirectory() as root:
            directory = Path(root)
            candidate = directory / "candidate"
            candidate.mkdir()
            binary = directory / "backend-desktop"
            binary.write_bytes(b"one exact compiler output")
            environment = directory / "development.sh"
            environment.write_text("export TEST_BUILD_ENV=1\n")
            command = f"source {environment}\nexec cargo build --locked\n"
            (directory / "command.sh").write_text(command)
            (directory / "raw.log").write_text("build finished\n")
            source = {"path": str(candidate), "head": "candidate-head", "tree": "candidate-tree",
                      "status_porcelain": "", "cargo_lock_sha256": "candidate-lock"}
            receipt = {
                "head": source["head"], "tree": source["tree"],
                "lock_sha256": source["cargo_lock_sha256"], "exit_status": 0,
                "frozen_preserved": True, "capacity_abort": False,
                "census_valid": True, "maximum_local_cargo": 1, "local_cargo_cap": 3,
                "rustc_version": "rustc 1.97.1 (test)",
                "build_cwd": str(candidate),
                "build_argv": ["/bin/bash", "-c", command],
                "command_sha256": motion.hashlib.sha256(command.encode()).hexdigest(),
                "raw_log_sha256": motion.sha256(directory / "raw.log"),
                "development_environment_sha256": motion.sha256(environment),
                "binaries": {str(binary): motion.sha256(binary)},
            }
            path = directory / "receipt.json"
            path.write_text(json.dumps(receipt))
            admitted = motion.compiler_admission(path, binary, source)
            self.assertEqual(admitted["state"], "VerifiedBuildReceipt", admitted["reasons"])
            wrong_source = dict(source, head="tool-head")
            result = motion.compiler_admission(path, binary, wrong_source)
            self.assertEqual(result["state"], "UnprovenBinarySource")
            self.assertIn("HEAD/tree differs", "; ".join(result["reasons"]))
            wrong_checkout = dict(source, path=str(motion.ROOT))
            self.assertIn("requires a hash-bound preservation receipt",
                          "; ".join(motion.compiler_admission(path, binary, wrong_checkout)["reasons"]))
            altered = deepcopy(receipt)
            altered["binaries"] = {str(binary): "0" * 64}
            path.write_text(json.dumps(altered))
            self.assertIn("binary SHA is not",
                          "; ".join(motion.compiler_admission(path, binary, source)["reasons"]))

    def test_preserved_copy_needs_exact_source_and_artifact_mapping(self):
        with tempfile.TemporaryDirectory() as root:
            directory = Path(root)
            built = directory / "build" / "backend-desktop"
            built.parent.mkdir()
            built.write_bytes(b"compiled bytes")
            copied = directory / "preserved" / "backend-desktop"
            copied.parent.mkdir()
            shutil.copy2(built, copied)
            environment = directory / "development.sh"
            environment.write_text("export TEST_BUILD_ENV=1\n")
            command = f"source {environment}\nexec cargo build --locked\n"
            (directory / "command.sh").write_text(command)
            (directory / "raw.log").write_text("build finished\n")
            source = {"path": str(directory / "frozen-source"), "head": "same-commit",
                      "tree": "same-tree", "status_porcelain": "", "cargo_lock_sha256": "same-lock"}
            receipt = {
                "head": source["head"], "tree": source["tree"],
                "lock_sha256": source["cargo_lock_sha256"], "exit_status": 0,
                "frozen_preserved": True, "capacity_abort": False,
                "census_valid": True, "maximum_local_cargo": 1, "local_cargo_cap": 3,
                "rustc_version": "rustc 1.97.1 (test)",
                "build_cwd": str(directory / "original-build-root"),
                "build_argv": ["/bin/bash", "-c", command],
                "command_sha256": motion.hashlib.sha256(command.encode()).hexdigest(),
                "raw_log_sha256": motion.sha256(directory / "raw.log"),
                "development_environment_sha256": motion.sha256(environment),
                "binaries": {str(built): motion.sha256(built)},
            }
            receipt_path = directory / "receipt.json"
            receipt_path.write_text(json.dumps(receipt))
            self.assertEqual(motion.compiler_admission(receipt_path, copied, source)["state"], "UnprovenBinarySource")
            preservation = {
                "schema": 1, "compiler_receipt_sha256": motion.sha256(receipt_path),
                "build_cwd": receipt["build_cwd"],
                "compiled_artifact_path": str(built), "compiled_artifact_sha256": motion.sha256(built),
                "capture_artifact_path": str(copied.resolve()), "capture_artifact_sha256": motion.sha256(copied),
                "capture_source_path": source["path"], "source_head": source["head"],
                "source_tree": source["tree"], "cargo_lock_sha256": source["cargo_lock_sha256"],
            }
            preservation_path = directory / "preservation.json"
            preservation_path.write_text(json.dumps(preservation))
            admitted = motion.compiler_admission(receipt_path, copied, source, preservation_path)
            self.assertEqual(admitted["state"], "VerifiedBuildReceipt", admitted["reasons"])
            self.assertEqual(admitted["compiled_artifact_path"], str(built))
            self.assertEqual(admitted["capture_artifact_path"], str(copied.resolve()))
            copied.write_bytes(b"modified copy")
            self.assertEqual(motion.compiler_admission(receipt_path, copied, source, preservation_path)["state"],
                             "UnprovenBinarySource")
            shutil.copy2(built, copied)
            wrong_source = dict(source, head="different-commit")
            self.assertEqual(motion.compiler_admission(receipt_path, copied, wrong_source, preservation_path)["state"],
                             "UnprovenBinarySource")
            symlink = directory / "linked-desktop"
            symlink.symlink_to(copied)
            self.assertIn("not a symlink",
                          "; ".join(motion.compiler_admission(receipt_path, symlink, source, preservation_path)["reasons"]))
            app_binary = directory / "Nudox.app" / "Contents" / "MacOS" / "Nudox"
            app_binary.parent.mkdir(parents=True)
            shutil.copy2(built, app_binary)
            info = directory / "Nudox.app" / "Contents" / "Info.plist"
            info.write_bytes(plistlib.dumps({"CFBundleIdentifier": "dev.nudox.desktop.qa",
                                             "CFBundleExecutable": "Nudox"}))
            bundle = motion.bundle_identity(app_binary)
            preserved_app = dict(preservation, capture_artifact_path=str(app_binary.resolve()),
                                 capture_bundle_info_path=bundle["info_path"],
                                 capture_bundle_info_sha256=bundle["info_sha256"],
                                 capture_bundle_identifier=bundle["identifier"])
            preservation_path.write_text(json.dumps(preserved_app))
            admitted_app = motion.compiler_admission(receipt_path, app_binary, source, preservation_path)
            self.assertEqual(admitted_app["state"], "VerifiedBuildReceipt", admitted_app["reasons"])
            info.write_bytes(plistlib.dumps({"CFBundleIdentifier": "dev.nudox.desktop.old",
                                             "CFBundleExecutable": "Nudox"}))
            self.assertEqual(motion.compiler_admission(receipt_path, app_binary, source, preservation_path)["state"],
                             "UnprovenBinarySource")
            linked_copy = directory / "linked-copy"
            linked_copy.hardlink_to(built)
            preserved_link = dict(preservation, capture_artifact_path=str(linked_copy.resolve()))
            preservation_path.write_text(json.dumps(preserved_link))
            self.assertIn("must not be hard-linked",
                          "; ".join(motion.compiler_admission(receipt_path, linked_copy, source, preservation_path)["reasons"]))

    def test_matrix_never_promotes_native_source_pixels_to_live_owner_proof(self):
        catalog = json.loads((HERE / "native_motion_matrix.json").read_text())
        source = next(case for case in catalog["required"] if case["id"] == "source_full")
        manifest = {"case": source, "class": catalog["evidence_class"], "passed_native_checks": True,
                    "inputs": [], "owner_receipt": None}
        result = matrix.assess({"required": [source], "evidence_class": catalog["evidence_class"]}, [manifest])
        self.assertEqual(result["rows"][0]["status"], "failed")
        self.assertFalse(result["native_coverage_complete"])
        self.assertIn("independent", result["live_index_acceptance"])
        forged = dict(manifest, inputs=[{"sha256": "input"}], owner_receipt={"sha256": "owner"},
                      binary_source_admission={"state": "VerifiedBuildReceipt"},
                      capture_inputs_stable=True, analysis={"clock_alignment": {"covered": False}})
        result = matrix.assess({"required": [source], "evidence_class": catalog["evidence_class"]}, [forged])
        self.assertIn("clock alignment is uncovered", "; ".join(result["rows"][0]["problems"]))


if __name__ == "__main__":
    unittest.main()
