"""A separate, synthetic sidecar proves shared localhost and 0600 Unix sockets."""
import json
import os
from pathlib import Path
import socket
import sys
import threading

SOCKET = "/tmp/recall-peer.sock"
PAYLOAD = b"recall-sandbox-peer-v1"


def receive(client):
    result = b""
    while len(result) < len(PAYLOAD):
        part = client.recv(len(PAYLOAD) - len(result))
        if not part:
            break
        result += part
    return result


def serve(listener):
    listener.listen(8)
    while True:
        client, _ = listener.accept()
        try:
            with client:
                client.settimeout(3)
                if receive(client) == PAYLOAD:
                    client.sendall(PAYLOAD)
        except OSError:
            continue


def check():
    for family, address in [(socket.AF_INET, ("127.0.0.1", 17991)), (socket.AF_UNIX, SOCKET)]:
        with socket.socket(family, socket.SOCK_STREAM) as client:
            client.settimeout(3)
            client.connect(address)
            client.sendall(PAYLOAD)
            if receive(client) != PAYLOAD:
                raise RuntimeError("sidecar echo mismatch")
    if Path(SOCKET).stat().st_mode & 0o777 != 0o600:
        raise RuntimeError("sidecar socket must remain 0600")
    print(json.dumps({"cross_container_tcp": "pass", "cross_container_unix_0600": "pass"}))


if __name__ == "__main__":
    if sys.argv[1:] == ["check"]:
        check()
    elif sys.argv[1:] == ["serve"]:
        Path(SOCKET).unlink(missing_ok=True)
        tcp = socket.socket()
        tcp.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        tcp.bind(("127.0.0.1", 17991))
        unix = socket.socket(socket.AF_UNIX)
        unix.bind(SOCKET)
        os.chmod(SOCKET, 0o600)
        threading.Thread(target=serve, args=(tcp,), daemon=True).start()
        serve(unix)
    else:
        raise SystemExit("expected serve or check")
