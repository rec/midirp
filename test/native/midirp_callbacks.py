"""Python callables used by the private Rust native-thread callback driver."""

from collections.abc import Callable, Iterator
from contextlib import contextmanager
from threading import Event, get_ident
from typing import Protocol
from unittest.mock import patch


class Connection(Protocol):
    def close(self) -> None: ...


class Output(Protocol):
    def send(self, message: bytes) -> None: ...


class UnraisableEvent(Protocol):
    exc_value: BaseException | None
    object: object


class Recorder:
    def __init__(self) -> None:
        self.messages: list[tuple[int, bytes]] = []
        self.threads: list[int] = []

    def __call__(self, timestamp: int, message: bytes) -> object:
        self.messages.append((timestamp, message))
        self.threads.append(get_ident())
        return 37  # The binding ignores callback return values.

    def verify_bytes(self) -> None:
        assert all(type(m) is bytes for _, m in self.messages)


class SendingRecorder(Recorder):
    def __init__(self, output: Output) -> None:
        super().__init__()
        self.output = output

    def __call__(self, timestamp: int, message: bytes) -> None:
        super().__call__(timestamp, message)
        self.output.send(message)


class ConstructorRecorder(Recorder):
    def __init__(self, constructors: list[Callable[[str], object]]) -> None:
        super().__init__()
        self.constructors = constructors
        self.errors: list[str] = []

    def __call__(self, timestamp: int, message: bytes) -> None:
        super().__call__(timestamp, message)
        for c in self.constructors:
            try:
                c("callback")
            except RuntimeError as e:
                self.errors.append(str(e))


class RaisingRecorder(Recorder):
    def __call__(self, timestamp: int, message: bytes) -> None:
        super().__call__(timestamp, message)
        if len(self.messages) == 1:
            raise ValueError("callback failed")


class CloseRecorder(Recorder):
    def __init__(self) -> None:
        super().__init__()
        self.targets: list[Connection] = []
        self.errors: list[str] = []

    def __call__(self, timestamp: int, message: bytes) -> None:
        super().__call__(timestamp, message)
        for t in self.targets:
            try:
                t.close()
            except RuntimeError as e:
                self.errors.append(str(e))


class DroppingRecorder(CloseRecorder):
    def __call__(self, timestamp: int, message: bytes) -> None:
        Recorder.__call__(self, timestamp, message)
        self.targets.clear()


class BlockingRecorder(Recorder):
    def __init__(self) -> None:
        super().__init__()
        self.started = Event()
        self.release = Event()

    def __call__(self, timestamp: int, message: bytes) -> None:
        super().__call__(timestamp, message)
        self.started.set()
        if not self.release.wait(5):
            raise RuntimeError("test callback release deadline exceeded")


class UnraisableRecorder:
    def __init__(self) -> None:
        self.errors: list[str] = []
        self.objects: list[object] = []
        self.targets: list[Connection] = []
        self.close_errors: list[str] = []
        self.outputs: list[Output] = []
        self.send_errors: list[str] = []

    def __call__(self, event: UnraisableEvent) -> None:
        self.errors.append(str(event.exc_value))
        self.objects.append(event.object)
        for t in self.targets:
            try:
                t.close()
            except RuntimeError as e:
                self.close_errors.append(str(e))
        for o in self.outputs:
            try:
                o.send(b"\xf8")
            except RuntimeError as e:
                self.send_errors.append(str(e))


@contextmanager
def capture_errors() -> Iterator[UnraisableRecorder]:
    recorder = UnraisableRecorder()
    with patch("sys.unraisablehook", recorder):
        yield recorder


def verify_send_boundary(output: Output) -> None:
    for m in (bytearray(b"x"), memoryview(b"x"), [1], (1,), "x", 1, None):
        try:
            output.send(m)  # ty: ignore[invalid-argument-type]
        except TypeError:
            pass
        else:
            raise AssertionError("send accepted a non-bytes argument")
    output.send(b"\x90\x3c\x7f")
