"""Production ownership and IPC exercised against controlled child faults."""

import gc
import inspect
import sys
import weakref
from collections.abc import AsyncIterator, Callable, Iterator
from functools import partial
from pathlib import Path
from queue import Full, Queue
from subprocess import Popen
from threading import Event, Thread
from time import monotonic, sleep
from typing import Self
from unittest.mock import patch

import pytest
from midirp import midi
from midirp._transport import (
    RECEIVE_CAPACITY,
    Context,
    Delivery,
    ReceiveQueue,
    shutdown,
)


@pytest.fixture
def workers(monkeypatch: pytest.MonkeyPatch) -> Iterator[list[Popen[bytes]]]:
    children: list[Popen[bytes]] = []
    fixture = Path(__file__).parent / "native" / "midirp_worker.py"

    def launch(command: list[str], stdin: int, stdout: int) -> Popen[bytes]:
        assert command == [sys.executable, "-m", "midirp._worker"]
        child = Popen([sys.executable, str(fixture)], stdin=stdin, stdout=stdout)
        children.append(child)
        return child

    monkeypatch.setattr("midirp._transport.subprocess.Popen", launch)
    yield children
    for c in children:
        if c.poll() is None:
            c.kill()
        c.wait(timeout=3)


def test_defaults_select_isolation_and_five_second_deadlines() -> None:
    for c in (midi.MidiInput, midi.MidiOutput):
        parameters = inspect.signature(c).parameters
        assert parameters["isolated"].default is True
        assert parameters["timeout"].default == 5.0
    assert (
        inspect.signature(midi.MidiInput).parameters["receive_byte_limit"].default
        == 8_388_608
    )


def test_receive_budget_recovers_after_dequeue_and_rejects_oversized_messages() -> None:
    messages = ReceiveQueue(8)
    messages.put_nowait((0, b"12345"))
    with pytest.raises(Full):
        messages.put_nowait((1, b"1234"))
    assert messages.get_nowait() == (0, b"12345")
    with pytest.raises(Full):
        messages.put_nowait((2, b"123456789"))
    messages.put_nowait((3, b"12345678"))
    assert messages.get_nowait() == (3, b"12345678")


@pytest.mark.parametrize("limit", [0, -1, True, 1.0])
def test_invalid_receive_limits_are_rejected_before_startup(limit: object) -> None:
    with pytest.raises((TypeError, ValueError)):
        midi.MidiInput("test", receive_byte_limit=limit)  # ty: ignore[invalid-argument-type]


@pytest.mark.parametrize("limit", [2, 3])
def test_receive_byte_limit_is_applied_in_the_child_and_exposes_drops(
    limit: int, workers: list[Popen[bytes]]
) -> None:
    received = Event()
    source = midi.MidiInput("early", receive_byte_limit=limit)
    with source.connect(
        source.ports()[0], "test", lambda t, m: received.set()
    ) as connection:
        if limit == 3:
            assert received.wait(3)
        assert connection.dropped_messages == (1 if limit == 2 else 0)
        if limit == 2:
            assert not received.is_set()


def test_partial_thread_startup_failure_terminates_and_reaps_its_child(
    workers: list[Popen[bytes]],
) -> None:
    start = Thread.start
    attempts = 0

    def limited_start(thread: Thread) -> None:
        nonlocal attempts
        attempts += 1
        if attempts == 2:
            raise RuntimeError("thread quota reached")
        start(thread)

    with patch.object(Thread, "start", limited_start):
        with pytest.raises(RuntimeError, match="thread quota"):
            midi.MidiOutput("output")
    assert len(workers) == 1
    assert workers[0].poll() is not None


@pytest.mark.parametrize("client", [midi.MidiInput, midi.MidiOutput])
def test_in_process_mode_does_not_start_child_processes(
    client: type[midi.MidiInput] | type[midi.MidiOutput], workers: list[Popen[bytes]]
) -> None:
    class LocalClient:
        def __init__(self, name: str) -> None:
            pass

        def ports(self) -> list[object]:
            return []

    with (
        patch("midirp._native.MidiInput", LocalClient),
        patch("midirp._native.MidiOutput", LocalClient),
    ):
        local = client("local", isolated=False)
        assert local.ports() == []
    assert not workers


