# ABOUTME: Runs exact pinned Python handshake, pairing, and wire drivers through bounded pipes.
# ABOUTME: Uses fresh synthetic identities and memory stores without sockets or native output.
import asyncio
import hashlib
import importlib.metadata
import inspect
import json
import pathlib
import sys

from aiohttp import WSMessage, WSMsgType
from aiosendspin.noise import driver, wire, session, constants, keys, models, pairing, pairing_token, trust_store

MAX_LINE = 400_000
MAX_OUTPUT = 8_000_000
output_bytes = 0


def send(value):
    global output_bytes
    encoded = json.dumps(value, separators=(",", ":"))
    assert len(encoded) <= MAX_LINE
    output_bytes += len(encoded)
    assert output_bytes <= MAX_OUTPUT
    print(encoded, flush=True)


def provenance():
    assert importlib.metadata.version("aiosendspin") == "10.2.1"
    expected = {
        driver: "bbfb90d2b61bd0d9f1c0d85a0bbf3328a6d72556882e3718fc21621da27ae08f",
        wire: "69bbd9c212020fbe0c21a2ba9c587a95153d46ece715a7e48f979d64f9b3be4d",
        session: "ff500e846bf1b84964588a913214196af1cf5171095285bf806043029c2249f8",
        constants: "c938e27befc2848d1bc4231480e5518b25cd3ae4b033816f2aacadfefdab81f8",
        keys: "c09d110784ecea67eb1f6116250be2d2eb451387ad3f85480a7879e9d02f1ee8",
        models: "a450806cfdcbca2fdf7f1f1da9d87f8ac0eae8519cf713152c4fd85d3a0e6280",
        pairing: "d60c2bf9e92ca7c2c82fd185c6fbb3001e6f6d50322bb551bcb7a222e822e48f",
        pairing_token: "e7ba0096af99d9e6cf2d19a0ae0df7a8217e10095431fe72344470137b997686",
        trust_store: "6f5b6b1fe58dad2a58e17c28aad38644195cd7986b4d1e1be2a557097b6c3e49",
    }
    for module, digest in expected.items():
        actual = hashlib.sha256(pathlib.Path(inspect.getfile(module)).read_bytes()).hexdigest()
        assert actual == digest, "pinned peer source mismatch"
    return {module.__name__: digest for module, digest in expected.items()}


class Pipe:
    closed = False
    close_code = None

    def __init__(self, reader):
        self.reader = reader

    async def request(self):
        line = await asyncio.wait_for(self.reader.readline(), 10)
        if not line or len(line) > MAX_LINE:
            raise EOFError("bounded session fixture pipe closed")
        return json.loads(line)

    async def send_str(self, data):
        send({"wire": {"kind": "text", "data": data}})

    async def send_bytes(self, data):
        send({"wire": {"kind": "binary", "data": data.hex()}})

    async def receive(self):
        request = await self.request()
        if request.get("stop"):
            self.closed = True
            return WSMessage(WSMsgType.CLOSE, None, "")
        message = request["wire"]
        if message["kind"] == "text":
            return WSMessage(WSMsgType.TEXT, message["data"], "")
        data = bytes.fromhex(message["data"])
        assert len(data) <= 65535
        return WSMessage(WSMsgType.BINARY, data, "")

    async def close(self):
        self.closed = True
        return True

    def exception(self):
        return None


async def run():
    hashes = provenance()
    reader = asyncio.StreamReader(limit=MAX_LINE)
    protocol = asyncio.StreamReaderProtocol(reader)
    await asyncio.get_running_loop().connect_read_pipe(lambda: protocol, sys.stdin)
    pipe = Pipe(reader)
    mode = (await pipe.request())["mode"]
    identity = keys.Identity.generate()
    pairing_psk = bytes([7]) * 32

    async def provider(client_id):
        return trust_store.ResolvedPsk(psk_id=keys.psk_id_for(pairing_psk), psk=pairing_psk,
                                      category=trust_store.PskCategory.PAIRING, counterparty_id=client_id)

    result = await driver.run_handshake_server(pipe, local_identity=identity, psk_provider=provider,
                                                timeout_s=10, strict=True)
    enc = result.encrypted_ws
    await enc.send_str(json.dumps({"type": "server/hello", "payload": {"name": "Pinned fixture"}}))
    hello = await enc.receive()
    assert hello.type == WSMsgType.TEXT
    hello = json.loads(hello.data)
    assert hello["type"] == "client/hello"
    assert hello["payload"]["supported_pair_methods"] == {"pairing_psk": {}}
    assert hello["payload"]["unpaired_access"] == {"enabled": False}
    await enc.send_str(json.dumps({"type": "server/activate", "payload": {"activities": ["pairing"],
                       "active_roles": [], "pairing": {"method": "pairing_psk"}}}))
    store = trust_store.InMemoryServerPairingStore()
    record = await pairing.run_pairing_psk_server(enc, pairing_index=1, client_id=result.peer_id,
                                                  store=store, pairing_psk=pairing_psk)
    assert (await store.record_by_client_id(result.peer_id)).psk == record.psk
    send({"stage": "paired", "source_hashes": hashes, "hash": result.handshake_hash.hex()})
    assert (await pipe.request())["committed"] is True
    for rotation in range(2):
        if rotation == 1:
            send({"stage": "queue_old"})
            assert (await pipe.request())["old_started"] is True
        resolved = trust_store.ResolvedPsk(psk_id=keys.psk_id_for(record.psk), psk=record.psk,
                                            category=trust_store.PskCategory.LONG_TERM,
                                            counterparty_id=result.peer_id)
        try:
            result = await driver.run_rehandshake_server(enc, local_identity=identity, client_id=result.peer_id,
                         suite=result.suite, prologue=result.handshake_hash, psk=resolved, timeout_s=10, strict=True)
        except driver.HandshakeAbortedError:
            assert mode == "corrupt_old" and rotation == 1
            send({"stage": "corrupt_old_rejected"})
            return
        assert result.encrypted_ws is enc
        payload = {"activities": ["playback"]}
        if rotation == 0:
            payload["active_roles"] = ["player@v1", "_fixture@v1"]
        await enc.send_str(json.dumps({"type": "server/activate", "payload": payload}))
        send({"stage": "rotated", "generation": rotation + 2, "hash": result.handshake_hash.hex()})
        if rotation == 0:
            await enc.send_str(json.dumps({"type": "stream/start", "payload": {"server_transmitted": 1,
                    "player": {"codec": "pcm", "channels": 2, "sample_rate": 48000, "bit_depth": 16}}}))
            await enc.send_bytes(bytes([4]) + (123456).to_bytes(8, "big", signed=True) + (1000).to_bytes(4, "big") + bytes([42]) * 70000)
        message = await enc.receive()
        assert message.type == WSMsgType.TEXT
        assert json.loads(message.data) == {"type": "client/time", "payload": {"client_transmitted": rotation}}
    send({"stage": "done", "hash": result.handshake_hash.hex()})


if __name__ == "__main__":
    asyncio.run(asyncio.wait_for(run(), 45))
