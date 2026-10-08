"""Native midir bindings. Importing this module does not initialize MIDI."""

from collections.abc import Callable
from types import TracebackType

__version__: str

class MidiError(Exception):
    """A native MIDI operation failed."""

class InitError(MidiError):
    """The MIDI backend could not initialize."""

class PortInfoError(MidiError):
    """Port information could not be retrieved."""

class ConnectError(MidiError):
    """A connection could not be opened; the client remains available."""

class SendError(MidiError):
    """A MIDI message could not be sent."""

class MidiInputPort:
    """An opaque input-port handle returned by discovery."""

class MidiOutputPort:
    """An opaque output-port handle returned by discovery."""

class MidiInput:
    """An input client that cannot be used while its connection is open."""

    def __init__(self, client_name: str) -> None: ...
    def ports(self) -> list[MidiInputPort]: ...
    def port_name(self, port: MidiInputPort) -> str: ...
    def connect(
        self,
        port: MidiInputPort,
        port_name: str,
        callback: Callable[[int, bytes], object],
    ) -> MidiInputConnection: ...

class MidiOutput:
    """An output client that cannot be used while its connection is open."""

    def __init__(self, client_name: str) -> None: ...
    def ports(self) -> list[MidiOutputPort]: ...
    def port_name(self, port: MidiOutputPort) -> str: ...
    def connect(self, port: MidiOutputPort, port_name: str) -> MidiOutputConnection: ...

class MidiInputConnection:
    """An owned connection that restores its original client when closed."""

    @property
    def closed(self) -> bool: ...
    def close(self) -> None: ...
    def __enter__(self) -> MidiInputConnection: ...
    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc_value: BaseException | None,
        traceback: TracebackType | None,
    ) -> None: ...

class MidiOutputConnection:
    """An owned connection that restores its original client when closed."""

    @property
    def closed(self) -> bool: ...
    def close(self) -> None: ...
    def __enter__(self) -> MidiOutputConnection: ...
    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc_value: BaseException | None,
        traceback: TracebackType | None,
    ) -> None: ...