def test_old_connection_generations_cannot_close_or_send_through_a_reopened_client(
    workers: list[Popen[bytes]],
) -> None:
    context = Context(1)
    context.request("init", ["output", "output"])
    context.open("connect", "output", "old", None)
    old_generation = context.generation
    context.close(old_generation)
    context.open("connect", "output", "new", None)
    context.close(old_generation)
    with pytest.raises(RuntimeError, match="closed"):
        context.request("send", [b"\xf8", old_generation], serial=True)
    context.request("send", [b"\xf8", context.generation], serial=True)
    context.close(context.generation)
    context.fail("test finished")
    workers[0].wait(timeout=3)


def test_isolated_discovery_send_close_and_reopen_use_the_same_client(
    workers: list[Popen[bytes]],
) -> None:
    output = midi.MidiOutput("check_bytes")
    (port,) = output.ports()
    assert port.id() == "output"
    assert output.port_name(port) == "output port"
    assert output.find_port_by_id("output") == port
    assert output.find_port_by_id("absent") is None
    with pytest.raises(TypeError):
        hash(port)
    for _ in range(2):
        with output.connect(port, "test") as connection:
            with pytest.raises(RuntimeError, match="unavailable"):
                output.ports()
            connection.send(b"\xf0\x7d\x00\xff\xf7")
        assert connection.closed
        connection.close()
        with pytest.raises(RuntimeError, match="closed"):
            connection.send(b"\xf8")
    assert len(workers) == 1
    del output, connection
    workers[0].wait(timeout=3)


@pytest.mark.parametrize("mode", ["hang_send", "crash_send", "native_error"])
def test_native_failure_invalidates_only_its_worker_and_requires_a_fresh_client(
    mode: str, workers: list[Popen[bytes]]
) -> None:
    broken = midi.MidiOutput(mode, timeout=1)
    healthy = midi.MidiOutput("healthy")
    bad_connection = broken.connect(broken.ports()[0], "test")
    good_connection = healthy.connect(healthy.ports()[0], "test")
    error = (
        TimeoutError
        if mode == "hang_send"
        else midi.SendError
        if mode == "native_error"
        else RuntimeError
    )
    started = monotonic()
    with pytest.raises(error):
        bad_connection.send(b"\xf8")
    assert monotonic() - started < 3
    assert bad_connection.closed
    with pytest.raises(RuntimeError, match="new client"):
        broken.ports()
    with pytest.raises(RuntimeError, match="new client"):
        bad_connection.close()
    good_connection.send(b"\xf8")
    good_connection.close()
    workers[0].wait(timeout=3)
    assert workers[1].poll() is None
    fresh = midi.MidiOutput("fresh")
    with fresh.connect(fresh.ports()[0], "test") as connection:
        connection.send(b"\xf8")


@pytest.mark.parametrize("isolated", [True, False])
@pytest.mark.parametrize("form", ["bytearray", "view", "strided_view"])
def test_send_snapshots_mutable_and_strided_buffers(
    isolated: bool, form: str, workers: list[Popen[bytes]]
) -> None:
    expected = b"\xf0\x7d\x00\xff\xf7"
    buffer = bytearray(
        expected if form != "strided_view" else b"\xf0a\x7db\x00c\xffd\xf7e"
    )
    message = (
        buffer
        if form == "bytearray"
        else memoryview(buffer)
        if form == "view"
        else memoryview(buffer)[::2]
    )

    class LocalConnection:
        def send(self, message: bytes) -> None:
            buffer[:] = b"x" * len(buffer)
            assert isinstance(message, bytes)
            assert message == expected

        def close(self) -> None:
            pass

    class LocalClient:
        def __init__(self, name: str) -> None:
            pass

        def ports(self) -> list[object]:
            return [object()]

        def connect(self, port: object, name: str) -> LocalConnection:
            return LocalConnection()

    with patch("midirp._native.MidiOutput", LocalClient):
        output = midi.MidiOutput("check_bytes", isolated=isolated)
        connection = output.connect(output.ports()[0], "test")
        connection.send(message)
        connection.close()


