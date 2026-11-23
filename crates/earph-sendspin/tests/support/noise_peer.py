# ABOUTME: Drives a real pinned aiosendspin NoiseSession with synthetic, ephemeral test identities.
# ABOUTME: Exchanges bounded test records over private process pipes without sockets or audio.
import base64
import hashlib
import importlib.metadata
import json
import sys

from aiosendspin.noise.session import NoiseCipherSuite, NoiseSession
from cryptography.hazmat.primitives.asymmetric.x25519 import X25519PrivateKey


def send(value):
    print(json.dumps(value), flush=True)


def receive():
    return json.loads(sys.stdin.readline())


def b64(value):
    return base64.urlsafe_b64encode(value).rstrip(b"=").decode("ascii")


def main():
    assert importlib.metadata.version("aiosendspin") == "10.2.1"
    request = receive()
    mode = request["mode"]
    client_init = request["client_init"].encode()
    client_public = base64.urlsafe_b64decode(request["client_public"] + "=")
    server = X25519PrivateKey.generate()
    server_init = json.dumps({"type": "server/init", "payload": {
        "version": 1, "server_id": b64(server.public_key().public_bytes_raw()), "future": True,
    }}, separators=(",", ":"))
    sentinel = hashlib.sha256(b"sendspin-sentinel-psk-v1").digest()
    referenced = bytes([7]) * 32 if mode in ("lt", "pr", "fallback", "wrong_psk") else sentinel
    psk = bytes([8]) * 32 if mode == "wrong_psk" else sentinel if mode == "fallback" else referenced
    category = "pr" if mode == "pr" else "lt" if referenced != sentinel else "sn"
    prologue = client_init + server_init.encode()
    if mode == "wrong_static":
        client_public = X25519PrivateKey.generate().public_key().public_bytes_raw()
    session = NoiseSession.as_initiator(
        suite=NoiseCipherSuite.CHACHAPOLY,
        local_static_priv=server.private_bytes_raw(),
        remote_static_pub=client_public,
        prologue=prologue,
        psk=psk,
    )
    payload = json.dumps({"psk_id": b64(hashlib.sha256(b"sendspin-psk-id-v1" + referenced).digest()),
                          "psk_category": category, "future": True}).encode()
    first = session.write_message(payload)
    send({"server_init": server_init + (" " if mode == "prologue" else ""), "handshake": first.hex()})
    reply = receive()
    if reply.get("abort"):
        send({"outcome": "aborted"})
        return
    try:
        second = session.read_message(bytes.fromhex(reply["handshake"]))
    except Exception:
        if mode != "wrong_psk":
            raise
        send({"outcome": "wrong_psk_rejected"})
        return
    assert second == b"{}"
    assert session.handshake_complete
    first_record = session.encrypt(bytes([4]) * 65519 if mode == "boundary" else b"\x04python-server")
    second_record = session.encrypt(b"\x04next")
    if mode == "corrupt":
        first_record = first_record[:-1] + bytes([first_record[-1] ^ 1])
    send({"outcome": "transport", "record": first_record.hex(), "next": second_record.hex(),
          "handshake_hash": session.handshake_hash.hex()})
    request = receive()
    if request.get("abort"):
        send({"outcome": "aborted"})
        return
    plaintext = session.decrypt(bytes.fromhex(request["record"]))
    assert plaintext == (bytes([4]) * 65519 if mode == "boundary" else b"\x04rust-client")
    send({"outcome": "both_directions_verified"})


if __name__ == "__main__":
    main()
