# ABOUTME: Tests the Sendspin loopback capture analysis against small synthetic captures.
# ABOUTME: Runs on any machine with Python 3.9 or later: python3 -m unittest test_loopback_analysis.py
import json
import pathlib
import struct
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import loopback_analysis as analysis  # noqa: E402

RATE = 48000
# The Apple Silicon host clock: 24,000,000 ticks each second, so 500 ticks each frame.
NUMER, DENOM = 125, 3
TICKS_PER_FRAME = 500
FIRST_SAMPLE_TIME = 60
FIRST_HOST_TICKS = 26_668_117_401_105
PERIOD = 512


def frame(counter):
    """The float frame that the output gives for pattern frame `counter` of the Python peer."""
    left = struct.unpack("<h", struct.pack("<H", (counter + 1) & 0xFFFF))[0]
    right = struct.unpack("<h", struct.pack("<H", ~counter & 0xFFFF))[0]
    return (left / 32768.0, right / 32768.0)


def host_us(index):
    """The host time of capture frame `index` in microseconds."""
    return (FIRST_HOST_TICKS + index * TICKS_PER_FRAME) * NUMER / DENOM / 1000


class Capture:
    """Builds one capture folder: the recording, the callback times, and the run record."""

    def __init__(self, frames):
        self.frames = [(0.0, 0.0)] * frames
        self.segments = []
        self.transitions = []
        self.skipped_callbacks = set()

    def play(self, at, first, count):
        for offset in range(count):
            self.frames[at + offset] = frame(first + offset)

    def segment(self, first_frame, sent, preamble, played_at, played_from, offset_us, stopped=False):
        """Records a sent segment whose frame `played_from` leaves at capture frame `played_at`."""
        timestamp = host_us(played_at) - offset_us - (played_from - first_frame) * 1e6 / RATE
        self.segments.append({
            "first_frame": first_frame, "preamble_frames": preamble, "frames_sent": sent + preamble,
            "first_timestamp_us": round(timestamp - preamble * 1e6 / RATE), "lead_us": 200_000,
            "stopped": stopped, "status": []})

    def write(self, directory):
        directory = pathlib.Path(directory)
        samples = [value for pair in self.frames for value in pair]
        data = struct.pack("<%df" % len(samples), *samples)
        header = b"RIFF" + struct.pack("<I", 36 + len(data)) + b"WAVEfmt " + struct.pack(
            "<IHHIIHH", 16, 3, 2, RATE, RATE * 8, 8, 32) + b"data" + struct.pack("<I", len(data))
        (directory / "capture.wav").write_bytes(header + data)
        lines = ["direction,sample_time,host_time,frames"]
        for number, start in enumerate(range(0, len(self.frames), PERIOD)):
            if number in self.skipped_callbacks:
                continue
            count = min(PERIOD, len(self.frames) - start)
            lines.append("input,%d,%d,%d" % (FIRST_SAMPLE_TIME + start,
                FIRST_HOST_TICKS + start * TICKS_PER_FRAME, count))
            # The recorder also lists output callbacks. The analysis ignores them.
            lines.append("output,%d,%d,%d" % (start, 1, count))
        (directory / "times.csv").write_text("\n".join(lines) + "\n")
        (directory / "run.json").write_text(json.dumps({
            "rate": RATE, "timebase": {"numer": NUMER, "denom": DENOM},
            "segments": self.segments, "transitions": self.transitions}))
        return directory