def test_native_close_timeout_consumes_parent_ownership_without_restoring_a_client(
    workers: list[Popen[bytes]],
) -> None:
    output = midi.MidiOutput("hang_close", timeout=1)
    connection = output.connect(output.ports()[0], "test")
    with pytest.raises(TimeoutError):
        connection.close()
    assert connection.closed
    workers[0].wait(timeout=3)
    with pytest.raises(RuntimeError, match="new client"):
        output.ports()


@pytest.mark.parametrize("isolated", [True, False])
@pytest.mark.parametrize("direction", ["input", "output"])
@pytest.mark.parametrize("interrupt", [True, False])
def test_context_exit_preserves_body_and_close_failures(
    isolated: bool, direction: str, interrupt: bool, workers: list[Popen[bytes]]
) -> None:
    class LocalConnection:
        closed = False

        def __enter__(self) -> Self:
            return self

        def close(self) -> None:
            raise RuntimeError("close failed")

    class LocalClient:
        def __init__(self, name: str) -> None:
            pass

        def ports(self) -> list[object]:
            return [object()]

        def connect(
            self,
            port: object,
            name: str,
            callback: Callable[[int, bytes], object] | None = None,
        ) -> LocalConnection:
            return LocalConnection()

    with (
        patch("midirp._native.MidiInput", LocalClient),
        patch("midirp._native.MidiOutput", LocalClient),
    ):
        if direction == "input":
            source = midi.MidiInput("hang_close", isolated=isolated, timeout=0.5)
            connection = source.connect(source.ports()[0], "test", lambda t, m: None)
        else:
            output = midi.MidiOutput("hang_close", isolated=isolated, timeout=0.5)
            connection = output.connect(output.ports()[0], "test")
        body_error = (
            KeyboardInterrupt("body interrupted")
            if interrupt
            else ValueError("body failed")
        )
        with pytest.raises(BaseExceptionGroup) as caught:
            with connection:
                raise body_error
        assert caught.value.exceptions[0] is body_error
        assert isinstance(
            caught.value.exceptions[1], TimeoutError if isolated else RuntimeError
        )
        assert isinstance(
            caught.value, BaseExceptionGroup if interrupt else ExceptionGroup
        )


def test_failed_open_discards_the_worker_instead_of_restoring_a_suspect_native_client(
    workers: list[Popen[bytes]],
) -> None:
    output = midi.MidiOutput("failed_open")
    with pytest.raises(midi.ConnectError, match="vanished"):
        output.connect(output.ports()[0], "test")
    workers[0].wait(timeout=3)
    with pytest.raises(RuntimeError, match="new client"):
        output.ports()


def test_ambiguous_port_ids_are_rejected_before_opening(
    workers: list[Popen[bytes]],
) -> None:
    source = midi.MidiInput("ambiguous")
    with pytest.raises(midi.PortInfoError, match="ambiguous"):
        source.connect(source.ports()[0], "test", lambda t, m: None)
    workers[0].wait(timeout=3)


@pytest.mark.parametrize("form", ["function", "partial", "object", "uninspectable"])
def test_input_callback_can_receive_during_open_without_being_pickled(
    form: str,
    workers: list[Popen[bytes]],
) -> None:
    messages: Queue[tuple[int, bytes]] = Queue()

    def receive(timestamp: int, message: bytes) -> None:
        messages.put((timestamp, message))

    class Receiver:
        __call__ = staticmethod(receive)

    class UninspectableReceiver(Receiver):
        @property
        def __signature__(self) -> inspect.Signature:
            raise ValueError("No signature available")

    callback = {
        "function": receive,
        "partial": partial(receive),
        "object": Receiver(),
        "uninspectable": UninspectableReceiver(),
    }[form]
    source = midi.MidiInput("early")
    with source.connect(source.ports()[0], "test", callback) as connection:
        assert messages.get(timeout=3) == (17, b"\x90\x3c\x7f")
        assert connection.dropped_messages == 0


