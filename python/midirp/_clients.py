"""One ownership facade for isolated and direct native MIDI."""

from __future__ import annotations

import inspect
import math
import sys
from collections.abc import Callable
from functools import partial
from types import TracebackType
from typing import cast

from . import _native
from ._transport import DEFAULT_RECEIVE_BYTE_LIMIT, Context, Delivery


class MidiInputPort:
    __slots__ = ("_native", "_identifier")
    __hash__ = None

    def __init__(self) -> None:
        raise TypeError("MIDI port handles cannot be constructed directly")

    @classmethod
    def _create(
        cls, native: _native.MidiInputPort | None, identifier: str = ""
    ) -> MidiInputPort:
        port = object.__new__(cls)
        port._native = native
        port._identifier = identifier
        return port

    def id(self) -> str:
        _native.check_thread()
        return self._identifier if self._native is None else self._native.id()

    def __eq__(self, other: object) -> bool:
        _native.check_thread()
        if not isinstance(other, MidiInputPort):
            return NotImplemented
        if self._native is not None and other._native is not None:
            return self._native == other._native
        return self.id() == other.id()

    def __ne__(self, other: object) -> bool:
        return not self == other


class MidiOutputPort:
    __slots__ = ("_native", "_identifier")
    __hash__ = None

    def __init__(self) -> None:
        raise TypeError("MIDI port handles cannot be constructed directly")

    @classmethod
    def _create(
        cls, native: _native.MidiOutputPort | None, identifier: str = ""
    ) -> MidiOutputPort:
        port = object.__new__(cls)
        port._native = native
        port._identifier = identifier
        return port

    def id(self) -> str:
        _native.check_thread()
        return self._identifier if self._native is None else self._native.id()

    def __eq__(self, other: object) -> bool:
        _native.check_thread()
        if not isinstance(other, MidiOutputPort):
            return NotImplemented
        if self._native is not None and other._native is not None:
            return self._native == other._native
        return self.id() == other.id()

    def __ne__(self, other: object) -> bool:
        return not self == other


class MidiInput:
    __slots__ = ("_native", "_context")

    def __init__(
        self,
        client_name: str,
        *,
        isolated: bool = True,
        timeout: float = 5.0,
        receive_byte_limit: int = DEFAULT_RECEIVE_BYTE_LIMIT,
    ) -> None:
        validate_settings(client_name, isolated, timeout)
        if not isinstance(receive_byte_limit, int) or isinstance(
            receive_byte_limit, bool
        ):
            raise TypeError("receive_byte_limit must be an integer")
        if receive_byte_limit <= 0:
            raise ValueError("receive_byte_limit must be positive")
        self._native = None if isolated else _native.MidiInput(client_name)
        self._context = Context(timeout, receive_byte_limit) if isolated else None
        if self._context is not None:
            self._context.request("init", ["input", client_name])

    def ports(self) -> list[MidiInputPort]:
        if self._context is not None:
            identifiers = cast(list[str], self._context.request("ports", []))
            return [MidiInputPort._create(None, i) for i in identifiers]
        assert self._native is not None
        return [MidiInputPort._create(p) for p in self._native.ports()]

    def port_name(self, port: MidiInputPort) -> str:
        _native.check_thread()
        if not isinstance(port, MidiInputPort):
            raise TypeError("Expected a MIDI input port")
        if self._context is not None:
            return cast(str, self._context.request("port_name", [port.id()]))
        assert self._native is not None
        native = (
            port._native
            if port._native is not None
            else self._native.find_port_by_id(port.id())
        )
        if native is None:
            raise _native.PortInfoError("MIDI input port is absent")
        return self._native.port_name(native)

    def find_port_by_id(self, id: str) -> MidiInputPort | None:
        _native.check_thread()
        if not isinstance(id, str):
            raise TypeError("Expected a string MIDI port ID")
        if self._context is not None:
            return next((p for p in self.ports() if p.id() == id), None)
        assert self._native is not None
        port = self._native.find_port_by_id(id)
        return None if port is None else MidiInputPort._create(port)

    def ignore(self, flags: _native.Ignore) -> None:
        _native.check_thread()
        if not isinstance(flags, _native.Ignore):
            raise TypeError("Expected Ignore flags")
        if self._context is not None:
            self._context.request("ignore", [int(flags)])
        else:
            assert self._native is not None
            self._native.ignore(flags)

    def connect(
        self,
        port: MidiInputPort,
        port_name: str,
        callback: Callable[[int, bytes], object],
    ) -> MidiInputConnection:
        validate_open(port_name, callback)
        if not callable(callback):
            raise TypeError("MIDI callback must be callable")
        if not isinstance(port, MidiInputPort):
            raise TypeError("Expected a MIDI input port")
        if self._context is not None:
            connection = MidiInputConnection._create(self, None)
            connection._delivery = self._context.open(
                "connect", port.id(), port_name, callback
            )
            connection._generation = self._context.generation
            connection._closed = False
            return connection
        assert self._native is not None
        native = (
            port._native
            if port._native is not None
            else self._native.find_port_by_id(port.id())
        )
        if native is None:
            raise _native.ConnectError("MIDI input port is absent")
        return MidiInputConnection._create(
            self, self._native.connect(native, port_name, callback)
        )

    def create_virtual(
        self, port_name: str, callback: Callable[[int, bytes], object]
    ) -> MidiInputConnection:
        validate_open(port_name, callback)
        if not callable(callback):
            raise TypeError("MIDI callback must be callable")
        if sys.platform == "win32":
            raise NotImplementedError(
                "Virtual MIDI input ports are not supported on this platform"
            )
        if self._context is not None:
            connection = MidiInputConnection._create(self, None)
            connection._delivery = self._context.open(
                "virtual", "", port_name, callback
            )
            connection._generation = self._context.generation
            connection._closed = False
            return connection
        assert self._native is not None
        return MidiInputConnection._create(
            self, self._native.create_virtual(port_name, callback)
        )

    def __del__(self) -> None:
        if (context := getattr(self, "_context", None)) is not None:
            context.dispose()


