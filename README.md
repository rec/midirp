# Python bindings for the midir Rust MIDI library

This project is being implemented according to [plan/plan.md](plan/plan.md).
It targets standard, GIL-enabled CPython 3.11 and later. Native MIDI operation
on each platform will be validated separately before claiming support.

## Development

The build uses midir 0.11.0, PyO3 0.29.3, maturin 1.15.0, and Rust 1.87 or
later. Cargo owns the package version; maturin uses it for Python metadata.
Python runtime dependencies are empty.

```sh
uv sync
uv run pytest
uv run maturin build --release --out dist
```

The canonical native module is `midirp.midi`. Importing it does not initialize
the MIDI backend. The package includes type stubs and a `py.typed` marker.

Native unit tests link to Python without the extension-module build setting:

```sh
PYO3_PYTHON="$PWD/.venv/bin/python" cargo test
PYO3_PYTHON="$PWD/.venv/bin/python" cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

maturin selects the extension-module build setting for wheel builds.
Linux source builds also require ALSA development libraries and `pkg-config`.
No MIDI clients or devices are opened by the unit tests.

## Current implementation

The first six slices provide the planned MIDI API and callback lifecycle:

- Input and output clients can enumerate ports, retrieve their names, and find
  ports with `find_port_by_id(id)`, returning `None` when absent.
- Port handles provide `id()` and equality using the upstream handle. Input and
  output handles are distinct and remain unhashable. IDs are opaque backend
  identifiers; no extra persistence guarantee across unplugging or reboot is
  added. Ports remain independent of their discovering client's connection state.
- An output client can connect to a discovered output port. The connection owns
  the native resource and keeps the original Python client alive.
- While connecting, connected, or closing, the client rejects other operations
  with `RuntimeError`. Failed connection attempts restore the same native client.
- `close()` restores the original client for reuse and is idempotent. Concurrent
  close calls wait for teardown to finish. Output connections support context
  managers. Input connections have the same close and context-manager contract.
- Native failures use `MidiError` subclasses: `InitError`, `PortInfoError`,
  `ConnectError`, and `SendError`. Invalid Python argument types raise `TypeError`.

Output connections provide `send(message: bytes)`. Bytes are passed unchanged to
midir, which handles MIDI message validity. Other containers, including
`bytearray` and `memoryview`, raise `TypeError`; convert them explicitly to
`bytes`. Native failures raise `SendError` with upstream detail and leave the
connection open. Sending on a closed connection raises `RuntimeError`.
Concurrent sends and close share the native resource lock with the GIL released;
close waits for an already-running send. An input callback may send through a
separate output connection.

Input clients provide `ignore(flags: Ignore)`. The default is `Ignore.NONE`.
Combine `Ignore.SYSEX`, `Ignore.TIME`, and `Ignore.ACTIVE_SENSE` with `|`;
`Ignore.ALL` combines those three filters, so other MIDI messages still arrive.
`Ignore(bits)` accepts integer masks from 0 through 7; unknown bits raise
`ValueError`, and `int(flags)` returns the mask. Set filters before connecting;
changing them while the client is unavailable raises `RuntimeError`. Filter
configuration survives failed connection attempts and close.

On macOS and Linux, `MidiInput.create_virtual(port_name, callback)` creates an
input that receives messages sent by other applications. Likewise,
`MidiOutput.create_virtual(port_name)` creates an output that sends messages to
applications connected to it. Both return the usual owned connection with the
same close, context-manager, GC, and shutdown behavior. A failed creation restores
the original client and raises `ConnectError`. On Windows both methods raise
`NotImplementedError` before changing native client state. Virtual input validates
its callable before consuming native ownership on supported platforms.

Port and connection handles cannot be constructed directly. Context-manager exit
closes the connection and propagates body errors.

Ownership transitions are tested without MIDI devices. Native discovery,
lookup, sending, filtering, virtual creation, connection, and destruction against
actual OS backends have not been exercised.
The build and installed wheel have been verified on macOS arm64 with CPython
3.11. Broader interpreter and platform validation remains later work.

Input clients now support `connect(port, port_name, callback)`. The callback
receives `(timestamp: int, message: bytes)` directly on the native MIDI callback
thread. Bytes are copied, timestamps are unchanged, and return values are ignored.
Exceptions go to `sys.unraisablehook` with the callable as context; subsequent
messages still arrive. Keep callbacks short; Python scheduling does not provide
hard real-time guarantees. Calling any connection's `close()` from a MIDI
callback or its error hook raises `RuntimeError`; signal the controlling thread
to close it instead.

Each main interpreter has one documented native cleanup worker, created when the
extension is imported, and a registry of weak native-resource references. The
worker does not deliver callbacks. Ordinary destruction and cyclic collection
disable delivery immediately and queue teardown without blocking Python or
joining the callback's own thread. Client reuse after automatic destruction must
wait for cleanup to finish; use explicit `close()` for synchronous restoration.
The connection's single callable reference is visible to Python's collector;
native closures do not duplicate that Python ownership.

An `atexit` handler stops registration, disables all input delivery, drains active
callbacks and connections with the GIL released, and joins the cleanup worker
before interpreter finalization. Subinterpreters are explicitly rejected.
Normal interpreter exit is covered; process termination that bypasses `atexit`
does not run Python cleanup. The native-thread unit driver exercises this same
bridge and resource lifecycle without opening MIDI devices. Tests include
callback-thread destruction, cycles, concurrent close, pending opens, and actual
CPython finalization in separate processes with deadlines. These tests do not
establish platform backend or physical-device shutdown safety.

Slice 5 unit checks cover strict immutable-byte arguments, unchanged forwarding,
native send-error translation, continued use after failure, closed-state errors,
send/close concurrency, and sending from a native input callback. The private
output driver records calls through the production byte boundary and ownership
core; it does not create or simulate an OS MIDI backend. Port identity and lookup
use midir directly and await native-backend validation.

Slice 6 checks cover all eight filter masks, combinations, unknown-bit and wrong-
type rejection, and preservation of configured filters through the ownership
transitions. Normal and virtual connections share their allocation, registration,
and native-open path. Virtual loopback and Windows unsupported-path runtime checks
remain part of slice 7; successful compilation is not native MIDI validation.