@pytest.mark.parametrize("isolated", [True, False])
def test_incompatible_callback_signatures_do_not_consume_the_client(
    isolated: bool, workers: list[Popen[bytes]]
) -> None:
    def one_argument(timestamp: int) -> None:
        pass

    def extra_argument(timestamp: int, message: bytes, required: int) -> None:
        pass

    def keyword_only(*, timestamp: int, message: bytes) -> None:
        pass

    class LocalClient:
        def __init__(self, name: str) -> None:
            pass

        def ports(self) -> list[object]:
            return [object()]

    with patch("midirp._native.MidiInput", LocalClient):
        source = midi.MidiInput("early", isolated=isolated)
        for c in (one_argument, extra_argument, keyword_only):
            with pytest.raises(TypeError, match="two positional arguments"):
                source.connect(source.ports()[0], "test", c)  # ty: ignore[invalid-argument-type]
            with pytest.raises(TypeError, match="two positional arguments"):
                source.create_virtual("test", c)  # ty: ignore[invalid-argument-type]
            assert source.ports()


@pytest.mark.parametrize("isolated", [True, False])
@pytest.mark.parametrize("operation", ["connect", "create_virtual"])
@pytest.mark.parametrize("kind", ["coroutine", "generator", "async_generator"])
def test_deferred_callbacks_are_rejected_without_consuming_the_client(
    isolated: bool, operation: str, kind: str, workers: list[Popen[bytes]]
) -> None:
    async def coroutine(timestamp: int, message: bytes) -> None:
        pass

    def generator(timestamp: int, message: bytes) -> Iterator[None]:
        yield None

    async def async_generator(timestamp: int, message: bytes) -> AsyncIterator[None]:
        yield None

    callback: Callable[[int, bytes], object] = {
        "coroutine": coroutine,
        "generator": generator,
        "async_generator": async_generator,
    }[kind]

    class Receiver:
        __call__ = staticmethod(callback)

    class LocalClient:
        def __init__(self, name: str) -> None:
            pass

        def ports(self) -> list[object]:
            return [object()]

    with patch("midirp._native.MidiInput", LocalClient):
        source = midi.MidiInput("early", isolated=isolated)
        for c in (callback, partial(callback), Receiver(), partial(Receiver())):
            with pytest.raises(TypeError, match="synchronous and not a generator"):
                if operation == "connect":
                    source.connect(source.ports()[0], "test", c)
                else:
                    source.create_virtual("test", c)
            assert source.ports()


def test_parent_queue_drops_new_messages_and_counts_both_queue_stages() -> None:
    started = Event()
    release = Event()
    finished = Event()
    messages: list[int] = []

    def receive(timestamp: int, message: bytes) -> None:
        messages.append(timestamp)
        started.set()
        assert release.wait(3)
        finished.set()

    delivery = Delivery(receive)
    delivery.enqueue(0, b"\xf8", 7)
    assert started.wait(3)
    for i in range(1, RECEIVE_CAPACITY + 4):
        delivery.enqueue(i, b"\xf8", 7)
    assert delivery.dropped == 3
    assert delivery.worker_dropped == 7
    # Delayed forwarding must not undo a newer control-reply snapshot.
    delivery.enqueue(RECEIVE_CAPACITY + 4, b"\xf8", 3)
    assert delivery.worker_dropped == 7
    assert delivery.dropped == 4
    delivery.retire()
    release.set()
    delivery.bridge.drain()
    assert finished.wait(3)
    assert messages == [0]


def test_child_queue_overflow_is_exposed_after_native_close(
    workers: list[Popen[bytes]],
) -> None:
    source = midi.MidiInput("burst")
    connection = source.connect(source.ports()[0], "test", lambda t, m: None)
    connection.close()
    assert connection.dropped_messages > 0


