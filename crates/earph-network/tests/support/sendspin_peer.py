# ABOUTME: Wraps the actual pinned Python Noise and pairing drivers in aiohttp WebSockets.
# ABOUTME: Uses synthetic private-interface fixtures without discovery or audio roles.
import asyncio
import base64
import hashlib
import importlib.metadata
import importlib.util
import json
import pathlib
import stat
import socket
import sys
import time

from aiohttp import web, WSMsgType
from aiohttp import web_ws
from aiohttp._websocket import writer
from aiosendspin.noise import driver, keys, pairing, trust_store
from aiosendspin.models import core, base

# Reuse the existing exact-source verifier, rather than copying its hash catalog.
source = pathlib.Path(__file__).resolve().parents[3] / "earph-sendspin/tests/support/session_peer.py"
spec = importlib.util.spec_from_file_location("earph_session_provenance", source)
provenance_module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(provenance_module)


def report(value):
    encoded = json.dumps(value, separators=(",", ":"))
    assert len(encoded) < 32000
    print(encoded, flush=True)


class ObservedSocket(web.WebSocketResponse):
    sent_messages = 0

    async def send_str(self, data, compress=None):
        await super().send_str(data, compress=compress)
        self.sent_messages += 1
    async def send_bytes(self, data, compress=None):
        await super().send_bytes(data, compress=compress)
        self.sent_messages += 1


def marker_payload(marker):
    return {"marker": marker}


