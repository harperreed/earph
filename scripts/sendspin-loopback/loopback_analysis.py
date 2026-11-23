#!/usr/bin/env python3
# ABOUTME: Measures Sendspin timing from one loopback capture: start offset, missing frames, and silence at a clear.
# ABOUTME: Reads capture.wav, times.csv, and run.json of a run folder, and writes analysis.json. Standard library only.
"""Analysis of one Sendspin loopback capture.

The Python peer sends a counter pattern: frame n holds n + 1 on the left channel and the
complement of n on the right channel, both modulo 65536. Each captured pattern frame therefore
names its own number. The recorder lists the host time of every input callback, so each captured
frame also has a host time. The peer stamps frame n with a server time on the same host clock.

    offset = host time at which frame n left the device - server time of frame n

A positive offset means the frame left late.
"""
import array
import bisect
import csv
import json
import math
import pathlib
import struct
import sys

USAGE = """usage: loopback_analysis.py RUN-FOLDER

Reads capture.wav, times.csv, and run.json in RUN-FOLDER.
Writes analysis.json there and prints a summary.
Exit status: 0 analyzed, 2 usage, 3 an input is missing or unreadable, 4 the capture is invalid."""

WRAP = 65536
# A shorter run of pattern frames is noise, for example frames that a gain ramp scaled.
MINIMUM_RUN_FRAMES = 8
# A run belongs to a segment only when it left within this time of its schedule.
MATCH_WINDOW_US = 500_000


class CaptureError(Exception):
    """An input that is missing or unreadable. The message names the file and the fix."""