def test_explicit_close_waits_for_parent_callback_without_cancelling_it(
    workers: list[Popen[bytes]],
) -> None:
    started = Event()
    release = Event()
    closing = Event()
    finished: Queue[object] = Queue()

    def receive(timestamp: int, message: bytes) -> None:
        started.set()
        assert release.wait(3)

    source = midi.MidiInput("early", timeout=1)
    connection = source.connect(source.ports()[0], "test", receive)
    assert started.wait(3)

    def close() -> None:
        closing.set()
        connection.close()
        finished.put(None)

    thread = Thread(target=close)
    thread.start()
    assert closing.wait(3)
    try:
        sleep(0.15)
        assert finished.empty()
    finally:
        release.set()
        thread.join(timeout=3)
    assert finished.get(timeout=3) is None
    assert connection.closed


def test_shutdown_terminates_and_reaps_a_child_stuck_in_native_close(
    workers: list[Popen[bytes]],
) -> None:
    output = midi.MidiOutput("hang_close", timeout=1)
    connection = output.connect(output.ports()[0], "test")
    started = monotonic()
    with patch("midirp._transport.SHUTTING_DOWN", False):
        shutdown()
        with pytest.raises(RuntimeError, match="shutdown"):
            midi.MidiOutput("late client")
    assert monotonic() - started < 3
    assert connection.closed
    assert workers[0].poll() is not None


def test_initialization_timeout_reaps_the_new_worker(
    workers: list[Popen[bytes]],
) -> None:
    with pytest.raises(TimeoutError):
        midi.MidiOutput("hang_init", timeout=1)
    workers[0].wait(timeout=3)


def test_callback_and_error_hook_cannot_start_or_send_native_work(
    workers: list[Popen[bytes]],
) -> None:
    reported = Event()
    errors: list[str] = []
    output = midi.MidiOutput("output")
    target = output.connect(output.ports()[0], "test")

    def receive(timestamp: int, message: bytes) -> None:
        target.send(message)

    def error_hook(event: object) -> None:
        try:
            midi.MidiInput("forbidden")
        except RuntimeError as error:
            errors.append(str(error))
        reported.set()

    source = midi.MidiInput("early")
    with patch("sys.unraisablehook", error_hook):
        connection = source.connect(source.ports()[0], "test", receive)
        assert reported.wait(3)
        connection.close()
    assert len(errors) == 1
    assert "callback" in errors[0]
    assert len(workers) == 2
    target.send(b"\xf8")
    target.close()


def test_cyclic_collection_releases_the_client_and_child(
    workers: list[Popen[bytes]],
) -> None:
    source = midi.MidiInput("quiet")

    class Receiver:
        connection: midi.MidiInputConnection

        def __call__(self, timestamp: int, message: bytes) -> None:
            pass

    receiver = Receiver()
    connection = source.connect(source.ports()[0], "test", receiver)
    receiver.connection = connection
    reference = weakref.ref(connection)
    del receiver, connection, source
    gc.collect()
    assert reference() is None
    workers[0].wait(timeout=3)


def test_destruction_schedules_close_and_restores_a_still_live_client(
    workers: list[Popen[bytes]],
) -> None:
    output = midi.MidiOutput("output")
    connection = output.connect(output.ports()[0], "test")
    del connection
    deadline = monotonic() + 3
    while True:
        try:
            ports = output.ports()
            break
        except RuntimeError:
            assert monotonic() < deadline
            sleep(0.01)
    assert len(ports) == 1
    assert workers[0].poll() is None


@pytest.mark.parametrize("timeout", [0, -1, float("nan"), float("inf")])
def test_invalid_deadlines_are_rejected_before_starting_a_worker(
    timeout: float, workers: list[Popen[bytes]]
) -> None:
    with pytest.raises(ValueError, match="positive and finite"):
        midi.MidiOutput("invalid", timeout=timeout)
    assert not workers
