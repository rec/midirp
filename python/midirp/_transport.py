"""Private, deadline-bounded local pipes and parent callback delivery."""

from __future__ import annotations

import atexit
import pickle
import subprocess
import sys
import weakref
from collections.abc import Callable
from queue import Empty, Full, Queue
from threading import Event, Lock, Thread
from time import monotonic
from typing import BinaryIO, NoReturn, cast

from . import _native


class Delivery:
    def __init__(self, callback: Callable[[int, bytes], object]) -> None:
        self.bridge = _native.CallbackBridge(callback)
        self.messages: Queue[tuple[int, bytes]] = Queue(RECEIVE_CAPACITY)
        self.retired = False
        self.dropped = 0
        self.worker_dropped = 0
        Thread(
            target=dispatch,
            args=(weakref.ref(self), self.messages),
            name="midirp callback",
            daemon=True,
        ).start()

    def enqueue(self, timestamp: int, message: bytes, dropped: int) -> None:
        self.worker_dropped = max(self.worker_dropped, dropped)
        if not self.retired:
            try:
                self.messages.put_nowait((timestamp, message))
            except Full:
                self.dropped += 1

    def retire(self) -> None:
        self.retired = True
        self.bridge.retire()

    def __del__(self) -> None:
        if hasattr(self, "bridge"):
            self.retire()


