"""Native interpreter-exit fixture, invoked only by the opt-in loopback test."""

import atexit
from threading import Event
from uuid import uuid4

from midirp import midi

started = Event()
release = Event()


def receive(timestamp: int, message: bytes) -> None:
    started.set()
    if not release.wait(10):
        raise RuntimeError("native shutdown test release deadline exceeded")


def open_connections() -> tuple[midi.MidiInputConnection, midi.MidiOutputConnection]:
    token = f"midirp-shutdown-{uuid4().hex}"
    source = midi.MidiInput("midirp shutdown input")
    incoming = source.create_virtual(token, receive)
    target = midi.MidiOutput("midirp shutdown output")
    (port,) = (p for p in target.ports() if token in target.port_name(p))
    outgoing = target.connect(port, token)
    outgoing.send(b"\x90\x3c\x7f")
    if not started.wait(5):
        raise RuntimeError("native callback did not start")
    for _ in range(100):
        outgoing.send(b"\x90\x3d\x01")
    # This handler runs before the extension's earlier cleanup handler.
    atexit.register(release.set)
    return incoming, outgoing


if __name__ == "__main__":
    # Keep both native connections live until CPython runs its exit handlers.
    incoming, outgoing = open_connections()
