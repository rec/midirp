"""Fresh-interpreter entry point; only this process opens isolated native MIDI."""

import pickle
import sys
from queue import Empty, Full, Queue
from threading import Event, Lock, Thread
from typing import BinaryIO, cast

# This import registers _native before Python attempts to import it.
from . import midi

# isort: split
from . import _native
from ._transport import RECEIVE_CAPACITY


class BusyError(RuntimeError):
    pass


class Sender:
    def __init__(self, stream: BinaryIO) -> None:
        self.stream = stream
        self.lock = Lock()

    def send(self, tag: str, payload: object) -> None:
        with self.lock:
            pickle.dump((tag, payload), self.stream, protocol=5)
            self.stream.flush()


class Producer:
    def __init__(self, sender: Sender, generation: int) -> None:
        self.sender = sender
        self.generation = generation
        self.messages: Queue[tuple[int, bytes]] = Queue(RECEIVE_CAPACITY)
        self.stopped = Event()
        self.dropped = 0
        self.thread = Thread(target=self.emit, daemon=True, name="midirp forwarding")
        self.thread.start()

    def __call__(self, timestamp: int, message: bytes) -> None:
        if not self.stopped.is_set():
            try:
                self.messages.put_nowait((timestamp, message))
            except Full:
                self.dropped += 1

    def emit(self) -> None:
        try:
            while not self.stopped.is_set():
                try:
                    timestamp, message = self.messages.get(timeout=0.1)
                except Empty:
                    continue
                self.sender.send(
                    "message", (self.generation, timestamp, message, self.dropped)
                )
        except (OSError, ValueError):
            self.stopped.set()

    def stop(self) -> None:
        self.stopped.set()
        self.thread.join()


class Worker:
    def __init__(self, sender: Sender) -> None:
        self.sender = sender
        self.client: _native.MidiInput | _native.MidiOutput | None = None
        self.connection: (
            _native.MidiInputConnection | _native.MidiOutputConnection | None
        ) = None
        self.producer: Producer | None = None
        self.generation = 0

    def perform(self, operation: str, args: list[object]) -> object:
        if operation == "init":
            direction, name = cast(tuple[str, str], tuple(args))
            self.client = (
                _native.MidiInput(name)
                if direction == "input"
                else _native.MidiOutput(name)
            )
            return None
        assert self.client is not None
        if operation == "close" or operation == "shutdown":
            if operation == "close" and args[0] != self.generation:
                return None
            if self.producer is not None:
                self.producer.stopped.set()
            if self.connection is not None:
                self.connection.close()
                self.connection = None
            if self.producer is not None:
                self.producer.stop()
            return None
        if operation == "send":
            if args[1] != self.generation or not isinstance(
                self.connection, _native.MidiOutputConnection
            ):
                raise BusyError("MIDI connection is closed")
            self.connection.send(cast(bytes, args[0]))
            return None
        if self.connection is not None:
            raise BusyError("MIDI client is unavailable while connected")
        if operation == "ports":
            return [p.id() for p in self.client.ports()]
        if operation == "port_name":
            if isinstance(self.client, _native.MidiInput):
                return self.client.port_name(self.input_port(cast(str, args[0])))
            return self.client.port_name(self.output_port(cast(str, args[0])))
        if operation == "ignore":
            assert isinstance(self.client, _native.MidiInput)
            self.client.ignore(_native.Ignore(cast(int, args[0])))
            return None
        if operation == "connect" or operation == "virtual":
            identifier, name, generation = cast(tuple[str, str, int], tuple(args))
            if operation == "virtual" and sys.platform == "win32":
                raise NotImplementedError(
                    "Virtual MIDI ports are not supported on this platform"
                )
            self.generation = generation
            if isinstance(self.client, _native.MidiInput):
                self.producer = Producer(self.sender, generation)
                if operation == "virtual":
                    self.connection = self.client.create_virtual(name, self.producer)
                else:
                    self.connection = self.client.connect(
                        self.input_port(identifier), name, self.producer
                    )
            elif operation == "virtual":
                self.connection = self.client.create_virtual(name)
            else:
                self.connection = self.client.connect(
                    self.output_port(identifier), name
                )
            return None
        raise ValueError(f"Unknown MIDI operation: {operation}")

    def input_port(self, identifier: str) -> _native.MidiInputPort:
        assert isinstance(self.client, _native.MidiInput)
        matches = [p for p in self.client.ports() if p.id() == identifier]
        if len(matches) != 1:
            raise _native.PortInfoError(
                "MIDI port is absent or its ID is ambiguous; rediscover ports"
            )
        return matches[0]

    def output_port(self, identifier: str) -> _native.MidiOutputPort:
        assert isinstance(self.client, _native.MidiOutput)
        matches = [p for p in self.client.ports() if p.id() == identifier]
        if len(matches) != 1:
            raise _native.PortInfoError(
                "MIDI port is absent or its ID is ambiguous; rediscover ports"
            )
        return matches[0]


def serve(worker: Worker, source: BinaryIO) -> None:
    while True:
        try:
            operation, args = cast(tuple[str, list[object]], pickle.load(source))
        except EOFError:
            return
        try:
            result = worker.perform(operation, args)
        except BusyError as error:
            worker.sender.send(
                "error", ("RuntimeError", str(error), False, dropped(worker))
            )
        except (TypeError, ValueError, NotImplementedError) as error:
            worker.sender.send(
                "error", (type(error).__name__, str(error), False, dropped(worker))
            )
        except (_native.MidiError, RuntimeError) as error:
            worker.sender.send(
                "error", (type(error).__name__, str(error), True, dropped(worker))
            )
            return
        else:
            worker.sender.send(
                "shutdown" if operation == "shutdown" else "ok",
                (result, dropped(worker)),
            )
        if operation == "shutdown":
            return


def dropped(worker: Worker) -> int:
    return 0 if worker.producer is None else worker.producer.dropped


def main() -> None:
    # Keep the extension import explicit even though native types are private.
    assert midi.__version__
    serve(Worker(Sender(sys.stdout.buffer)), sys.stdin.buffer)


if __name__ == "__main__":
    main()
