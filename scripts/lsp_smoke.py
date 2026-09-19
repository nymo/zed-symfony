#!/usr/bin/env python3
"""Send a minimal initialize/shutdown sequence to a stdio LSP server."""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_SERVER = ROOT / "target" / "debug" / "symfony-lsp"


def encode_message(message: dict) -> bytes:
    body = json.dumps(message, separators=(",", ":")).encode()
    return b"Content-Length: " + str(len(body)).encode() + b"\r\n\r\n" + body


def read_message(stream) -> dict:
    header = bytearray()
    while not header.endswith(b"\r\n\r\n"):
        byte = stream.read(1)
        if not byte:
            raise RuntimeError("LSP server closed stdout before sending a response")
        header.extend(byte)

    content_length = None
    for line in header.decode().split("\r\n"):
        if line.lower().startswith("content-length:"):
            content_length = int(line.split(":", 1)[1].strip())
            break
    if content_length is None:
        raise RuntimeError(f"LSP response has no Content-Length header: {header!r}")

    body = stream.read(content_length)
    if len(body) != content_length:
        raise RuntimeError("LSP server closed stdout before sending the full response")
    return json.loads(body)


def send(process: subprocess.Popen, message: dict) -> None:
    process.stdin.write(encode_message(message))
    process.stdin.flush()


def read_response(stream, request_id: int) -> dict:
    while True:
        message = read_message(stream)
        if message.get("id") == request_id:
            return message
        print(json.dumps(message, indent=2), file=sys.stderr)


def main() -> int:
    server = Path(sys.argv[1]) if len(sys.argv) > 1 else DEFAULT_SERVER
    process = subprocess.Popen(
        [str(server)],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    try:
        send(
            process,
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "processId": None,
                    "rootUri": "file:///tmp/symfony-fixture",
                    "capabilities": {},
                },
            },
        )
        initialize_response = read_response(process.stdout, 1)
        if "error" in initialize_response:
            raise RuntimeError(f"initialize failed: {initialize_response}")
        print(json.dumps(initialize_response, indent=2))

        send(process, {"jsonrpc": "2.0", "method": "initialized", "params": {}})
        send(
            process,
            {
                "jsonrpc": "2.0",
                "id": 2,
                "method": "shutdown",
            },
        )
        shutdown_response = read_response(process.stdout, 2)
        if "error" in shutdown_response:
            raise RuntimeError(f"shutdown failed: {shutdown_response}")
        print(json.dumps(shutdown_response, indent=2))

        send(process, {"jsonrpc": "2.0", "method": "exit"})
        process.stdin.close()
        process.wait(timeout=10)
    finally:
        if process.poll() is None:
            process.kill()
            process.wait()

    stderr = process.stderr.read().decode(errors="replace").strip()
    if stderr:
        print(stderr, file=sys.stderr)
    return process.returncode or 0


if __name__ == "__main__":
    raise SystemExit(main())
