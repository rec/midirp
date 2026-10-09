"""Native midir bindings. Importing this module does not initialize MIDI."""

from collections.abc import Callable
from types import TracebackType
from typing import ClassVar

__version__: str

class MidiError(Exception):
    """A native MIDI operation failed."""

class InitError(MidiError):
    """The MIDI backend could not initialize."""

class PortInfoError(MidiError):
    """Port information could not be retrieved."""

class ConnectError(MidiError):
    """Failed native opens disable isolated and ALSA clients."""

class SendError(MidiError):
    """A MIDI message could not be sent."""

class StateError(RuntimeError):
    """MIDI ownership state prevents this operation."""

class CallbackThreadError(RuntimeError):
    """A blocking MIDI operation was attempted from a callback."""

class NativePanicError(RuntimeError):
    """A recoverable native MIDI panic occurred."""

class ResourceError(RuntimeError):
    """MIDI lifecycle resources could not be allocated."""

class WorkerError(RuntimeError):
    """The MIDI worker exited or communication failed."""

class WorkerTimeoutError(TimeoutError):
    """A MIDI worker operation exceeded its deadline."""

class Ignore:
    """Immutable upstream input-filter bits. ALL combines three filters."""

    NONE: ClassVar[Ignore]
    SYSEX: ClassVar[Ignore]
    TIME: ClassVar[Ignore]
    ACTIVE_SENSE: ClassVar[Ignore]
    ALL: ClassVar[Ignore]

    def __init__(self, bits: int) -> None: ...
    def __or__(self, other: Ignore) -> Ignore: ...
    def __int__(self) -> int: ...
    def __repr__(self) -> str: ...
    def __eq__(self, other: object) -> bool: ...
    def __ne__(self, other: object) -> bool: ...

class MidiInputPort:
    """An opaque, unhashable input-port handle returned by discovery."""

    __hash__: ClassVar[None]

    def id(self) -> str: ...
    def __eq__(self, other: object) -> bool: ...
    def __ne__(self, other: object) -> bool: ...

class MidiOutputPort:
    """An opaque, unhashable output-port handle returned by discovery."""

    __hash__: ClassVar[None]

    def id(self) -> str: ...
    def __eq__(self, other: object) -> bool: ...
    def __ne__(self, other: object) -> bool: ...

class MidiInput:
    """An input client that cannot be used while its connection is open."""

    def __init__(
        self,
        client_name: str,
        *,
        isolated: bool = True,
        timeout: float = 5.0,
        receive_byte_limit: int = 8_388_608,
    ) -> None: ...
    def ports(self) -> list[MidiInputPort]: ...
    @property
    def worker_failure(self) -> Exception | None:
        """Known isolated-worker failure without a traceback; otherwise None."""
        ...
    def port_name(self, port: MidiInputPort) -> str: ...
    def find_port_by_id(self, id: str) -> MidiInputPort | None:
        """Missing ID returns None.

        Ambiguous or unavailable CoreMIDI IDs raise PortInfoError.
        """
        ...
    def ignore(self, flags: Ignore) -> None: ...
    def connect(
        self,
        port: MidiInputPort,
        port_name: str,
        callback: Callable[[int, bytes], object],
    ) -> MidiInputConnection:
        """Use a synchronous, nongenerator callback.

        Callbacks may run before return; initialize their state before opening.
        """
        ...
    def create_virtual(
        self, port_name: str, callback: Callable[[int, bytes], object]
    ) -> MidiInputConnection:
        """Use a synchronous, nongenerator callback.

        Callbacks may run before return; initialize their state before opening.
        """
        ...

class MidiOutput:
    """An output client that cannot be used while its connection is open."""

    def __init__(
        self, client_name: str, *, isolated: bool = True, timeout: float = 5.0
    ) -> None: ...
    def ports(self) -> list[MidiOutputPort]: ...
    @property
    def worker_failure(self) -> Exception | None:
        """Known isolated-worker failure without a traceback; otherwise None."""
        ...
    def port_name(self, port: MidiOutputPort) -> str: ...
    def find_port_by_id(self, id: str) -> MidiOutputPort | None:
        """Missing ID returns None.

        Ambiguous or unavailable CoreMIDI IDs raise PortInfoError.
        """
        ...
    def connect(self, port: MidiOutputPort, port_name: str) -> MidiOutputConnection: ...
    def create_virtual(self, port_name: str) -> MidiOutputConnection: ...

class MidiInputConnection:
    """Successful close restores the client; isolated native failures disable it."""

    @property
    def worker_failure(self) -> Exception | None:
        """Known failure of this client's isolated worker; not device health."""
        ...
    @property
    def last_message_time(self) -> float | None:
        """Python monotonic seconds at last callback start, or None before delivery."""
        ...
    @property
    def dropped_messages(self) -> int:
        """Queue overflow count in isolated mode; zero in native mode."""
        ...
    @property
    def closed(self) -> bool:
        """A momentary ownership snapshot; does not reserve the connection."""
        ...
    def close(self) -> None:
        """Stop accepting delivery and wait for admitted callbacks and teardown."""
        ...
    def __enter__(self) -> MidiInputConnection:
        """Check current state; another thread can still close the connection."""
        ...
    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc_value: BaseException | None,
        traceback: TracebackType | None,
    ) -> None: ...

class MidiOutputConnection:
    """Successful close restores the client; isolated native failures disable it."""

    def send(self, message: bytes | bytearray | memoryview) -> None:
        """Serialized with send/close; competing threads have no guaranteed order."""
        ...
    @property
    def worker_failure(self) -> Exception | None:
        """Known failure of this client's isolated worker; not device health."""
        ...
    @property
    def closed(self) -> bool:
        """A momentary ownership snapshot; does not reserve the connection."""
        ...
    def close(self) -> None:
        """Wait for teardown; cannot cancel a native send already in progress."""
        ...
    def __enter__(self) -> MidiOutputConnection:
        """Check current state; another thread can still close the connection."""
        ...
    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc_value: BaseException | None,
        traceback: TracebackType | None,
    ) -> None: ...
