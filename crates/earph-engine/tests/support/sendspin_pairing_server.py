# ABOUTME: Serves one Pairing PSK flow with the pinned Python Sendspin drivers over a real WebSocket.
# ABOUTME: Reads the player's pairing token from standard input, so no argument or log holds it.
import asyncio
import importlib.util
import json
import os
import pathlib
import sys

from aiohttp import web
from aiosendspin.noise import driver, keys, pairing, pairing_token, trust_store

# Reuse the transport peer's source checks and clock replies instead of copying them.
source = pathlib.Path(__file__).resolve().parents[3] / "earph-network/tests/support/sendspin_peer.py"
spec = importlib.util.spec_from_file_location("earph_transport_peer", source)
transport_peer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(transport_peer)
report = transport_peer.report


def keep(path, identity, record, client_id):
    """Writes the server side of the pairing as the fixture line that the player server reads.

    The file holds secrets. It must be new, and only its owner can read it.
    """
    fixture = {"server_private": identity.private_bytes.hex(), "psk": record.psk.hex(), "client_id": client_id}
    with os.fdopen(os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "w") as file:
        file.write(json.dumps(fixture) + "\n")


async def run(host, token, record_path=None):
    hashes = transport_peer.provenance_module.provenance()
    # The pinned decoder reads the token that the Rust command printed.
    decoded = pairing_token.decode_psk_token(token)
    identity = keys.Identity.generate()
    finished = asyncio.Event()
    failure = []

    async def connection(request):
        assert request.path == "/sendspin"
        ws = web.WebSocketResponse(compress=False, max_msg_size=100000, heartbeat=None)
        await ws.prepare(request)
        try:
            async def provider(client_id):
                assert client_id == decoded.client_id, "the client is not the token's player"
                return trust_store.ResolvedPsk(psk_id=keys.psk_id_for(decoded.pairing_psk),
                    psk=decoded.pairing_psk, category=trust_store.PskCategory.PAIRING,
                    counterparty_id=client_id)
            result = await driver.run_handshake_server(ws, local_identity=identity,
                psk_provider=provider, expected_client_id=decoded.client_id, timeout_s=5, strict=True)
            enc = result.encrypted_ws
            await enc.send_str(json.dumps({"type": "server/hello", "payload": {"name": "Pinned pairing server"}}))
            hello = json.loads((await enc.receive()).data)
            assert hello["type"] == "client/hello"
            assert hello["payload"]["supported_pair_methods"] == {"pairing_psk": {}}
            enc = transport_peer.ClockRespondingSocket(enc)
            await enc.send_str(json.dumps({"type": "server/activate", "payload": {"activities": ["pairing"],
                "active_roles": [], "pairing": {"method": "pairing_psk"}}}))
            store = trust_store.InMemoryServerPairingStore()
            record = await pairing.run_pairing_psk_server(enc, pairing_index=1, client_id=result.peer_id,
                store=store, pairing_psk=decoded.pairing_psk)
            assert (await store.record_by_client_id(result.peer_id)).psk == record.psk
            if record_path is not None:
                keep(record_path, identity, record, result.peer_id)
            report({"stage": "paired", "psk_id": keys.psk_id_for(record.psk)})
            resolved = trust_store.ResolvedPsk(psk_id=keys.psk_id_for(record.psk), psk=record.psk,
                category=trust_store.PskCategory.LONG_TERM, counterparty_id=result.peer_id)
            enc.suppress_times = True
            await driver.run_rehandshake_server(enc, local_identity=identity, client_id=result.peer_id,
                suite=result.suite, prologue=result.handshake_hash, psk=resolved, timeout_s=5, strict=True)
            enc.suppress_times = False
            await enc.send_str(json.dumps({"type": "server/activate", "payload": {"activities": [], "active_roles": []}}))
            goodbye = json.loads((await enc.receive()).data)
            assert goodbye["type"] == "client/goodbye"
            report({"stage": "goodbye", "reason": goodbye["payload"]["reason"]})
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
        await asyncio.wait_for(finished.wait(), 30)
        assert not failure, failure
    finally:
        await runner.cleanup()


if __name__ == "__main__":
    # A second argument names a new private file that keeps the server side for sendspin_player_server.py.
    asyncio.run(run(sys.argv[1], sys.stdin.readline().strip(), *sys.argv[2:3]))