class MidiOutput:
    __slots__ = ("_native", "_context")

    def __init__(
        self, client_name: str, *, isolated: bool = True, timeout: float = 5.0
    ) -> None:
        validate_settings(client_name, isolated, timeout)
        self._native = None if isolated else _native.MidiOutput(client_name)
        self._context = Context(timeout) if isolated else None
        if self._context is not None:
            self._context.request("init", ["output", client_name])

    def ports(self) -> list[MidiOutputPort]:
        if self._context is not None:
            identifiers = cast(list[str], self._context.request("ports", []))
            return [MidiOutputPort._create(None, i) for i in identifiers]
        assert self._native is not None
        return [MidiOutputPort._create(p) for p in self._native.ports()]

    def port_name(self, port: MidiOutputPort) -> str:
        _native.check_thread()
        if not isinstance(port, MidiOutputPort):
            raise TypeError("Expected a MIDI output port")
        if self._context is not None:
            return cast(str, self._context.request("port_name", [port.id()]))
        assert self._native is not None
        native = (
            port._native
            if port._native is not None
            else self._native.find_port_by_id(port.id())
        )
        if native is None:
            raise _native.PortInfoError("MIDI output port is absent")
        return self._native.port_name(native)

    def find_port_by_id(self, id: str) -> MidiOutputPort | None:
        _native.check_thread()
        if not isinstance(id, str):
            raise TypeError("Expected a string MIDI port ID")
        if self._context is not None:
            return next((p for p in self.ports() if p.id() == id), None)
        assert self._native is not None
        port = self._native.find_port_by_id(id)
        return None if port is None else MidiOutputPort._create(port)

    def connect(self, port: MidiOutputPort, port_name: str) -> MidiOutputConnection:
        validate_open(port_name)
        if not isinstance(port, MidiOutputPort):
            raise TypeError("Expected a MIDI output port")
        if self._context is not None:
            connection = MidiOutputConnection._create(self, None)
            self._context.open("connect", port.id(), port_name, None)
            connection._generation = self._context.generation
            connection._closed = False
            return connection
        assert self._native is not None
        native = (
            port._native
            if port._native is not None
            else self._native.find_port_by_id(port.id())
        )
        if native is None:
            raise _native.ConnectError("MIDI output port is absent")
        return MidiOutputConnection._create(
            self, self._native.connect(native, port_name)
        )

    def create_virtual(self, port_name: str) -> MidiOutputConnection:
        validate_open(port_name)
        if sys.platform == "win32":
            raise NotImplementedError(
                "Virtual MIDI output ports are not supported on this platform"
            )
        if self._context is not None:
            connection = MidiOutputConnection._create(self, None)
            self._context.open("virtual", "", port_name, None)
            connection._generation = self._context.generation
            connection._closed = False
            return connection
        assert self._native is not None
        return MidiOutputConnection._create(
            self, self._native.create_virtual(port_name)
        )

    def __del__(self) -> None:
        if (context := getattr(self, "_context", None)) is not None:
            context.dispose()


class MidiInputConnection:
    __slots__ = (
        "_client",
        "_native",
        "_delivery",
        "_closed",
        "_generation",
        "__weakref__",
    )

    def __init__(self) -> None:
        raise TypeError("MIDI connection handles cannot be constructed directly")

    @classmethod
    def _create(
        cls,
        client: MidiInput,
        native: _native.MidiInputConnection | None,
        delivery: Delivery | None = None,
    ) -> MidiInputConnection:
        connection = object.__new__(cls)
        connection._client = client
        connection._native = native
        connection._delivery = delivery
        connection._closed = native is None and delivery is None
        connection._generation = (
            0 if client._context is None else client._context.generation
        )
        return connection

    @property
    def closed(self) -> bool:
        if self._native is not None:
            return self._native.closed
        assert self._client._context is not None
        return self._closed or bool(self._client._context.failure)

    @property
    def dropped_messages(self) -> int:
        return (
            0
            if self._delivery is None
            else self._delivery.dropped + self._delivery.worker_dropped
        )

    def close(self) -> None:
        _native.check_thread()
        if self._native is not None:
            self._native.close()
        elif not self._closed:
            assert self._client._context is not None and self._delivery is not None
            self._delivery.retire()
            try:
                self._client._context.close(self._generation)
                self._closed = True
            finally:
                self._delivery.bridge.drain()

    def __enter__(self) -> MidiInputConnection:
        if self._native is not None:
            self._native.__enter__()
        elif self.closed:
            raise RuntimeError("MIDI connection is closed or failed")
        return self

    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc_value: BaseException | None,
        traceback: TracebackType | None,
    ) -> None:
        close_context(self, exc_value)

    def __del__(self) -> None:
        if hasattr(self, "_delivery") and self._delivery is not None:
            self._delivery.retire()
            if not self._closed and self._client._context is not None:
                self._client._context.defer_close(self._generation)


