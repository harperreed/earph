#!/usr/bin/env python3
# ABOUTME: Captures Earph playing a Sendspin stream from the pinned Python peer into the silent phoaudio Loopback device.
# ABOUTME: Runs on the Mac itself: sets up a private identity, drives peer, player, and recorder, then analyzes the capture.
"""Driver of the Sendspin loopback timing capture. Standard library only, Python 3.9 or later."""
import argparse
import ctypes
import ipaddress
import json
import os
import pathlib
import queue
import re
import secrets
import shutil
import subprocess
import sys
import threading
import time

import loopback_analysis

HERE = pathlib.Path(__file__).resolve().parent
PEERS = HERE.parents[1] / "crates/earph-engine/tests/support"
# The only output this harness ever names. It is silent: its output returns on its input.
OUTPUT = "phoaudio Loopback"
RATE = 48000
SAFE_PATH = re.compile(r"^[A-Za-z0-9_./-]+$")

USAGE = """usage: loopback_capture.py --root FOLDER COMMAND [options]

Measures Sendspin timing on a Mac. Earph plays a stream from the pinned Python peer into
the silent "phoaudio Loopback" device, and loopcheck records what leaves that device.
Run every command on the Mac itself. Run them in this order:

  setup     Create the Python environment, a keychain file with a throwaway password, and a
            player identity, then pair the player with the Python peer. Run once.
  clock     Show how Python's monotonic clock relates to the host clock of the recorder.
  run NAME  Record one run into FOLDER/runs/NAME and analyze it.
              --streams N          segments in the run (default 1)
              --seconds S          length of each segment (default 10)
              --transition KIND    clear: one stream, a stream/clear before each later segment.
                                   end: a stream/end and a new stream/start.
                                   gap: one stream, each segment runs out, and the next follows
                                   after --idle-seconds with no message between (default clear)
              --preamble-frames F  silence before the pattern after a stream/start (default 4800)
              --lead-us U          head start of each segment (default: the pinned server's
                                   send-ahead for the state that the player declares)
              --idle-seconds S     with clear: let each segment run out S seconds before its
                                   clear, so the clear meets an output that plays idle silence
                                   (default 0: the clear cuts playing audio; with gap: 1)
              --status-seconds S   time between status snapshots (default 5)
  analyze NAME   Analyze FOLDER/runs/NAME again.

FOLDER is a private work folder (mode 700). The commands create nothing outside it.
Options and their environment variables:
  --earph PATH       the earph executable                          (EARPH_BIN)
  --helper PATH      earph-credential-helper, default beside earph (EARPH_CREDENTIAL_HELPER)
  --loopcheck PATH   the loopcheck recorder of phoaudio            (LOOPCHECK)
  --uv PATH          uv, for setup                                 (default: uv on PATH)
  --tmux PATH        tmux                                          (default: tmux on PATH)
  --tmux-socket NAME --tmux-session NAME   the recorder session    (default: phoaudio, rec)
  --python VERSION   Python for the peer environment, for setup    (default: 3.12)
  --address IP       the Mac's own local network address           (default: found with ifconfig)

Exit status: 0 done, 1 the run failed, 2 usage, 3 the environment is not ready, 4 the capture is invalid."""


class Failure(Exception):
    """A failure with an exit status and a message that names the fix."""

    def __init__(self, status, message):
        super().__init__(message)
        self.status = status


def monotonic_us():
    """The clock of the Python peer: its server time and its clock replies."""
    return time.monotonic_ns() // 1000