class Context:
    def __init__(self, timeout: float) -> None:
        if SHUTTING_DOWN:
            raise RuntimeError("MIDI interpreter shutdown has begun")
        self.timeout = timeout
        self.operation = Lock()
        self.requests: Queue[tuple[str, list[object]]] = Queue(1)
        self.replies: Queue[tuple[str, object]] = Queue(1)
        self.stopped = Event()
        self.failure = ""
        self.state = "available"
        self.delivery: weakref.ReferenceType[Delivery] | None = None
        self.generation = 0
        self.worker_dropped = 0
        self.disposing = False
        self.process: subprocess.Popen[bytes] = subprocess.Popen(
            [sys.executable, "-m", "midirp._worker"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
        )
        try:
            CONTEXTS.add(self)
            Thread(
                target=write_requests, args=(self,), daemon=True, name="midirp requests"
            ).start()
            Thread(
                target=read_results, args=(self,), daemon=True, name="midirp results"
            ).start()
        except (RuntimeError, MemoryError):
            self.fail("Cannot start MIDI communication threads")
            self.process.wait(timeout=1)
            cast(BinaryIO, self.process.stdin).close()
            cast(BinaryIO, self.process.stdout).close()
            raise

    def request(
        self, operation: str, args: list[object], *, serial: bool = False
    ) -> object:
        _native.check_thread()
        deadline = monotonic() + self.timeout
        acquired = (
            self.operation.acquire(timeout=self.timeout)
            if serial
            else self.operation.acquire(blocking=False)
        )
        if not acquired:
            if serial:
                self.expire(operation)
            raise RuntimeError("MIDI client is busy with another operation")
        try:
            return self.exchange(operation, args, deadline)
        finally:
            self.operation.release()

    def exchange(self, operation: str, args: list[object], deadline: float) -> object:
        if self.failure:
            raise RuntimeError(
                f"MIDI worker failed; create a new client: {self.failure}"
            )
        if operation in ("ports", "port_name", "ignore") and self.state != "available":
            raise RuntimeError(
                "MIDI client is unavailable while connecting, connected, or closing"
            )
        if operation == "send" and (
            args[1] != self.generation or self.state != "connected"
        ):
            raise RuntimeError("MIDI connection is closed")
        self.requests.put_nowait((operation, args))
        try:
            tag, payload = self.replies.get(timeout=max(0, deadline - monotonic()))
        except Empty:
            self.expire(operation)
        if tag == "dead":
            raise RuntimeError(
                f"MIDI worker failed; create a new client: {self.failure}"
            )
        if tag == "error":
            kind, message, fatal, dropped = cast(tuple[str, str, bool, int], payload)
            self.update_dropped(dropped)
            if fatal:
                self.fail(message)
            errors: dict[str, type[Exception]] = {
                "MidiError": _native.MidiError,
                "InitError": _native.InitError,
                "PortInfoError": _native.PortInfoError,
                "ConnectError": _native.ConnectError,
                "SendError": _native.SendError,
                "RuntimeError": RuntimeError,
                "TypeError": TypeError,
                "ValueError": ValueError,
                "NotImplementedError": NotImplementedError,
            }
            raise errors[kind](message)
        result, dropped = cast(tuple[object, int], payload)
        self.update_dropped(dropped)
        if self.failure and operation != "shutdown":
            raise RuntimeError(
                f"MIDI worker failed; create a new client: {self.failure}"
            )
        return result

    def close(self, generation: int) -> None:
        _native.check_thread()
        deadline = monotonic() + self.timeout
        if not self.operation.acquire(timeout=self.timeout):
            self.expire("close")
        try:
            if generation == self.generation:
                if self.failure:
                    raise RuntimeError(
                        f"MIDI worker failed; create a new client: {self.failure}"
                    )
                self.state = "closing"
                self.exchange("close", [generation], deadline)
                self.state = "available"
        finally:
            self.operation.release()

    def open(
        self,
        operation: str,
        identifier: str,
        name: str,
        callback: Callable[[int, bytes], object] | None,
    ) -> Delivery | None:
        _native.check_thread()
        if not self.operation.acquire(blocking=False):
            raise RuntimeError("MIDI client is busy with another operation")
        delivery = None
        try:
            if self.failure:
                raise RuntimeError(
                    f"MIDI worker failed; create a new client: {self.failure}"
                )
            if self.state != "available":
                raise RuntimeError(
                    "MIDI client is unavailable while connecting, connected, or closing"
                )
            self.state = "opening"
            self.generation += 1
            self.worker_dropped = 0
            delivery = Delivery(callback) if callback is not None else None
            self.delivery = weakref.ref(delivery) if delivery is not None else None
            self.exchange(
                operation,
                [identifier, name, self.generation],
                monotonic() + self.timeout,
            )
            self.state = "connected"
            return delivery
        except (
            RuntimeError,
            TimeoutError,
            _native.MidiError,
            NotImplementedError,
            TypeError,
            ValueError,
            MemoryError,
        ):
            if delivery is not None:
                delivery.retire()
            if not self.failure:
                self.state = "available"
            raise
        finally:
            self.operation.release()

    def update_dropped(self, dropped: int) -> None:
        self.worker_dropped = max(self.worker_dropped, dropped)
        if self.delivery is not None and (delivery := self.delivery()) is not None:
            delivery.worker_dropped = max(delivery.worker_dropped, dropped)

    def expire(self, operation: str) -> NoReturn:
        message = f"MIDI {operation} exceeded {self.timeout:g}s; create a new client"
        self.fail(message)
        raise TimeoutError(message)

    def fail(self, message: str) -> None:
        if not self.failure:
            self.failure = message
            self.state = "failed"
            self.stopped.set()
            if self.delivery is not None and (delivery := self.delivery()) is not None:
                delivery.retire()
            try:
                self.process.kill()
            except ProcessLookupError:
                pass
            except OSError as error:
                print(f"midirp: cannot terminate worker: {error}", file=sys.stderr)
            try:
                self.replies.put_nowait(("dead", None))
            except Full:
                pass

    def defer_close(self, generation: int) -> None:
        if self.state == "connected" and generation == self.generation:
            try:
                Thread(
                    target=close_deferred,
                    args=(self, generation),
                    daemon=True,
                    name="midirp cleanup",
                ).start()
            except RuntimeError:
                self.fail("Cannot start deferred MIDI close")

    def dispose(self) -> None:
        if not self.disposing and not self.stopped.is_set():
            self.disposing = True
            try:
                Thread(
                    target=dispose, args=(self,), daemon=True, name="midirp disposal"
                ).start()
            except RuntimeError:
                self.fail("Cannot start MIDI worker disposal")


def dispatch(
    reference: weakref.ReferenceType[Delivery], messages: Queue[tuple[int, bytes]]
) -> None:
    while True:
        try:
            timestamp, message = messages.get(timeout=0.1)
        except Empty:
            delivery = reference()
            if delivery is None or delivery.retired:
                return
            del delivery
            continue
        delivery = reference()
        if delivery is None or delivery.retired:
            return
        delivery.bridge.deliver(timestamp, message)
        del delivery


def write_requests(context: Context) -> None:
    stream = cast(BinaryIO, context.process.stdin)
    try:
        while not context.stopped.is_set():
            try:
                request = context.requests.get(timeout=0.1)
            except Empty:
                continue
            pickle.dump(request, stream, protocol=5)
            stream.flush()
    except (OSError, ValueError) as error:
        context.fail(f"MIDI worker pipe failed: {error}")
    finally:
        try:
            stream.close()
        except OSError:
            pass


def read_results(context: Context) -> None:
    stream = cast(BinaryIO, context.process.stdout)
    try:
        while not context.stopped.is_set():
            tag, payload = cast(tuple[str, object], pickle.load(stream))
            if tag == "message":
                generation, timestamp, message, dropped = cast(
                    tuple[int, int, bytes, int], payload
                )
                if generation == context.generation:
                    context.update_dropped(dropped)
                    if context.delivery is not None:
                        offer(context.delivery, timestamp, message, dropped)
            else:
                context.replies.put_nowait((tag, payload))
                if tag == "shutdown":
                    context.failure = "MIDI worker shut down"
                    context.state = "failed"
                    context.stopped.set()
                    break
    except (EOFError, OSError, ValueError, pickle.UnpicklingError, Full) as error:
        context.fail(f"MIDI worker exited or communication failed: {error}")
    finally:
        stream.close()
        try:
            context.process.wait(timeout=1)
        except subprocess.TimeoutExpired:
            context.fail("MIDI worker did not exit after termination")


def offer(
    reference: weakref.ReferenceType[Delivery],
    timestamp: int,
    message: bytes,
    dropped: int,
) -> None:
    if (delivery := reference()) is not None:
        delivery.enqueue(timestamp, message, dropped)


def close_deferred(context: Context, generation: int) -> None:
    try:
        context.close(generation)
    except (RuntimeError, TimeoutError, _native.MidiError):
        # Deferred errors already invalidate the worker; no unraisable traceback.
        return


def dispose(context: Context) -> None:
    try:
        context.request("shutdown", [], serial=True)
    except (RuntimeError, TimeoutError, _native.MidiError):
        return


def shutdown() -> None:
    global SHUTTING_DOWN
    SHUTTING_DOWN = True
    contexts = list(CONTEXTS)
    deliveries = [
        d
        for c in contexts
        if c.delivery is not None and (d := c.delivery()) is not None
    ]
    for d in deliveries:
        d.retire()
    deadline = monotonic() + max(
        (c.timeout for c in contexts if not c.stopped.is_set()), default=0
    )
    for c in contexts:
        if not c.stopped.is_set():
            try:
                c.requests.put_nowait(("shutdown", []))
            except Full:
                c.fail("MIDI shutdown raced with an active request")
    for c in contexts:
        try:
            c.process.wait(timeout=max(0, deadline - monotonic()))
        except subprocess.TimeoutExpired:
            c.fail("MIDI shutdown deadline exceeded")
    reap_deadline = monotonic() + 1
    for c in contexts:
        try:
            c.process.wait(timeout=max(0, reap_deadline - monotonic()))
        except subprocess.TimeoutExpired:
            print("midirp: worker termination could not be confirmed", file=sys.stderr)
    for d in deliveries:
        # Native children are stopped first. Arbitrary Python cannot be cancelled.
        d.bridge.drain()


RECEIVE_CAPACITY = 128
CONTEXTS: weakref.WeakSet[Context] = weakref.WeakSet()
SHUTTING_DOWN = False
atexit.register(shutdown)