def read_wav(path):
    """Returns the rate and the interleaved stereo float samples of a float32 WAVE file."""
    try:
        data = path.read_bytes()
    except OSError as error:
        raise CaptureError("cannot read %s: %s. Record the run first." % (path, error))
    if data[:4] != b"RIFF" or data[8:12] != b"WAVE":
        raise CaptureError("%s is no WAVE file" % path)
    position, rate, samples = 12, None, None
    while position + 8 <= len(data):
        kind, size = data[position:position + 4], struct.unpack_from("<I", data, position + 4)[0]
        body = data[position + 8:position + 8 + size]
        if kind == b"fmt ":
            code, channels, rate, _, _, bits = struct.unpack_from("<HHIIHH", body)
            if (code, channels, bits) != (3, 2, 32):
                raise CaptureError("%s must hold stereo float32 audio, as loopcheck writes it" % path)
        elif kind == b"data":
            samples = array.array("f")
            samples.frombytes(body[:len(body) // 8 * 8])
            if sys.byteorder != "little":
                samples.byteswap()
        position += 8 + size + size % 2
    if rate is None or samples is None:
        raise CaptureError("%s lacks a format or a data chunk" % path)
    return rate, samples


def read_callbacks(path):
    """Returns (sample time, host ticks, frames) of every input callback, in order."""
    try:
        with open(path, newline="") as file:
            rows = [row for row in csv.DictReader(file) if row["direction"] == "input"]
        return [(float(row["sample_time"]), int(row["host_time"]), int(row["frames"])) for row in rows]
    except (OSError, KeyError, ValueError) as error:
        raise CaptureError("cannot read %s: %s. Pass --times to loopcheck." % (path, error))


class HostClock:
    """Maps a capture frame index to the host time at which that frame passed the device."""

    def __init__(self, callbacks, rate, numer, denom):
        self.rate = rate
        self.starts = []
        self.times_us = []
        self.gaps = self.overlaps = 0
        index = 0
        for number, (sample_time, ticks, frames) in enumerate(callbacks):
            if number:
                previous = callbacks[number - 1]
                jump = sample_time - (previous[0] + previous[2])
                self.gaps += jump > 0
                self.overlaps += jump < 0
            self.starts.append(index)
            self.times_us.append(ticks * numer / denom / 1000)
            index += frames
        self.frames = index
        # The device clock follows the host clock at the nominal rate. A residual shows a timestamp that does not.
        self.residual_max_us = 0.0
        for (sample_time, _, _), time_us in zip(callbacks, self.times_us):
            expected = self.times_us[0] + (sample_time - callbacks[0][0]) * 1e6 / rate
            self.residual_max_us = max(self.residual_max_us, abs(time_us - expected))

    def time_us(self, index):
        callback = max(0, bisect.bisect_right(self.starts, index) - 1)
        return self.times_us[callback] + (index - self.starts[callback]) * 1e6 / self.rate

    def callback_starts(self, first, end):
        """The capture indexes in [first, end) at which an input callback began, after `first` itself."""
        low = bisect.bisect_right(self.starts, first)
        high = bisect.bisect_left(self.starts, end)
        return [first] + self.starts[low:high]


def pattern_tables():
    """The float values of the left and the right channel for each counter modulo 65536."""
    signed = [value / 32768.0 for value in list(range(32768)) + list(range(-32768, 0))]
    left = [signed[(counter + 1) & 0xFFFF] for counter in range(WRAP)]
    right = [signed[~counter & 0xFFFF] for counter in range(WRAP)]
    return left, right


def counter_of(left, right):
    """The counter modulo 65536 of an exact pattern frame, or None."""
    scaled_left, scaled_right = left * 32768.0, right * 32768.0
    if scaled_left != math.floor(scaled_left) or scaled_right != math.floor(scaled_right):
        return None
    counter = (int(scaled_left) - 1) & 0xFFFF
    return counter if int(scaled_right) & 0xFFFF == ~counter & 0xFFFF else None


def find_runs(samples):
    """Finds the runs of consecutive pattern frames.

    Returns the runs as (capture index, counter modulo 65536, frames) and the count of
    nonzero frames outside every run. Counter 65535 is zero on both channels, so it counts
    as a pattern frame only between its two neighbours.
    """
    table_left, table_right = pattern_tables()
    runs = []
    unidentified = 0
    start = counter = None
    expected = 0

    def close(end):
        nonlocal start, unidentified
        frames = end - start
        # A trailing zero frame is silence, not counter 65535.
        if frames and (counter + frames - 1) & 0xFFFF == 0xFFFF:
            frames -= 1
        if frames >= MINIMUM_RUN_FRAMES:
            runs.append((start, counter, frames))
        else:
            unidentified += frames
        start = None

    lefts, rights = samples[0::2], samples[1::2]
    for index, (left, right) in enumerate(zip(lefts, rights)):
        if start is not None:
            if left == table_left[expected] and right == table_right[expected]:
                expected = (expected + 1) & 0xFFFF
                continue
            close(index)
        if left == 0.0 and right == 0.0:
            continue
        found = counter_of(left, right)
        if found is None:
            unidentified += 1
            continue
        start, counter, expected = index, found, (found + 1) & 0xFFFF
    if start is not None:
        close(len(lefts))
    return runs, unidentified


def statistics(points):
    """Mean, spread, and slope of (time in microseconds, offset in microseconds) points."""
    if not points:
        return None
    count = len(points)
    offsets = [offset for _, offset in points]
    mean = sum(offsets) / count
    spread = math.sqrt(sum((offset - mean) ** 2 for offset in offsets) / count)
    seconds = [(time - points[0][0]) / 1e6 for time, _ in points]
    mean_seconds = sum(seconds) / count
    variance = sum((second - mean_seconds) ** 2 for second in seconds)
    slope = None
    if variance > 0:
        slope = sum((second - mean_seconds) * (offset - mean)
            for second, offset in zip(seconds, offsets)) / variance
    return {"samples": count, "mean": mean, "spread": spread, "min": min(offsets), "max": max(offsets),
        "first": offsets[0], "last": offsets[-1], "seconds": seconds[-1], "slope_us_per_s": slope}


def status_facts(snapshots, dropped_before):
    """What the player's own status said about one segment, from its `earph status --json` snapshots.

    The first snapshot holds the facts of the segment's anchor. A later one can follow idle silence,
    which drops the anchor. The status counts dropped frames since the source started.
    """
    timings = [snapshot["playback"]["timing"] for snapshot in snapshots if snapshot.get("playback")]
    if not timings:
        return None
    first, last = timings[0], timings[-1]
    # A full clear before the segment left `clear_held_frames` of the old audio in the output to play on.
    facts = {name: first.get(name)
        for name in ("anchors", "first_lead_us", "first_wait_us", "start_error_us", "clear_held_frames")}
    facts.update({"last_error_us": [timing.get("last_error_us") for timing in timings],
        "max_abs_error_us": last.get("max_abs_error_us"), "underruns": last.get("underruns"),
        "dropped_frames": last["dropped_frames"] - dropped_before,
        "output_discarded_frames": last.get("output_discarded_frames"),
        # A starved output: queue frames with nothing queued, and chunks more than 20 ms late.
        "late_chunks": last.get("late_chunks"), "starved_frames": last.get("starved_frames")})
    return facts


def analyze(directory):
    """Analyzes one run folder and returns the report."""
    directory = pathlib.Path(directory)
    try:
        run = json.loads((directory / "run.json").read_text())
    except (OSError, ValueError) as error:
        raise CaptureError("cannot read %s: %s. The capture driver writes it." % (directory / "run.json", error))
    rate, samples = read_wav(directory / "capture.wav")
    callbacks = read_callbacks(directory / "times.csv")
    if rate != run["rate"]:
        raise CaptureError("capture.wav has %d Hz; the run sent %d Hz" % (rate, run["rate"]))
    if not callbacks:
        raise CaptureError("times.csv lists no input callback. The recorder captured nothing.")
    clock = HostClock(callbacks, rate, run["timebase"]["numer"], run["timebase"]["denom"])
    frames = len(samples) // 2
    runs, unidentified = find_runs(samples)
    us_per_frame = 1e6 / rate

    # Each segment sent the pattern frames [first, first + count), frame n at `due(n)` on the server clock.
    segments = []
    for index, sent in enumerate(run["segments"]):
        first = sent["first_frame"]
        segments.append({"index": index, "first": first,
            "count": sent["frames_sent"] - sent["preamble_frames"],
            "zero_us": sent["first_timestamp_us"] + sent["preamble_frames"] * us_per_frame,
            "runs": [], "sent": sent})

    def due(segment, frame):
        return segment["zero_us"] + (frame - segment["first"]) * us_per_frame

    unmatched = []
    for start, counter, length in runs:
        left_us = clock.time_us(start)
        best = None
        for segment in segments:
            estimate = segment["first"] + (left_us - segment["zero_us"]) / us_per_frame
            frame = counter + WRAP * round((estimate - counter) / WRAP)
            if frame < segment["first"] or frame + length > segment["first"] + segment["count"]:
                continue
            offset = left_us - due(segment, frame)
            if abs(offset) <= MATCH_WINDOW_US and (best is None or abs(offset) < abs(best[2])):
                best = (segment, frame, offset)
        if best is None:
            unmatched.append({"capture_index": start, "counter": counter, "frames": length})
        else:
            best[0]["runs"].append((best[1], start, length))

    report_segments = []
    dropped = 0
    for segment in segments:
        played = sorted(segment["runs"])
        first, end = segment["first"], segment["first"] + segment["count"]
        points = []
        missing = []
        cursor = first
        for frame, start, length in played:
            if frame > cursor:
                missing.append([cursor, frame])
            cursor = max(cursor, frame + length)
            # One offset for each input callback that the run touches.
            for index in clock.callback_starts(start, start + length):
                points.append((clock.time_us(index), clock.time_us(index) - due(segment, frame + index - start)))
        head = missing.pop(0)[1] - first if missing and missing[0][0] == first else 0
        if not played:
            head = 0
        sent = segment["sent"]
        status = status_facts(sent.get("status", []), dropped)
        if status is not None:
            dropped += status["dropped_frames"]
        report_segments.append({
            "index": segment["index"], "first_frame": first, "frames_sent": segment["count"],
            "preamble_frames": sent["preamble_frames"], "lead_us": sent.get("lead_us"),
            "stopped": sent.get("stopped", False),
            "frames_captured": sum(length for _, _, length in played),
            "first_captured_frame": played[0][0] if played else None,
            "head_missing_frames": head,
            "tail_missing_frames": end - cursor if played else segment["count"],
            "interior_missing": missing,
            "offset_us": statistics(points),
            "status": status,
            "_edges": (played[0][1], played[-1][1] + played[-1][2], played[0][0]) if played else None})

    transitions = []
    for index, sent in enumerate(run.get("transitions", [])):
        old, new = report_segments[index], report_segments[index + 1]
        entry = {"from": index, "to": index + 1, "kind": sent["kind"],
            "server_time_us": sent.get("server_time_us")}
        if old["_edges"] and new["_edges"]:
            old_end, new_start = old["_edges"][1], new["_edges"][0]
            old_end_us, new_start_us = clock.time_us(old_end), clock.time_us(new_start)
            entry.update({"silent_frames": new_start - old_end, "silence_us": new_start_us - old_end_us,
                "first_new_frame_late_us": (new["_edges"][2] - new["first_frame"]) * us_per_frame})
            if sent.get("server_time_us") is not None:
                entry["old_audio_after_clear_us"] = old_end_us - sent["server_time_us"]
                entry["clear_to_first_new_us"] = new_start_us - sent["server_time_us"]
        transitions.append(entry)
    for segment in report_segments:
        del segment["_edges"]

    return {
        "capture": {"frames": frames, "callbacks": len(callbacks), "callback_frames": clock.frames,
            "gaps": clock.gaps, "overlaps": clock.overlaps,
            "host_time_residual_max_us": clock.residual_max_us,
            "valid": clock.gaps == 0 and clock.overlaps == 0 and clock.frames >= frames},
        "unidentified_frames": unidentified,
        "unmatched_runs": unmatched,
        "segments": report_segments,
        "transitions": transitions,
    }


def summary(report):
    """The report as short lines for a person or an agent."""
    capture = report["capture"]
    lines = ["capture  %d frames, %d callbacks, gaps %d, overlaps %d, timestamp residual %.1f us, %s" % (
        capture["frames"], capture["callbacks"], capture["gaps"], capture["overlaps"],
        capture["host_time_residual_max_us"], "valid" if capture["valid"] else "INVALID")]
    lines.append("frames   %d unidentified, %d unmatched runs" % (
        report["unidentified_frames"], len(report["unmatched_runs"])))
    for segment in report["segments"]:
        offset = segment["offset_us"]
        timing = "no frame captured" if offset is None else (
            "offset mean %+.1f us, spread %.1f, min %+.1f, max %+.1f, first %+.1f, last %+.1f over %.1f s" % (
                offset["mean"], offset["spread"], offset["min"], offset["max"], offset["first"],
                offset["last"], offset["seconds"]))
        lines.append("segment %d  sent %d, captured %d, head missing %d, interior gaps %d, tail missing %d; %s" % (
            segment["index"], segment["frames_sent"], segment["frames_captured"],
            segment["head_missing_frames"], len(segment["interior_missing"]),
            segment["tail_missing_frames"], timing))
        status = segment["status"]
        if status is not None:
            lines.append("  status   first lead %s us, first wait %s us, dropped %d, start error %s us, last error %s us" % (
                status["first_lead_us"], status["first_wait_us"], status["dropped_frames"],
                status["start_error_us"], "/".join(str(error) for error in status["last_error_us"])))
            lines.append("  status   clear held %s frames; since the source started: output discarded %s frames, "
                "starved %s frames, %s late chunks" % (status["clear_held_frames"],
                status["output_discarded_frames"], status["starved_frames"], status["late_chunks"]))
    for transition in report["transitions"]:
        if "silence_us" not in transition:
            lines.append("%s %d>%d  not measurable: a segment has no captured frame" % (
                transition["kind"], transition["from"], transition["to"]))
            continue
        line = "%s %d>%d  silence %.1f ms, first new frame late by %.1f ms" % (
            transition["kind"], transition["from"], transition["to"],
            transition["silence_us"] / 1000, transition["first_new_frame_late_us"] / 1000)
        if "old_audio_after_clear_us" in transition:
            line += ", old audio ran %.1f ms after the %s, new audio left %.1f ms after it" % (
                transition["old_audio_after_clear_us"] / 1000, transition["kind"],
                transition["clear_to_first_new_us"] / 1000)
        lines.append(line)
    return "\n".join(lines)


def main(arguments):
    if len(arguments) != 1 or arguments[0].startswith("-"):
        print(USAGE, file=sys.stderr)
        return 2
    directory = pathlib.Path(arguments[0])
    try:
        report = analyze(directory)
    except CaptureError as error:
        print("loopback_analysis: %s" % error, file=sys.stderr)
        return 3
    (directory / "analysis.json").write_text(json.dumps(report, indent=1) + "\n")
    print(summary(report))
    if not report["capture"]["valid"]:
        print("loopback_analysis: the recorder lost frames, so this capture proves nothing. Record again.",
            file=sys.stderr)
        return 4
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