def clock_evidence(samples=2000):
    """Compares time.monotonic_ns with mach_absolute_time, the host clock of Core Audio time stamps.

    Each sample reads monotonic, absolute, monotonic. The clocks are one clock when every
    absolute reading, scaled by the timebase, lies between its two monotonic readings.
    """
    if sys.platform != "darwin":
        raise Failure(3, "the clock check needs macOS: it reads mach_absolute_time")

    class Timebase(ctypes.Structure):
        _fields_ = [("numer", ctypes.c_uint32), ("denom", ctypes.c_uint32)]

    system = ctypes.CDLL("/usr/lib/libSystem.B.dylib")
    system.mach_absolute_time.restype = ctypes.c_uint64
    system.mach_continuous_time.restype = ctypes.c_uint64
    timebase = Timebase()
    if system.mach_timebase_info(ctypes.byref(timebase)) != 0 or timebase.denom == 0:
        raise Failure(3, "mach_timebase_info failed")
    numer, denom = timebase.numer, timebase.denom
    tick_ns = -(-numer // denom)
    below = above = 0
    inside = True
    for _ in range(samples):
        before = time.monotonic_ns()
        ticks = system.mach_absolute_time()
        after = time.monotonic_ns()
        absolute = ticks * numer // denom
        # Python rounds its own scaling, so allow one tick at each side.
        inside = inside and before - tick_ns <= absolute <= after + tick_ns
        below, above = max(below, absolute - before), max(above, after - absolute)
    continuous = system.mach_continuous_time() * numer // denom
    return {"timebase": {"numer": numer, "denom": denom}, "samples": samples,
        "monotonic_is_mach_absolute_time": inside,
        "largest_absolute_minus_earlier_monotonic_ns": below,
        "largest_later_monotonic_minus_absolute_ns": above,
        # The continuous clock also counts sleep. A large difference shows that the check can tell them apart.
        "continuous_minus_monotonic_ns": continuous - time.monotonic_ns(),
        "python": sys.version.split()[0]}


def local_address():
    """The first private local network address of this machine. Earph refuses loopback and 100.64.0.0/10."""
    listing = subprocess.run(["/sbin/ifconfig"], capture_output=True, text=True, check=True).stdout
    shared = ipaddress.ip_network("100.64.0.0/10")
    for found in re.findall(r"^\s+inet (\d+\.\d+\.\d+\.\d+) ", listing, re.MULTILINE):
        address = ipaddress.ip_address(found)
        if address.is_private and not address.is_loopback and not address.is_link_local and address not in shared:
            return found
    raise Failure(3, "found no private local network address. Join a local network, or pass --address IP.")


class Setup:
    """The paths of one work folder and the tools around it."""

    def __init__(self, options):
        if not options.root:
            raise Failure(2, "--root FOLDER is needed\n" + USAGE)
        self.root = pathlib.Path(options.root).resolve()
        if not self.root.is_dir() or self.root.stat().st_mode & 0o077:
            raise Failure(3, "%s must be an existing folder with mode 700. Run: mkdir -m 700 FOLDER" % self.root)
        self.python = self.root / "py/venv/bin/python"
        self.runtime = self.root / "rt"
        self.keychain = self.root / "kc/earph-sendspin.keychain-db"
        self.password = self.root / "kc/password"
        self.record = self.root / "kc/server-record.json"
        self.options = options
        earph = options.earph or os.environ.get("EARPH_BIN")
        if not earph or not os.access(earph, os.X_OK):
            raise Failure(3, "pass --earph PATH or set EARPH_BIN to the earph executable")
        self.earph = str(pathlib.Path(earph).resolve())
        helper = options.helper or os.environ.get("EARPH_CREDENTIAL_HELPER") or str(
            pathlib.Path(self.earph).with_name("earph-credential-helper"))
        if not os.access(helper, os.X_OK):
            raise Failure(3, "no credential helper at %s. Pass --helper PATH." % helper)
        self.helper = str(pathlib.Path(helper).resolve())
        if len(os.fsencode(self.runtime)) >= 60:
            raise Failure(3, "the runtime path %s is too long for a socket. Use a shorter --root." % self.runtime)

    def tool(self, option, name):
        path = getattr(self.options, option) or shutil.which(name)
        if not path or not os.access(path, os.X_OK):
            raise Failure(3, "cannot find %s. Pass --%s PATH." % (name, option.replace("_", "-")))
        return path

    def player(self, *arguments):
        """The earph command line with the private runtime and the private credential store."""
        return [self.earph, "--runtime", str(self.runtime)] + list(arguments) + [
            "--credential-helper", self.helper, "--credential-store", str(self.keychain)]

    def control(self, *arguments):
        command = [self.earph, "--runtime", str(self.runtime)] + list(arguments)
        return subprocess.run(command, capture_output=True, text=True, timeout=20)

    def address(self):
        return self.options.address or local_address()


def checked(command, what, **options):
    """Runs a setup step and fails with its error output. The command line can hold a password, so no message shows it."""
    result = subprocess.run(command, capture_output=True, text=True, **options)
    if result.returncode != 0:
        raise Failure(3, "%s failed with status %d: %s" % (what, result.returncode, result.stderr.strip()))
    return result


class Peer:
    """One pinned Python peer: a fixture line in, one JSON stage for each line out."""

    def __init__(self, command, log_path, first_line=None, stdin=subprocess.PIPE):
        self.log_path = log_path
        self.process = subprocess.Popen(command, stdin=stdin, stdout=subprocess.PIPE,
            stderr=open(log_path, "w"), text=True)
        self.lines = queue.Queue()
        self.states = []
        threading.Thread(target=self.read, daemon=True).start()
        if first_line is not None:
            self.send_line(first_line)

    def read(self):
        for line in self.process.stdout:
            self.lines.put(line)
        self.lines.put(None)

    def send_line(self, line):
        self.process.stdin.write(line.rstrip("\n") + "\n")
        self.process.stdin.flush()

    def command(self, **command):
        self.send_line(json.dumps(command))

    def expect(self, stage, seconds=10):
        """Returns the next stage, which must be `stage`. Player state reports may come at any time."""
        deadline = time.monotonic() + seconds
        while True:
            try:
                line = self.lines.get(timeout=max(0.01, deadline - time.monotonic()))
            except queue.Empty:
                raise Failure(1, "the Python peer reported no `%s` within %d s. Inspect %s." % (
                    stage, seconds, self.log_path))
            if line is None:
                raise Failure(1, "the Python peer ended before `%s`. Inspect %s." % (stage, self.log_path))
            value = json.loads(line)
            if value["stage"] == stage:
                return value
            if value["stage"] != "state":
                raise Failure(1, "the Python peer reported `%s` before `%s`. Inspect %s." % (
                    value["stage"], stage, self.log_path))
            self.states.append(value)

    def end(self):
        if self.process.poll() is None:
            self.process.terminate()
        self.process.wait()


def setup(config):
    """Creates the peer environment, the keychain file, and the identity, then pairs with the Python peer."""
    root = config.root
    for name in ("py", "kc", "rt", "runs", "tmp"):
        (root / name).mkdir(mode=0o700, exist_ok=True)
    environment = dict(os.environ, UV_CACHE_DIR=str(root / "py/uv-cache"), TMPDIR=str(root / "tmp"))
    if not config.python.exists():
        uv = config.tool("uv", "uv")
        # Only an installed Python is used. Nothing is downloaded but the pinned packages.
        environment["UV_PYTHON_DOWNLOADS"] = "never"
        checked([uv, "venv", "--python", config.options.python, str(root / "py/venv")],
            "creating the Python environment", env=environment)
        checked([uv, "pip", "install", "--python", str(config.python), "--require-hashes", "--no-deps",
            "-r", str(HERE / "peer-requirements.txt")], "installing the pinned peer packages", env=environment)
        print("python   created %s" % (root / "py/venv"))
    if not config.keychain.exists():
        if not config.password.exists():
            descriptor = os.open(config.password, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            with os.fdopen(descriptor, "w") as file:
                file.write(secrets.token_hex(16))
        password = config.password.read_text()
        checked(["security", "create-keychain", "-p", password, str(config.keychain)], "creating the keychain file")
        # No flags means no lock on sleep and no lock timeout.
        checked(["security", "set-keychain-settings", str(config.keychain)], "configuring the keychain file")
        print("keychain created %s" % config.keychain)
    checked(["security", "unlock-keychain", "-p", config.password.read_text(), str(config.keychain)],
        "unlocking the keychain file")
    if config.record.exists():
        print("paired   already; the server record is %s" % config.record)
        return
    created = subprocess.run(config.player("sendspin", "init"), capture_output=True, text=True)
    if created.returncode != 0 and "already" not in created.stderr:
        raise Failure(3, "earph sendspin init failed: %s" % created.stderr.strip())
    address = config.address()
    # The token goes from earph straight into the peer. This process never reads it.
    token = subprocess.Popen(config.player("sendspin", "token"), stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    peer = Peer([str(config.python), str(PEERS / "sendspin_pairing_server.py"), address, str(config.record)],
        root / "kc/pairing-peer-stderr.log", stdin=token.stdout)
    try:
        token.stdout.close()
        if token.wait() != 0:
            raise Failure(3, "earph sendspin token failed. Unlock the keychain file and run setup again.")
        ready = peer.expect("ready", 20)
        paired = subprocess.run(config.player("sendspin", "pair", "--server",
            "ws://%s:%d/sendspin" % (address, ready["port"]), "--timeout", "20"), capture_output=True, text=True)
        if paired.returncode != 0:
            raise Failure(3, "earph sendspin pair failed: %s" % paired.stderr.strip())
        peer.expect("paired", 20)
        peer.expect("goodbye", 20)
        if "server key  %s" % ready["server_id"] not in paired.stdout:
            raise Failure(3, "earph paired with another server than the Python peer")
    finally:
        peer.end()
    print("paired   the server record is %s" % config.record)


def snapshot(config, started_us):
    """One `earph status --json` reading, stamped on the peer's clock."""
    result = config.control("status", "--json")
    if result.returncode != 0:
        raise Failure(1, "earph status failed: %s" % result.stderr.strip())
    state = json.loads(result.stdout)
    now_us = monotonic_us()
    return {"at_us": now_us, "since_segment_start_s": (now_us - started_us) / 1e6,
        "transport": state.get("transport"), "muted": state.get("muted"),
        "playback": state.get("sendspin_playback")}


def unmuted(config):
    """Whether the Engine answers and plays unmuted. The command unmutes once the server knows the player."""
    result = config.control("status", "--json")
    try:
        return result.returncode == 0 and json.loads(result.stdout).get("muted") is False
    except ValueError:
        return False


def server_send_ahead_us(declared):
    """The head start that aiosendspin 10.2.1 gives a buffered source of the stream's own rate.

    See `_role_send_ahead_us` in aiosendspin/server/push_stream.py: the larger of the minimum
    buffer, the required lead time, and 50 ms, plus the output delay. A resampler adds a hold-back.
    """
    lead_ms = max(declared["min_buffer_ms"], declared["required_lead_time_ms"], 50)
    return (lead_ms + declared["output_delay_ms"]) * 1000


def run(config, options):
    name = options.name
    if not SAFE_PATH.match(name) or "/" in name:
        raise Failure(2, "the run name may hold letters, digits, dot, dash, and underscore only")
    if options.streams < 1 or options.seconds < 3 or options.idle_seconds < 0:
        raise Failure(2, "--streams must be at least 1, --seconds at least 3, and --idle-seconds at least 0")
    if not config.record.exists():
        raise Failure(3, "no pairing record. Run: loopback_capture.py --root FOLDER setup")
    loopcheck = options.loopcheck or os.environ.get("LOOPCHECK")
    if not loopcheck or not os.access(loopcheck, os.X_OK):
        raise Failure(3, "pass --loopcheck PATH or set LOOPCHECK to phoaudio's loopcheck recorder")
    tmux = config.tool("tmux", "tmux")
    out = config.root / "runs" / name
    job = HERE / "rec-job.sh"
    for path in (out, job, loopcheck):
        if not SAFE_PATH.match(str(path)):
            raise Failure(3, "%s holds a character that the recorder command line cannot carry" % path)
    try:
        out.mkdir(mode=0o700)
    except FileExistsError:
        raise Failure(2, "the run folder %s exists. Choose a new name; this tool deletes nothing." % out)

    # The device must exist at the stream's format. This tool changes no device setting.
    properties = subprocess.run([loopcheck, "props"], capture_output=True, text=True)
    (out / "device-before.txt").write_text(properties.stdout + properties.stderr)
    if properties.returncode != 0 or "nominal_rate 48000\n" not in properties.stdout \
            or "output_channels 2\n" not in properties.stdout:
        raise Failure(3, "the phoaudio Loopback device is missing or not at 48 kHz stereo. Inspect %s." % (
            out / "device-before.txt"))
    session = ["-L", options.tmux_socket, "has-session", "-t", options.tmux_session]
    if subprocess.run([tmux] + session, capture_output=True).returncode != 0:
        raise Failure(3, "no tmux session `%s`. In a Terminal window on this Mac, run: tmux -L %s new -d -s %s" % (
            options.tmux_session, options.tmux_socket, options.tmux_session))
    clock = clock_evidence()
    if not clock["monotonic_is_mach_absolute_time"]:
        raise Failure(3, "time.monotonic_ns is not mach_absolute_time here, so server times and capture "
            "times lie on different clocks. Run the `clock` command and inspect its output.")
    checked(["security", "unlock-keychain", "-p", config.password.read_text(), str(config.keychain)],
        "unlocking the keychain file")
    address = config.address()

    same_stream = options.transition != "end"
    if options.transition == "gap" and options.idle_seconds < 0.5:
        # A gap up to 100 ms becomes silence and keeps the anchor. This gap must drop it.
        options.idle_seconds = 1.0
    cut = options.transition == "clear" and options.idle_seconds == 0
    gap_seconds = options.idle_seconds + 1 if same_stream else 1.5
    # The recorder runs from before the player starts until after the last audio.
    total = 8 + options.streams * (options.seconds + gap_seconds + 1.5) + 4
    limit = total + 60
    record = {"name": name, "rate": RATE, "timebase": clock["timebase"], "clock": clock,
        "options": {"streams": options.streams, "seconds": options.seconds, "transition": options.transition,
            "preamble_frames": options.preamble_frames, "status_seconds": options.status_seconds,
            "idle_seconds": options.idle_seconds},
        "output": OUTPUT, "segments": [], "transitions": []}
    peer = player = None
    try:
        subprocess.run([tmux, "-L", options.tmux_socket, "send-keys", "-t", options.tmux_session,
            "/bin/bash %s %s %d %s" % (job, out, int(total + 0.999), loopcheck), "Enter"], check=True)
        deadline = time.monotonic() + 10
        while not (out / "rec-started").exists():
            if time.monotonic() > deadline:
                raise Failure(3, "the recorder did not start within 10 s. The tmux session `%s` is busy or gone." %
                    options.tmux_session)
            time.sleep(0.05)
        time.sleep(2)

        peer = Peer([str(config.python), str(PEERS / "sendspin_player_server.py"), address, str(limit)],
            out / "peer-stderr.log", first_line=config.record.read_text())
        ready = peer.expect("ready", 20)
        record["peer_source_hashes"] = len(ready["source_hashes"])
        player = subprocess.Popen(config.player("sendspin", "--server",
            "ws://%s:%d/sendspin" % (address, ready["port"]), "--server-key", ready["server_id"],
            "--output", OUTPUT, "--volume", "0"),
            stdout=open(out / "earph-stdout.txt", "w"), stderr=open(out / "earph-stderr.txt", "w"))
        record["hello"] = peer.expect("hello", 20)
        declared = peer.expect("state", 20)
        record["declared"] = declared
        lead_us = options.lead_us if options.lead_us is not None else server_send_ahead_us(declared["player"])
        record["lead_us"] = lead_us
        deadline = time.monotonic() + 15
        while not unmuted(config):
            if time.monotonic() > deadline or player.poll() is not None:
                raise Failure(1, "the player did not unmute. Inspect %s." % (out / "earph-stderr.txt"))
            time.sleep(0.1)

        for index in range(options.streams):
            last = index == options.streams - 1
            if index == 0 or not same_stream:
                peer.command(command="start")
                peer.expect("start")
                preamble = options.preamble_frames
            elif options.transition == "clear":
                # A track change of Music Assistant: a full clear on the running stream, then new audio at once.
                peer.command(command="clear")
                if cut:
                    record["segments"][-1].update(sent_frames(peer.expect("streamed")))
                record["transitions"].append({"kind": "clear", "server_time_us": peer.expect("clear")["server_time"]})
                preamble = 0
            else:
                # New audio on a stream that ran out: the player anchors again on its running output.
                record["transitions"].append({"kind": "gap", "server_time_us": monotonic_us()})
                preamble = 0
            # A segment that a clear ends keeps sending until that clear.
            frames = int((options.seconds + (2 if cut and not last else 0)) * RATE)
            first_frame = index * 20_000_000
            peer.command(command="stream", first_frame=first_frame, frames=frames,
                preamble_frames=preamble, lead_us=lead_us)
            streaming = peer.expect("streaming")
            started_us = monotonic_us()
            segment = {"first_frame": first_frame, "preamble_frames": preamble, "lead_us": lead_us,
                "first_timestamp_us": streaming["first_timestamp"], "status": []}
            record["segments"].append(segment)
            ends = time.monotonic() + options.seconds + preamble / RATE
            pause = 1.5
            while time.monotonic() + pause < ends - 0.4:
                time.sleep(pause)
                segment["status"].append(snapshot(config, started_us))
                pause = options.status_seconds
            if cut and not last:
                time.sleep(max(0, ends - 0.4 - time.monotonic()))
                segment["status"].append(snapshot(config, started_us))
                time.sleep(max(0, ends - time.monotonic()))
                continue
            segment.update(sent_frames(peer.expect("streamed", options.seconds + 20)))
            # The last chunk left the peer one head start before its audio is due.
            time.sleep(lead_us / 1e6 + 0.5)
            segment["status"].append(snapshot(config, started_us))
            if same_stream and not last:
                time.sleep(options.idle_seconds)
                continue
            peer.command(command="end")
            ended = peer.expect("end")
            if not last:
                record["transitions"].append({"kind": "end", "server_time_us": ended["server_time"]})
                time.sleep(gap_seconds)

        record["final_status"] = snapshot(config, monotonic_us())
        stopped = config.control("stop")
        if stopped.returncode != 0:
            raise Failure(1, "earph stop failed: %s" % stopped.stderr.strip())
        peer.expect("closed", 20)
        record["player_exit_status"] = player.wait(timeout=20)
        peer.process.stdin.close()
        record["peer_exit_status"] = peer.process.wait(timeout=20)
    finally:
        if player is not None and player.poll() is None:
            config.control("quit")
            try:
                player.wait(timeout=10)
            except subprocess.TimeoutExpired:
                player.terminate()
        if peer is not None:
            peer.end()
        (out / "run.json").write_text(json.dumps(record, indent=1) + "\n")

    deadline = time.monotonic() + total + 30
    recorder = ""
    while "exit " not in recorder:
        if time.monotonic() > deadline:
            raise Failure(1, "the recorder did not finish. Inspect %s." % (out / "run.txt"))
        time.sleep(0.5)
        if (out / "run.txt").exists():
            recorder = (out / "run.txt").read_text()
    if "exit 0" not in recorder:
        raise Failure(4, "the recorder failed or lost frames. Inspect %s." % (out / "run.txt"))
    return analyze(out)


def sent_frames(streamed):
    return {"frames_sent": streamed["frames"], "stopped": streamed["stopped"]}


def analyze(out):
    status = loopback_analysis.main([str(out)])
    print("results  %s" % (out / "analysis.json"))
    return status


class Parser(argparse.ArgumentParser):
    def error(self, message):
        raise Failure(2, "%s\n%s" % (message, USAGE))


def main(arguments):
    parser = Parser(add_help=False)
    parser.add_argument("command", nargs="?")
    parser.add_argument("name", nargs="?")
    parser.add_argument("--help", "-h", action="store_true")
    for name in ("root", "earph", "helper", "loopcheck", "uv", "tmux", "address"):
        parser.add_argument("--" + name)
    parser.add_argument("--tmux-socket", default="phoaudio")
    parser.add_argument("--tmux-session", default="rec")
    parser.add_argument("--python", default="3.12")
    parser.add_argument("--streams", type=int, default=1)
    parser.add_argument("--seconds", type=float, default=10)
    parser.add_argument("--transition", choices=("clear", "end", "gap"), default="clear")
    parser.add_argument("--preamble-frames", type=int, default=4800)
    parser.add_argument("--lead-us", type=int)
    parser.add_argument("--status-seconds", type=float, default=5)
    parser.add_argument("--idle-seconds", type=float, default=0)
    try:
        options = parser.parse_args(arguments)
        if options.help or options.command is None:
            print(USAGE)
            return 0 if options.help else 2
        if options.command == "clock":
            print(json.dumps(clock_evidence(), indent=1))
            return 0
        if options.command not in ("setup", "run", "analyze"):
            raise Failure(2, "unknown command %s\n%s" % (options.command, USAGE))
        if options.command != "setup" and not options.name:
            raise Failure(2, "%s needs a run name\n%s" % (options.command, USAGE))
        if options.command == "analyze":
            if not options.root:
                raise Failure(2, "--root FOLDER is needed\n" + USAGE)
            return analyze(pathlib.Path(options.root) / "runs" / options.name)
        config = Setup(options)
        if options.command == "setup":
            setup(config)
            return 0
        return run(config, options)
    except Failure as failure:
        print("loopback_capture: %s" % failure, file=sys.stderr)
        return failure.status


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
