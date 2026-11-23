#!/bin/bash
# ABOUTME: Records the phoaudio Loopback input for one Sendspin capture run, with the host time of every callback.
# ABOUTME: Runs inside the tmux session that holds microphone permission. Arguments: run folder, seconds, loopcheck path.
if [ $# -ne 3 ]; then
  echo "usage: rec-job.sh RUN-FOLDER SECONDS LOOPCHECK" >&2
  echo "loopback_capture.py sends this job into the recorder tmux session. Do not run it over SSH:" >&2
  echo "macOS gives a process without microphone permission only silence." >&2
  exit 2
fi
out="$1"; seconds="$2"; loopcheck="$3"
# The driver waits for this marker before it starts the player.
date +%s > "$out/rec-started"
"$loopcheck" run --record "$out/capture.wav" --seconds "$seconds" --times "$out/times.csv" > "$out/run.txt" 2>&1
echo "exit $?" >> "$out/run.txt"