class LoopbackAnalysis(unittest.TestCase):
    def analyze(self, capture):
        with tempfile.TemporaryDirectory() as directory:
            return analysis.analyze(capture.write(directory))

    def test_a_clear_gives_the_offsets_the_missing_frames_and_the_silence(self):
        capture = Capture(20000)
        # The first segment crosses counter 65535, whose frame is zero on both channels.
        # The server sent 4000 frames and cleared; the last 1000 never left.
        capture.play(1000, 65000, 3000)
        capture.segment(65000, 4000, 480, played_at=1000, played_from=65000, offset_us=250, stopped=True)
        # The second segment lost its first 1200 frames. Frame 201200 left 2400 frames after the old audio.
        capture.play(6400, 201200, 3800)
        capture.segment(200000, 5000, 0, played_at=6400, played_from=201200, offset_us=-100)
        clear_us = round(host_us(4000) - 30_000)
        capture.transitions.append({"kind": "clear", "server_time_us": clear_us})
        report = self.analyze(capture)

        self.assertTrue(report["capture"]["valid"], report["capture"])
        self.assertEqual(report["capture"]["frames"], 20000)
        self.assertEqual(report["unidentified_frames"], 0)
        self.assertEqual(report["unmatched_runs"], [])
        first, second = report["segments"]
        self.assertEqual(first["frames_sent"], 4000)
        self.assertEqual(first["frames_captured"], 3000)
        self.assertEqual(first["head_missing_frames"], 0)
        self.assertEqual(first["tail_missing_frames"], 1000)
        self.assertEqual(first["interior_missing"], [])
        self.assertAlmostEqual(first["offset_us"]["mean"], 250, delta=1)
        self.assertLess(first["offset_us"]["spread"], 1)
        self.assertAlmostEqual(first["offset_us"]["min"], 250, delta=1)
        self.assertAlmostEqual(first["offset_us"]["max"], 250, delta=1)
        self.assertGreaterEqual(first["offset_us"]["samples"], 5)
        self.assertEqual(second["first_captured_frame"], 201200)
        self.assertEqual(second["head_missing_frames"], 1200)
        self.assertEqual(second["tail_missing_frames"], 0)
        self.assertEqual(second["frames_captured"], 3800)
        self.assertAlmostEqual(second["offset_us"]["mean"], -100, delta=1)

        (transition,) = report["transitions"]
        self.assertEqual(transition["kind"], "clear")
        self.assertEqual(transition["silent_frames"], 2400)
        self.assertAlmostEqual(transition["silence_us"], 50_000, delta=1)
        self.assertAlmostEqual(transition["old_audio_after_clear_us"], 30_000, delta=1)
        self.assertAlmostEqual(transition["clear_to_first_new_us"], 80_000, delta=1)
        # The first new frame was due 1200 frames earlier than the first frame that left.
        self.assertAlmostEqual(transition["first_new_frame_late_us"], 25_000, delta=1)

    def test_a_changing_offset_shows_as_a_slope(self):
        capture = Capture(60000)
        # A lost output frame moves the later audio 1 frame early: 2 frames in all over this capture.
        capture.play(1000, 0, 19000)
        capture.play(20000, 19001, 20000)
        capture.play(40000, 39002, 19000)
        capture.segment(0, 58002, 0, played_at=1000, played_from=0, offset_us=0)
        report = self.analyze(capture)
        (segment,) = report["segments"]
        self.assertEqual(segment["interior_missing"], [[19000, 19001], [39001, 39002]])
        self.assertAlmostEqual(segment["offset_us"]["first"], 0, delta=1)
        self.assertAlmostEqual(segment["offset_us"]["last"], -2e6 / RATE, delta=1)
        self.assertLess(segment["offset_us"]["slope_us_per_s"], -20)
        self.assertGreater(segment["offset_us"]["spread"], 10)

    def test_a_changed_sample_is_unidentified_and_never_a_pattern_frame(self):
        capture = Capture(8000)
        capture.play(1000, 500, 3000)
        capture.segment(500, 3000, 0, played_at=1000, played_from=500, offset_us=40)
        # A gain ramp scales the first three frames.
        for index in range(1000, 1003):
            left, right = capture.frames[index]
            capture.frames[index] = (left * 0.5, right * 0.5)
        report = self.analyze(capture)
        self.assertEqual(report["unidentified_frames"], 3)
        (segment,) = report["segments"]
        self.assertEqual(segment["head_missing_frames"], 3)
        self.assertEqual(segment["frames_captured"], 2997)

    def test_a_recorder_gap_makes_the_capture_invalid(self):
        capture = Capture(8000)
        capture.play(1000, 500, 3000)
        capture.segment(500, 3000, 0, played_at=1000, played_from=500, offset_us=40)
        capture.skipped_callbacks.add(5)
        report = self.analyze(capture)
        self.assertFalse(report["capture"]["valid"])
        self.assertEqual(report["capture"]["gaps"], 1)

    def test_audio_that_matches_no_segment_is_reported(self):
        capture = Capture(8000)
        capture.play(1000, 500, 3000)
        # The run record names a segment that was due 2 seconds later.
        capture.segment(500, 3000, 0, played_at=1000, played_from=500, offset_us=-2_000_000)
        report = self.analyze(capture)
        self.assertEqual(len(report["unmatched_runs"]), 1)
        self.assertEqual(report["unmatched_runs"][0]["frames"], 3000)
        self.assertEqual(report["segments"][0]["frames_captured"], 0)
        self.assertIsNone(report["segments"][0]["offset_us"])

    def test_the_status_snapshots_give_each_segment_its_own_facts(self):
        def snapshot(anchors, lead, wait, start_error, last_error, dropped):
            return {"at_us": 1, "playback": {"timing": {"anchors": anchors, "first_lead_us": lead,
                "first_wait_us": wait, "start_error_us": start_error, "last_error_us": last_error,
                "max_abs_error_us": 900, "dropped_frames": dropped, "underruns": 0,
                "clear_held_frames": 1300 if anchors > 1 else None, "output_discarded_frames": 0,
                "late_chunks": anchors - 1, "starved_frames": 64 * (anchors - 1)}}}
        capture = Capture(12000)
        capture.play(1000, 500, 3000)
        capture.segment(500, 3000, 0, played_at=1000, played_from=500, offset_us=40)
        capture.play(6000, 9000, 3000)
        capture.segment(8000, 4000, 0, played_at=6000, played_from=9000, offset_us=40)
        capture.transitions.append({"kind": "clear", "server_time_us": round(host_us(4100))})
        capture.segments[0]["status"] = [snapshot(1, 199_000, 60_000, 300, 310, 0), snapshot(1, 199_000, 60_000, 300, -90, 0)]
        # The status counts dropped frames since the source started. This segment dropped 1000 of them.
        # Its last snapshot came after the audio ran out, so it holds no anchor facts.
        capture.segments[1]["status"] = [snapshot(2, 150_000, 170_000, 280, 700, 1000), snapshot(2, 150_000, 170_000, None, 20, 1000)]
        first, second = self.analyze(capture)["segments"]
        self.assertEqual(first["status"], {"anchors": 1, "first_lead_us": 199_000, "first_wait_us": 60_000,
            "start_error_us": 300, "last_error_us": [310, -90], "max_abs_error_us": 900,
            "dropped_frames": 0, "underruns": 0, "clear_held_frames": None, "output_discarded_frames": 0,
            "late_chunks": 0, "starved_frames": 0})
        # Both counters run since the source started, like the dropped frames.
        self.assertEqual((second["status"]["late_chunks"], second["status"]["starved_frames"]), (1, 64))
        self.assertEqual(second["status"]["dropped_frames"], 1000)
        # The clear before the second segment left 1300 frames of the first one to play on.
        self.assertEqual(second["status"]["clear_held_frames"], 1300)
        self.assertEqual(second["status"]["first_wait_us"], 170_000)
        self.assertEqual(second["status"]["start_error_us"], 280)
        self.assertEqual(second["head_missing_frames"], 1000)
        # A segment without a snapshot has no status facts.
        capture.segments[0]["status"] = []
        self.assertIsNone(self.analyze(capture)["segments"][0]["status"])

    def test_a_missing_file_names_the_file(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(analysis.CaptureError) as failure:
                analysis.analyze(pathlib.Path(directory))
            self.assertIn("run.json", str(failure.exception))


if __name__ == "__main__":
    unittest.main()
