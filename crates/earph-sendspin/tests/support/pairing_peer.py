# ABOUTME: Runs pinned Python token and pairing producers with synthetic Noise channels.
# ABOUTME: Uses private process pipes and memory stores without network or real credentials.
import asyncio
import base64
import hashlib
import importlib.metadata
import inspect
import json
import pathlib
import sys

from aiohttp import WSMessage, WSMsgType
from aiosendspin.noise import pairing, pairing_token, session, trust_store
from aiosendspin.noise.keys import b64url_encode, psk_id_for
from aiosendspin.noise.session import NoiseCipherSuite, NoiseSession
from cryptography.hazmat.primitives.asymmetric.x25519 import X25519PrivateKey


def send(value):
    print(json.dumps(value), flush=True)


def receive():
    line = sys.stdin.readline()
    if not line:
        raise EOFError("pairing fixture pipe closed")
    return json.loads(line)


def provenance():
    assert importlib.metadata.version("aiosendspin") == "10.2.1"
    expected = {
        pairing: "d60c2bf9e92ca7c2c82fd185c6fbb3001e6f6d50322bb551bcb7a222e822e48f",
        pairing_token: "e7ba0096af99d9e6cf2d19a0ae0df7a8217e10095431fe72344470137b997686",
        trust_store: "6f5b6b1fe58dad2a58e17c28aad38644195cd7986b4d1e1be2a557097b6c3e49",
        session: "ff500e846bf1b84964588a913214196af1cf5171095285bf806043029c2249f8",
    }
    hashes = {}
    for module, digest in expected.items():
        actual = hashlib.sha256(pathlib.Path(inspect.getfile(module)).read_bytes()).hexdigest()
        assert actual == digest, f"pinned source mismatch: {module.__name__}"
        hashes[module.__name__] = actual
    return {"source_hashes": hashes, "python": sys.version.split()[0]}


def tokens(request):
    token = pairing_token.PSKPairingToken(
        client_id=b64url_encode(bytes(range(32))), pairing_psk=bytes(range(224, 256))
    )
    decoded = pairing_token.decode_psk_token(request["rust_token"])
    assert decoded == token
    send({"token": pairing_token.encode_psk_token(token),
          "client_public": base64.urlsafe_b64decode(decoded.client_id + "=").hex(),
          "psk": decoded.pairing_psk.hex(), "provenance": provenance()})


def noise_pair():
    client_key, server_key = X25519PrivateKey.generate(), X25519PrivateKey.generate()
    server = NoiseSession.as_initiator(
        suite=NoiseCipherSuite.CHACHAPOLY, local_static_priv=server_key.private_bytes_raw(),
        remote_static_pub=client_key.public_key().public_bytes_raw(), prologue=b"synthetic-pairing-producer", psk=bytes([7]) * 32,
    )
    client = NoiseSession.as_responder(
        suite=NoiseCipherSuite.CHACHAPOLY, local_static_priv=client_key.private_bytes_raw(),
        remote_static_pub=server_key.public_key().public_bytes_raw(), prologue=b"synthetic-pairing-producer",
    )
    client.read_message(server.write_message(b"{}"))
    client.mix_psk(bytes([7]) * 32)
    assert server.read_message(client.write_message(b"{}")) == b"{}"
    return client, server, b64url_encode(server_key.public_key().public_bytes_raw())


async def client_reference():
    client, server, server_id = noise_pair()
    store = trust_store.InMemoryClientPairingStore(record_capacity=5)
    received, events, acks = [], [], asyncio.Queue()

    class Transport:
        session = client

        async def send_str(self, text):
            decoded = server.decrypt(client.encrypt(b"\x00" + text.encode()))
            assert decoded[0] == 0
            message = json.loads(decoded[1:])
            received.append(message)
            events.append(message["type"])
            if message["type"] == "client/pair-finalize":
                assert not await store.list_records()
                await acks.put(server.encrypt(b'\x00{"type":"server/pair-finalize","payload":{}}'))

        async def receive(self):
            events.append("server/pair-finalize")
            plain = client.decrypt(await acks.get())
            return WSMessage(WSMsgType.TEXT, plain[1:].decode(), "")

    await pairing.run_pairing_psk_client(
        Transport(), pairing_index=9, server_id=server_id, store=store,
        on_finalize=lambda: events.append("on_finalize"),
    )
    records = await store.list_records()
    assert len(records) == 1
    assert records[0].psk == base64.urlsafe_b64decode(received[1]["payload"]["long_term_psk"] + "=")
    send({"flight": received, "events": events, "stored_psk": records[0].psk.hex(), "provenance": provenance()})


async def server_consumer(request):
    client_init = request["client_init"].encode()
    client_public = base64.urlsafe_b64decode(request["client_public"] + "=")
    key = X25519PrivateKey.generate()
    public = key.public_key().public_bytes_raw()
    server_init = json.dumps({"type": "server/init", "payload": {"version": 1, "server_id": b64url_encode(public)}}, separators=(",", ":"))
    channel = NoiseSession.as_initiator(
        suite=NoiseCipherSuite.CHACHAPOLY, local_static_priv=key.private_bytes_raw(),
        remote_static_pub=client_public, prologue=client_init + server_init.encode(), psk=bytes([7]) * 32,
    )
    payload = json.dumps({"psk_id": psk_id_for(bytes([7]) * 32), "psk_category": "pr"}).encode()
    send({"server_init": server_init, "handshake": channel.write_message(payload).hex(), "provenance": provenance()})
    assert channel.read_message(bytes.fromhex(receive()["handshake"])) == b"{}"
    activation = {"type": "server/activate", "payload": {"activities": ["pairing"], "active_roles": [], "pairing": {"method": "pairing_psk"}}}
    send({"record": channel.encrypt(b"\x00" + json.dumps(activation).encode()).hex()})
    store = trust_store.InMemoryServerPairingStore()
    client_id = b64url_encode(client_public)
    seen = []

    class Transport:
        session = channel

        async def receive(self):
            request = await asyncio.to_thread(receive)
            plain = channel.decrypt(bytes.fromhex(request["record"]))
            assert plain[0] == 0
            message = json.loads(plain[1:])
            seen.append(message["type"])
            return WSMessage(WSMsgType.TEXT, plain[1:].decode(), "")

        async def send_str(self, text):
            record = await store.record_by_client_id(client_id)
            assert record is not None  # The real consumer stored before acknowledging.
            encrypted = channel.encrypt(b"\x00" + text.encode())
            send({"record": encrypted.hex(), "server_stored": True, "lost_ack": request["mode"] == "lost_ack"})

    record = await pairing.run_pairing_psk_server(
        Transport(), pairing_index=1, client_id=client_id, store=store, pairing_psk=bytes([7]) * 32,
    )
    assert seen == ["client/pair-init", "client/pair-finalize"]
    send({"stored_psk": record.psk.hex(), "seen": seen, "outcome": "stored_before_ack"})


def main():
    request = receive()
    provenance()
    if request["mode"] == "tokens":
        tokens(request)
    elif request["mode"] == "client_reference":
        asyncio.run(client_reference())
    else:
        asyncio.run(server_consumer(request))


if __name__ == "__main__":
    main()
