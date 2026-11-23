# ABOUTME: Serves one Sendspin player connection with the pinned Python Noise driver and typed models.
# ABOUTME: Follows line commands from standard input and reports each stage on standard output.
import asyncio
import hashlib
import importlib.util
import json
import pathlib
import struct
import sys
import time

from aiohttp import web, WSMsgType
from aiosendspin.models import core, player, types
from aiosendspin.noise import driver, keys, trust_store

# Reuse the transport peer's source checks and clock replies instead of copying them.
source = pathlib.Path(__file__).resolve().parents[3] / "earph-network/tests/support/sendspin_peer.py"
spec = importlib.util.spec_from_file_location("earph_transport_peer", source)
transport_peer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(transport_peer)
report = transport_peer.report

SAMPLE_RATE = 48000
CHUNK_FRAMES = 1200
# The pinned message models that parse the player's messages and build the server's.
MODEL_HASHES = (
    (core, "7021f6aa1c8163ab3f3736007f751fd97de91d0b4679dedfa31b760b7c714a34"),
    (player, "9724496d32575b3e1967ddda2a922ba445281c8910cb7104fe8746822330cfce"),
    (types, "2cbeda391f84d46a0255874660ec744cec5c7ddf546f3037452a010a05e3f39d"),
)


def now_us():
    """The server clock. The clock replies of the transport peer use the same clock."""
    return time.monotonic_ns() // 1000


def pattern(first_frame, frames):
    """Frame n holds n + 1 on the left and the complement of n on the right, as the Rust test expects."""
    return b"".join(struct.pack("<HH", (n + 1) & 0xFFFF, ~n & 0xFFFF)
        for n in range(first_frame, first_frame + frames))