class ClockRespondingSocket:
    """Serve actual clock requests while the pinned pairing driver owns other messages."""
    def __init__(self, encrypted, hold_first_time=False):
        self.encrypted = encrypted
        self.hold_first_time = hold_first_time
        self.held_time = None
        self.suppress_times = False
        self.time_replies = 0

    def __getattr__(self, name):
        return getattr(self.encrypted, name)

    async def respond_time(self, message, received):
        t1 = core.ClientTimeMessage.from_json(message.data).payload.client_transmitted
        if self.hold_first_time and self.held_time is None:
            self.held_time = (t1, received)
            return
        if not self.suppress_times:
            payload = core.ServerTimePayload(client_transmitted=t1, server_received=received,
                server_transmitted=time.monotonic_ns() // 1000).to_dict()
            await self.encrypted.send_str(json.dumps({"type": "server/time", "payload": payload}))
            self.time_replies += 1

    async def receive(self):
        while True:
            message = await self.encrypted.receive()
            received = time.monotonic_ns() // 1000
            if message.type == WSMsgType.TEXT and json.loads(message.data)["type"] == "client/time":
                await self.respond_time(message, received)
                continue
            return message

    async def finish_clock_burst(self):
        # Finish the current real burst before isolating ordinary producer backpressure.
        while self.time_replies < 8:
            message = await self.encrypted.receive()
            received = time.monotonic_ns() // 1000
            assert message.type == WSMsgType.TEXT
            assert json.loads(message.data)["type"] == "client/time"
            await self.respond_time(message, received)


async def run(host, mode):
    hashes = provenance_module.provenance()
    assert importlib.metadata.version("aiohttp") == "3.14.5"
    for module, expected in (
        (web_ws, "b75db90128267f0a01331859c68fde05e71314c4dde2d48ba4435e39fd010f35"),
        (writer, "c29ddc35b1de62e3a594c7827ff781806b0a3e6dcc63f0fbf686f1ef20f44484"),
        (core, "7021f6aa1c8163ab3f3736007f751fd97de91d0b4679dedfa31b760b7c714a34"),
        (base, "969d48e3a876b2733184e9c5ab451180076a477fc12acd19a4b6a9da235c1697"),
    ):
        actual = hashlib.sha256(pathlib.Path(module.__file__).read_bytes()).hexdigest()
        assert actual == expected, "pinned aiohttp wheel source mismatch"
        hashes[module.__name__] = actual
    finished = asyncio.Event()
    failure = []
    identity = keys.Identity.generate()
    pairing_psk = bytes([7]) * 32

    async def connection(request):
        assert request.path == "/sendspin"
        for header in ("Origin", "Authorization", "Cookie", "Proxy-Authorization"):
            assert header not in request.headers
        if mode in ("write_stall", "write_timeout"):
            request.transport.get_extra_info("socket").setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 1024)
        ws = ObservedSocket(compress=False, max_msg_size=100000, heartbeat=None)
        await ws.prepare(request)
        try:
            async def provider(client_id):
                return trust_store.ResolvedPsk(psk_id=keys.psk_id_for(pairing_psk), psk=pairing_psk,
                    category=trust_store.PskCategory.PAIRING, counterparty_id=client_id)
            result = await driver.run_handshake_server(ws, local_identity=identity, psk_provider=provider,
                timeout_s=5, strict=True)
            enc = result.encrypted_ws
            await enc.send_str(json.dumps({"type": "server/hello", "payload": {"name": "Pinned socket fixture"}}))
            hello = json.loads((await enc.receive()).data)
            assert hello["type"] == "client/hello"
            assert hello["payload"]["supported_roles"] == []
            assert hello["payload"]["unpaired_access"] == {"enabled": False}
            if mode.startswith("clock") and mode != "clock_commit":
                await enc.send_str(json.dumps({"type": "server/activate", "payload": {"activities": [], "active_roles": []}}))
                echoes = []
                exchanges = []
                for index in range(8):
                    request_time = await enc.receive()
                    received = time.monotonic_ns() // 1000
                    request_time = core.ClientTimeMessage.from_json(request_time.data)
                    t1 = request_time.payload.client_transmitted
                    assert not echoes or t1 > echoes[-1]
                    echoes.append(t1)
                    transmitted = time.monotonic_ns() // 1000
                    payload = core.ServerTimePayload(client_transmitted=t1, server_received=received, server_transmitted=transmitted).to_dict()
                    if mode == "clock_fragmented":
                        payload["padding"] = "a" * 70000
                    if mode == "clock_invalid" and index == 0:
                        for bad_payload in (
                            {**payload, "client_transmitted": t1 + 1},
                            {**payload, "server_received": True},
                            {**payload, "server_transmitted": 1.25},
                            {**payload, "server_received": 9007199254740992},
                            {**payload, "server_transmitted": received - 1},
                        ):
                            await enc.send_str(json.dumps({"type": "server/time", "payload": bad_payload}))
                    if mode != "clock_timeout" or index != 2:
                        await enc.send_str(json.dumps({"type": "server/time", "payload": payload}))
                    if mode == "clock_invalid" and index == 0:
                        await enc.send_str(json.dumps({"type": "server/time", "payload": payload}))
                    exchanges.append([t1, received, transmitted])
                report({"stage": "clock_burst", "echoes": echoes, "exchanges": exchanges, "source_hashes": hashes})
                if mode == "clock_rotation":
                    ninth = core.ClientTimeMessage.from_json((await enc.receive()).data).payload.client_transmitted
                    assert ninth > echoes[-1]
                    resolved = trust_store.ResolvedPsk(psk_id=keys.psk_id_for(pairing_psk), psk=pairing_psk,
                        category=trust_store.PskCategory.PAIRING, counterparty_id=result.peer_id)
                    result = await driver.run_rehandshake_server(enc, local_identity=identity, client_id=result.peer_id,
                        suite=result.suite, prologue=result.handshake_hash, psk=resolved, timeout_s=5, strict=True)
                    report({"stage": "clock_quiet", "pending_t1": ninth})
                    while not (pathlib.Path(sys.argv[3]) / "reactivate-clock").exists():
                        await asyncio.sleep(0.005)
                    await enc.send_str(json.dumps({"type": "server/activate", "payload": {"activities": [], "active_roles": []}}))
                    resumed = []
                    for index in range(8):
                        request_time = await enc.receive()
                        received = time.monotonic_ns() // 1000
                        t1 = core.ClientTimeMessage.from_json(request_time.data).payload.client_transmitted
                        assert t1 > ninth and (not resumed or t1 > resumed[-1])
                        resumed.append(t1)
                        payload = core.ServerTimePayload(client_transmitted=t1, server_received=received,
                            server_transmitted=time.monotonic_ns() // 1000).to_dict()
                        await enc.send_str(json.dumps({"type": "server/time", "payload": payload}))
                    report({"stage": "clock_resumed", "echoes": resumed})
                # Keep the actual socket alive while the observer reads the mapping.
                while not (pathlib.Path(sys.argv[3]) / "release-clock").exists():
                    await asyncio.sleep(0.005)
                await ws.close()
                return ws
            enc = ClockRespondingSocket(enc, hold_first_time=mode == "clock_commit")
            activities = ["pairing"]
            await enc.send_str(json.dumps({"type": "server/activate", "payload": {"activities": activities,
                "active_roles": [], "pairing": {"method": "pairing_psk"}}}))
            if mode.startswith(("admission_", "candidate_")) and (mode.endswith("_stall") or mode.endswith("_timeout")):
                enc.suppress_times = True
                assert (await enc.receive()).type in (WSMsgType.CLOSE, WSMsgType.CLOSED, WSMsgType.ERROR)
                report({"stage": "service_cancelled"})
                return ws
            if mode == "stale_candidate":
                resolved = trust_store.ResolvedPsk(psk_id=keys.psk_id_for(pairing_psk), psk=pairing_psk,
                    category=trust_store.PskCategory.PAIRING, counterparty_id=result.peer_id)
                enc.suppress_times = True
                result = await driver.run_rehandshake_server(enc, local_identity=identity, client_id=result.peer_id,
                    suite=result.suite, prologue=result.handshake_hash, psk=resolved, timeout_s=5, strict=True)
                enc.suppress_times = False
                enc.time_replies = 0
                await enc.send_str(json.dumps({"type": "server/activate", "payload": {"activities": [], "active_roles": []}}))
                await asyncio.sleep(0.4)
                await enc.send_str(json.dumps({"type": "server/state", "payload": marker_payload(0)}))
                reply = await enc.receive()
                assert json.loads(reply.data) == {"type": "client/goodbye", "payload": {"marker": 0}}
                report({"stage": "stale_candidate_ignored"})
                return ws
            store = trust_store.InMemoryServerPairingStore()
            record = await pairing.run_pairing_psk_server(enc, pairing_index=1, client_id=result.peer_id,
                store=store, pairing_psk=pairing_psk)
            assert (await store.record_by_client_id(result.peer_id)).psk == record.psk
            report({"stage": "paired", "source_hashes": hashes})
            if mode == "clock_commit":
                assert enc.held_time is not None, "probe must precede acknowledged commit"
                t1, received = enc.held_time
                payload = core.ServerTimePayload(client_transmitted=t1, server_received=received,
                    server_transmitted=time.monotonic_ns() // 1000).to_dict()
                payload["padding"] = "h" * 70000
                await enc.send_str(json.dumps({"type": "server/time", "payload": payload}))
                report({"stage": "clock_held", "messages": ws.sent_messages, "held_t1": t1})
                while not (pathlib.Path(sys.argv[3]) / "release-held-clock").exists():
                    await asyncio.sleep(0.005)
                boundary = json.loads((pathlib.Path(sys.argv[3]) / "release-held-clock").read_text())["after_us"]
                echoes = []
                while len(echoes) < 8:
                    request_time = await enc.encrypted.receive()
                    received = time.monotonic_ns() // 1000
                    fresh = core.ClientTimeMessage.from_json(request_time.data).payload.client_transmitted
                    # A write can start between finalize flush and the authenticated commit acknowledgment.
                    # That earlier burst also belongs to the blocked interval and must not count as fresh.
                    if fresh <= boundary:
                        continue
                    assert fresh > t1 and (not echoes or fresh > echoes[-1])
                    echoes.append(fresh)
                    payload = core.ServerTimePayload(client_transmitted=fresh, server_received=received,
                        server_transmitted=time.monotonic_ns() // 1000).to_dict()
                    await enc.send_str(json.dumps({"type": "server/time", "payload": payload}))
                report({"stage": "clock_burst", "echoes": echoes})
                while not (pathlib.Path(sys.argv[3]) / "release-clock").exists():
                    await asyncio.sleep(0.005)
                await ws.close()
                return ws
            if mode in ("pressure", "commit_timeout"):
                enc.suppress_times = True
                assert (await enc.receive()).type in (WSMsgType.CLOSE, WSMsgType.CLOSED, WSMsgType.ERROR)
                report({"stage": "commit_survived_pressure"})
                return ws
            if mode in ("commit_close", "unchanged", "uncertain"):
                await ws.close()
                report({"stage": "closed_during_commit"})
                return ws
            # This message is held by the real client reducer until its acknowledged commit finishes.
            initial = marker_payload(-1)
            if mode == "arrival":
                initial["padding"] = "a" * 70000
            await enc.send_str(json.dumps({"type": "server/state", "payload": initial}))
            if mode == "arrival":
                report({"stage": "deferred_sent", "messages": ws.sent_messages})
            reply = await enc.receive()
            assert json.loads(reply.data) == {"type": "client/goodbye", "payload": {"marker": -1}}
            for rotation in range(2):
                resolved = trust_store.ResolvedPsk(psk_id=keys.psk_id_for(record.psk), psk=record.psk,
                    category=trust_store.PskCategory.LONG_TERM, counterparty_id=result.peer_id)
                enc.suppress_times = True
                result = await driver.run_rehandshake_server(enc, local_identity=identity, client_id=result.peer_id,
                    suite=result.suite, prologue=result.handshake_hash, psk=resolved, timeout_s=5, strict=True)
                if mode == "suppression" and rotation == 0:
                    await asyncio.sleep(0.2)
                activities = ["playback"] if mode == "playback" else []
                roles = ["player@v1"] if mode == "roles" else []
                enc.suppress_times = False
                enc.time_replies = 0
                await enc.send_str(json.dumps({"type": "server/activate", "payload": {"activities": activities, "active_roles": roles}}))
                if mode in ("playback", "roles"):
                    enc.suppress_times = True
                    assert (await enc.receive()).type in (WSMsgType.CLOSE, WSMsgType.CLOSED, WSMsgType.ERROR)
                    report({"stage": "denied"})
                    return ws
                await enc.send_str(json.dumps({"type": "server/state", "payload": marker_payload(rotation)}))
                reply = await enc.receive()
                assert reply.type == WSMsgType.TEXT
                assert json.loads(reply.data) == {"type": "client/goodbye", "payload": {"marker": rotation}}
                report({"stage": "rotated", "generation": rotation + 2})
            if mode in ("write_stall", "write_timeout"):
                await enc.finish_clock_burst()
                request.transport.pause_reading()
                report({"stage": "write_stall"})
                release = pathlib.Path(sys.argv[3]) / "release-transport"
                deadline = asyncio.get_running_loop().time() + 10
                while not release.exists():
                    assert asyncio.get_running_loop().time() < deadline
                    await asyncio.sleep(0.005)
                request.transport.abort()
            if mode == "corrupt":
                await ws.send_bytes(bytes([42]) * 32)
                enc.suppress_times = True
                assert (await enc.receive()).type in (WSMsgType.CLOSE, WSMsgType.CLOSED, WSMsgType.ERROR)
            report({"stage": "done"})
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
    report({"stage": "ready", "port": port, "source_hashes": hashes})
    try:
        await asyncio.wait_for(finished.wait(), 20)
        assert not failure, failure
    finally:
        await runner.cleanup()


def readback(directory):
    provenance_module.provenance()
    root = pathlib.Path(directory)
    assert stat.S_IMODE(root.stat().st_mode) == 0o700
    path = root / "committed.json"
    value = {"capacity": 5, "records": [], "superseded": []}
    if path.exists():
        assert stat.S_IMODE(path.stat().st_mode) == 0o600
        assert path.stat().st_size <= 65536
        value = json.loads(path.read_bytes())
    assert value["capacity"] == 5 and len(value["records"]) <= 5
    records = []
    for record in value["records"]:
        assert len(record["psk"]) == 32 and len(record["server"]) == 32
        identifier = keys.psk_id_for(bytes(record["psk"]))
        raw_identifier = base64.urlsafe_b64decode(identifier + "=" * (-len(identifier) % 4))
        records.append({"server": record["server"], "order": record["order"],
            "psk_id": list(raw_identifier)})
    report({"capacity": value["capacity"], "records": records, "superseded": value["superseded"]})


if __name__ == "__main__":
    if sys.argv[1] == "--readback":
        readback(sys.argv[2])
    else:
        asyncio.run(run(sys.argv[1], sys.argv[2]))
