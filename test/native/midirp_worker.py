"""Controlled native stand-ins for subprocess protocol tests; no OS MIDI."""

from __future__ import annotations

import os
import sys
from collections.abc import Callable
from threading import Event
from unittest.mock import patch

from midirp import midi
from midirp._worker import Sender, Worker, serve


class InputPort:
    def __init__(self, identifier: str = "input") -> None:
        self.identifier = identifier

    def id(self) -> str:
        return self.identifier


class OutputPort:
    def __init__(self, identifier: str = "output") -> None:
        self.identifier = identifier

    def id(self) -> str:
        return self.identifier


class Input:
    def __init__(self, name: str) -> None:
        self.name = name
        self.flags = 0
        if name == "hang_init":
            Event().wait()

    def ports(self) -> list[InputPort]:
        if self.name == "zero_id":
            return [InputPort("0")]
        return [InputPort(), InputPort()] if self.name == "ambiguous" else [InputPort()]

    def port_name(self, port: InputPort) -> str:
        return "input port"

    def ignore(self, flags: midi.Ignore) -> None:
        self.flags = int(flags)

    def connect(
        self, port: InputPort, name: str, callback: Callable[[int, bytes], object]
    ) -> InputConnection:
        if self.name in ("early", "twice"):
            callback(17, b"\x90\x3c\x7f")
        if self.name == "twice":
            callback(18, b"\xf8")
        if self.name == "burst":
            for i in range(2048):
                callback(i, b"\xf8")
        return InputConnection(self.name)

    def create_virtual(
        self, name: str, callback: Callable[[int, bytes], object]
    ) -> InputConnection:
        return self.connect(InputPort(), name, callback)


class Output:
    def __init__(self, name: str) -> None:
        self.name = name
        if name == "hang_init":
            Event().wait()

    def ports(self) -> list[OutputPort]:
        if self.name == "zero_id":
            return [OutputPort("0")]
        return (
            [OutputPort(), OutputPort()] if self.name == "ambiguous" else [OutputPort()]
        )

    def port_name(self, port: OutputPort) -> str:
        return "output port"

    def connect(self, port: OutputPort, name: str) -> OutputConnection:
        if self.name == "failed_open":
            raise midi.ConnectError("device vanished during connect")
        return OutputConnection(self.name)

    def create_virtual(self, name: str) -> OutputConnection:
        return self.connect(OutputPort(), name)


class InputConnection:
    def __init__(self, mode: str) -> None:
        self.mode = mode
        self.closed = False

    def close(self) -> None:
        if self.mode == "hang_close":
            Event().wait()
        self.closed = True


class OutputConnection:
    def __init__(self, mode: str) -> None:
        self.mode = mode
        self.closed = False

    def send(self, message: bytes) -> None:
        if self.mode == "hang_send":
            Event().wait()
        if self.mode == "crash_send":
            os._exit(19)
        if self.mode == "native_error":
            raise midi.SendError("device disconnected")
        if self.mode == "check_bytes" and message != b"\xf0\x7d\x00\xff\xf7":
            raise midi.SendError("bytes changed in transport")

    def close(self) -> None:
        if self.mode == "hang_close":
            Event().wait()
        self.closed = True


def main() -> None:
    from midirp import _native

    with (
        patch.object(_native, "MidiInput", Input),
        patch.object(_native, "MidiOutput", Output),
        patch.object(_native, "MidiInputConnection", InputConnection),
        patch.object(_native, "MidiOutputConnection", OutputConnection),
    ):
        serve(Worker(Sender(sys.stdout.buffer)), sys.stdin.buffer)


if __name__ == "__main__":
    main()
