"""Opt-in native validation. Default pytest discovery does not collect this file."""

import subprocess
import sys
from contextlib import ExitStack
from pathlib import Path
from queue import Queue
from uuid import uuid4

import pytest
from midirp import midi


class Collector:
    def __init__(self) -> None:
        self.messages: Queue[tuple[int, bytes]] = Queue()

    def __call__(self, timestamp: int, message: bytes) -> None:
        self.messages.put((timestamp, message))


@pytest.mark.parametrize("direction", ["input", "output"])
@pytest.mark.parametrize("bits", [None, *range(8)])
def test_virtual_loopback_preserves_bytes_timestamps_and_filters(
    direction: str, bits: int | None
) -> None:
    require_unix()
    receiver = Collector()
    source = midi.MidiInput("midirp validation input")
    target = midi.MidiOutput("midirp validation output")
    if bits is not None:
        source.ignore(midi.Ignore(bits))
    token = f"midirp-{uuid4().hex}"
    with ExitStack() as stack:
        if direction == "input":
            incoming = stack.enter_context(source.create_virtual(token, receiver))
            (port,) = (p for p in target.ports() if token in target.port_name(p))
            outgoing = stack.enter_context(target.connect(port, token))
        else:
            outgoing = stack.enter_context(target.create_virtual(token))
            (port,) = (p for p in source.ports() if token in source.port_name(p))
            incoming = stack.enter_context(source.connect(port, token, receiver))
        for m in MESSAGES:
            outgoing.send(m)
        received: list[tuple[int, bytes]] = []
        while True:
            item = receiver.messages.get(timeout=5)
            received.append(item)
            if item[1] == MESSAGES[-1]:
                break
        flags = 0 if bits is None else bits
        expected = [m for m in MESSAGES if not filtered(m, flags)]
        assert [m for _, m in received] == expected
        assert all(type(m) is bytes for _, m in received)
        assert all(type(t) is int and t >= 0 for t, _ in received)
        assert [t for t, _ in received] == sorted(t for t, _ in received)
    assert incoming.closed
    assert outgoing.closed
    assert isinstance(source.ports(), list)
    assert isinstance(target.ports(), list)


def test_duplicate_names_keep_distinct_port_identity_and_lookup() -> None:
    require_unix()
    token = f"midirp-duplicate-{uuid4().hex}"
    first = midi.MidiOutput("midirp duplicate")
    second = midi.MidiOutput("midirp duplicate")
    observer = midi.MidiInput("midirp identity observer")
    with first.create_virtual(token), second.create_virtual(token):
        ports = [p for p in observer.ports() if token in observer.port_name(p)]
        assert len(ports) == 2
        assert observer.port_name(ports[0]) == observer.port_name(ports[1])
        assert ports[0] != ports[1]
        assert ports[0].id() != ports[1].id()
        for p in ports:
            assert observer.find_port_by_id(p.id()) == p
            with pytest.raises(TypeError):
                hash(p)
        assert observer.find_port_by_id(f"absent-{uuid4().hex}") is None
        old_ids = [p.id() for p in ports]
    assert all(observer.find_port_by_id(i) is None for i in old_ids)


def test_close_restores_clients_for_reopen_and_propagates_context_errors() -> None:
    require_unix()
    source = midi.MidiInput("midirp reopen input")
    target = midi.MidiOutput("midirp reopen output")
    source.ignore(midi.Ignore.TIME | midi.Ignore.ACTIVE_SENSE)
    for _ in range(2):
        token = f"midirp-reopen-{uuid4().hex}"
        receiver = Collector()
        with source.create_virtual(token, receiver) as incoming:
            (port,) = (p for p in target.ports() if token in target.port_name(p))
            assert target.find_port_by_id(port.id()) == port
            with pytest.raises(TypeError):
                hash(port)
            with pytest.raises(RuntimeError):
                source.ignore(midi.Ignore.NONE)
            with pytest.raises(ValueError, match="body error"):
                with target.connect(port, token) as outgoing:
                    outgoing.send(b"\xf8")
                    outgoing.send(b"\x90\x3c\x7f")
                    assert receiver.messages.get(timeout=5)[1] == b"\x90\x3c\x7f"
                    raise ValueError("body error")
            assert outgoing.closed
            with pytest.raises(RuntimeError):
                outgoing.send(b"\xf8")
        incoming.close()
        assert incoming.closed
        assert target.find_port_by_id(port.id()) is None


def test_native_interpreter_exit_drains_live_connections_and_queued_traffic() -> None:
    require_unix()
    result = subprocess.run(
        [sys.executable, str(Path(__file__).with_name("shutdown.py"))],
        check=True,
        capture_output=True,
        text=True,
        timeout=20,
    )
    assert result.stderr == ""


def require_unix() -> None:
    if sys.platform not in ("darwin", "linux"):
        pytest.skip("Virtual loopback requires CoreMIDI or ALSA")


def filtered(message: bytes, bits: int) -> bool:
    return bool(
        (bits & 1 and message[0] == 0xF0)
        or (bits & 2 and message[0] in (0xF1, 0xF8))
        or (bits & 4 and message[0] == 0xFE)
    )


MESSAGES = [
    b"\x90\x3c\x7f",
    b"\xb0\x01\x40",
    b"\xf8",
    b"\xf1\x00",
    b"\xfe",
    b"\xf0\x7d\x00\x01\xf7",
    b"\x90\x63\x01",
]