async def send_audio(enc, command):
    """Sends silence, then the pattern, in 25 ms chunks stamped on the server clock."""
    pcm = bytes(command["preamble_frames"] * 4) + pattern(command["first_frame"], command["frames"])
    first = now_us() + command["lead_us"]
    for offset in range(0, len(pcm) // 4, CHUNK_FRAMES):
        timestamp = first + offset * 1_000_000 // SAMPLE_RATE
        frame = player.pack_player_audio_frame(timestamp, pcm[offset * 4:(offset + CHUNK_FRAMES) * 4])
        player.stamp_send_ahead(frame, player.compute_send_ahead(timestamp, now_us()))
        await enc.send_bytes(bytes(frame))
        # The player's transport ends a connection that holds more than 32 undrained events.
        await asyncio.sleep(0.005)
    report({"stage": "sent", "frames": len(pcm) // 4, "first_timestamp": first})


async def follow(enc, command):
    """Runs one command of the test. Each server message comes from a pinned model."""
    name = command["command"]
    if name == "start":
        stream = player.StreamStartPlayer(codec=types.AudioCodec.PCM, sample_rate=SAMPLE_RATE,
            channels=2, bit_depth=16)
        message = core.StreamStartMessage(payload=core.StreamStartPayload(
            server_transmitted=now_us(), player=stream))
    elif name == "audio":
        return await send_audio(enc, command)
    elif name == "clear":
        # The pinned server's player role names its role in a clear and in an end.
        message = core.StreamClearMessage(payload=core.StreamClearPayload(
            server_transmitted=now_us(), roles=["player"]))
    elif name == "end":
        message = core.StreamEndMessage(payload=core.StreamEndPayload(roles=["player"]))
    else:
        raise AssertionError("unknown command")
    await enc.send_str(message.to_json())
    report({"stage": name})


async def read_player(enc):
    """Reports every message of the player until the player ends the connection."""
    while True:
        # The wrapped socket answers each clock request itself.
        message = await enc.receive()
        if message.type != WSMsgType.TEXT:
            # A cancelled player may reset the socket, as the other pinned peers allow.
            assert message.type in (WSMsgType.CLOSE, WSMsgType.CLOSING, WSMsgType.CLOSED, WSMsgType.ERROR)
            report({"stage": "closed"})
            return
        kind = json.loads(message.data)["type"]
        assert kind == "client/state", kind
        # The pinned model applies the server's own field rules.
        state = core.ClientStateMessage.from_json(message.data).payload
        declared = state.player
        report({"stage": "state", "available": state.available, "player": None if declared is None else {
            "output_delay_ms": declared.output_delay_ms,
            "required_lead_time_ms": declared.required_lead_time_ms,
            "min_buffer_ms": declared.min_buffer_ms,
            "supported_commands": None if declared.supported_commands is None
                else [command.value for command in declared.supported_commands],
            "legacy_state": declared.state,
            "ignored_commands": declared.ignored_commands}})


async def run(host, fixture):
    hashes = transport_peer.provenance_module.provenance()
    for module, expected in MODEL_HASHES:
        actual = hashlib.sha256(pathlib.Path(module.__file__).read_bytes()).hexdigest()
        assert actual == expected, "pinned peer source mismatch"
        hashes[module.__name__] = actual
    identity = keys.Identity.from_private_bytes(bytes.fromhex(fixture["server_private"]))
    psk = bytes.fromhex(fixture["psk"])
    finished = asyncio.Event()
    failure = []

    async def connection(request):
        assert request.path == "/sendspin"
        ws = web.WebSocketResponse(compress=False, max_msg_size=100000, heartbeat=None)
        await ws.prepare(request)
        try:
            async def provider(client_id):
                # A paired server authenticates on its stored long-term record at once.
                return trust_store.ResolvedPsk(psk_id=keys.psk_id_for(psk), psk=psk,
                    category=trust_store.PskCategory.LONG_TERM, counterparty_id=client_id)
            result = await driver.run_handshake_server(ws, local_identity=identity, psk_provider=provider,
                expected_client_id=fixture["client_id"], timeout_s=5, strict=True)
            assert not result.credential_mismatch and not result.noncompliance
            enc = result.encrypted_ws
            await enc.send_str(json.dumps({"type": "server/hello", "payload": {"name": "Pinned player server"}}))
            hello = core.ClientHelloMessage.from_json((await enc.receive()).data).payload
            support = hello.player_support
            report({"stage": "hello", "roles": hello.supported_roles,
                "buffer_capacity": support.buffer_capacity,
                "formats": [[f.codec.value, f.channels, f.sample_rate, f.bit_depth]
                    for f in support.supported_formats],
                "flagged": [name for name in ("legacy_support_keys_used", "unlisted_support_roles",
                    "missing_support_roles", "unpaired_access_incomplete") if getattr(hello, name)]})
            enc = transport_peer.ClockRespondingSocket(enc)
            await enc.send_str(core.ServerActivateMessage(payload=core.ServerActivatePayload(
                activities=[types.Activity.PLAYBACK], active_roles=["player@v1"])).to_json())
            reading = asyncio.create_task(read_player(enc))
            loop = asyncio.get_running_loop()
            while not reading.done():
                line = asyncio.ensure_future(loop.run_in_executor(None, sys.stdin.readline))
                await asyncio.wait({line, reading}, return_when=asyncio.FIRST_COMPLETED)
                if not line.done() or not line.result():
                    break
                await follow(enc, json.loads(line.result()))
            await reading
        except Exception as error:
            failure.append(type(error).__name__)
            raise
        finally:
            finished.set()
        return ws

    app = web.Application()
    app.router.add_get("/sendspin", connection)
    runner = web.AppRunner(app, access_log=None)
    await runner.setup()
    site = web.TCPSite(runner, host, 0)
    await site.start()
    port = site._server.sockets[0].getsockname()[1]
    report({"stage": "ready", "port": port, "server_id": identity.peer_id, "source_hashes": hashes})
    try:
        await asyncio.wait_for(finished.wait(), 60)
        assert not failure, failure
    finally:
        await runner.cleanup()


if __name__ == "__main__":
    asyncio.run(run(sys.argv[1], json.loads(sys.stdin.readline())))