class MidiOutputConnection:
    __slots__ = ("_client", "_native", "_closed", "_generation", "__weakref__")

    def __init__(self) -> None:
        raise TypeError("MIDI connection handles cannot be constructed directly")

    @classmethod
    def _create(
        cls, client: MidiOutput, native: _native.MidiOutputConnection | None
    ) -> MidiOutputConnection:
        connection = object.__new__(cls)
        connection._client = client
        connection._native = native
        connection._closed = native is None
        connection._generation = (
            0 if client._context is None else client._context.generation
        )
        return connection

    def send(self, message: bytes | bytearray | memoryview) -> None:
        _native.check_thread()
        if not isinstance(message, (bytes, bytearray, memoryview)):
            raise TypeError("MIDI messages must be bytes, bytearray, or memoryview")
        message = bytes(message)
        if self._native is not None:
            self._native.send(message)
        else:
            if self.closed:
                raise RuntimeError("MIDI connection is closed or failed")
            assert self._client._context is not None
            self._client._context.request(
                "send", [message, self._generation], serial=True
            )

    @property
    def closed(self) -> bool:
        if self._native is not None:
            return self._native.closed
        assert self._client._context is not None
        return self._closed or bool(self._client._context.failure)

    def close(self) -> None:
        _native.check_thread()
        if self._native is not None:
            self._native.close()
        elif not self._closed:
            assert self._client._context is not None
            self._client._context.close(self._generation)
            self._closed = True

    def __enter__(self) -> MidiOutputConnection:
        if self._native is not None:
            self._native.__enter__()
        elif self.closed:
            raise RuntimeError("MIDI connection is closed or failed")
        return self

    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc_value: BaseException | None,
        traceback: TracebackType | None,
    ) -> None:
        close_context(self, exc_value)

    def __del__(self) -> None:
        if hasattr(self, "_client") and self._native is None and not self._closed:
            assert self._client._context is not None
            self._client._context.defer_close(self._generation)


def close_context(
    connection: MidiInputConnection | MidiOutputConnection,
    body_error: BaseException | None,
) -> None:
    try:
        connection.close()
    except BaseException as close_error:
        # Preserve interrupts as well as ordinary exceptions from either phase.
        if body_error is None:
            raise
        raise BaseExceptionGroup(
            "MIDI context body and close both failed", [body_error, close_error]
        ) from None


def validate_settings(name: str, isolated: bool, timeout: float) -> None:
    _native.check_thread()
    if not isinstance(name, str):
        raise TypeError("MIDI client names must be strings")
    if not isinstance(isolated, bool):
        raise TypeError("isolated must be a boolean")
    if not isinstance(timeout, (int, float)) or isinstance(timeout, bool):
        raise TypeError("timeout must be a number")
    if not math.isfinite(timeout) or timeout <= 0:
        raise ValueError("timeout must be positive and finite")


def validate_open(
    name: str, callback: Callable[[int, bytes], object] | None = None
) -> None:
    _native.check_thread()
    if not isinstance(name, str):
        raise TypeError("MIDI connection names must be strings")
    if callback is not None and not callable(callback):
        raise TypeError("MIDI callback must be callable")
    if callback is not None:
        target = callback
        while isinstance(target, partial):
            target = target.func
        for c in (target, target.__call__):
            if (
                inspect.iscoroutinefunction(c)
                or inspect.isgeneratorfunction(c)
                or inspect.isasyncgenfunction(c)
            ):
                raise TypeError("MIDI callback must be synchronous and not a generator")
        signature_callback = callback
        # inspect.signature(instance) incorrectly removes an argument from a
        # static __call__. Inspect the actual callable in that case.
        if isinstance(
            inspect.getattr_static(type(target), "__call__", None), staticmethod
        ):
            signature_callback = target.__call__
            if isinstance(callback, partial):
                signature_callback = partial(
                    signature_callback, *callback.args, **callback.keywords
                )
        try:
            signature = inspect.signature(signature_callback)
        except (TypeError, ValueError):
            pass  # Some native callables do not publish a Python signature.
        else:
            try:
                signature.bind(0, b"")
            except TypeError as error:
                raise TypeError(
                    "MIDI callback must accept two positional arguments: "
                    "timestamp, message"
                ) from error
