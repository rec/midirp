# midirp

Typed Python bindings for [midir](https://github.com/Boddlnagg/midir), with raw
MIDI bytes, native timestamps, and explicit connection ownership. Import the
API from `midirp.midi`. There are no Python runtime dependencies.

The build matrix exercises standard, GIL-enabled CPython 3.11–3.14 on macOS
arm64/x86_64, Linux x86_64 (ALSA), and Windows x86_64 (WinMM). Build and unit
checks do not establish native MIDI support. See [validation](plan/validation.md)
for recorded results and outstanding native checks. Free-threaded Python,
subinterpreters, PyPy, musl, and other architectures are outside this matrix.

## Installation and building

The package has not been published. Build locally:

```sh
uv sync --frozen
uv run maturin build --release --locked --sdist --out dist
```

`--sdist` builds the wheel from the source archive, checking its completeness.
Install a CI or locally built wheel with `uv pip install /path/to/midirp.whl`
using an interpreter matching its CPython version and architecture. A wheel
installation needs no Rust compiler. Source builds require Rust 1.87 or later,
a C linker, and a supported Python interpreter. macOS requires Xcode Command
Line Tools; Windows requires Visual Studio C++ build tools and the MSVC Rust
target. Linux also requires `pkg-config` and ALSA development headers
(`libasound2-dev` on Debian/Ubuntu, `alsa-lib-devel` on Fedora).

Linux MIDI operations require a working ALSA sequencer, normally `/dev/snd/seq`,
and permission to access it. Containers need the device passed through.
Installing headers or importing a wheel does not provide a MIDI device.
Standard wheels use the ALSA backend and no optional midir features.

Cargo owns version `0.1.0`. The pinned build uses midir 0.11.0, PyO3 0.29.3,
and maturin 1.15.0. Wheels are interpreter-specific rather than abi3.

## Discover and select a port

```python
from midirp import midi

output = midi.MidiOutput("my application")
for port in output.ports():
    print(port.id(), output.port_name(port))
```

Choose a port's ID deliberately, then use `find_port_by_id(id)`. A missing ID
returns `None`; an empty port list is valid. Names need not be unique. Handles
support equality and are unhashable. IDs are opaque backend identifiers; there
is no extra persistence guarantee across disconnection or reboot. Input and
output handles are different types. Discovery can race with unplugging.
Finding a port or reading its name does not reserve it: either metadata lookup
or a later connect can fail if the device disappears. Handle errors from the
operation itself; another presence check cannot prevent this race.

```python
from midirp import midi

def play_note(port_id: str) -> None:
    output = midi.MidiOutput("note example")
    port = output.find_port_by_id(port_id)
    if port is None:
        raise LookupError(f"MIDI output is absent: {port_id}")
    with output.connect(port, "note connection") as connection:
        connection.send(b"\x90\x3c\x7f")
        connection.send(b"\x80\x3c\x00")
```

`send()` accepts immutable `bytes` only. Convert mutable buffers explicitly.
Bytes are forwarded unchanged; midir handles message validity. A native send
error leaves the connection open. Concurrent sends and close share a native
lock, with the GIL released; close waits for a send already in progress.
Competing threads have no guaranteed send order or priority over close.
Starting close does not cancel an active send. Use one controlling thread for
sending and closing when message order matters.

## Receive messages

```python
from queue import Queue
from midirp import midi

def receive_one(port_id: str) -> tuple[int, bytes]:
    messages: Queue[tuple[int, bytes]] = Queue()

    def receive(timestamp: int, message: bytes) -> None:
        messages.put((timestamp, message))

    source = midi.MidiInput("input example")
    source.ignore(midi.Ignore.TIME | midi.Ignore.ACTIVE_SENSE)
    port = source.find_port_by_id(port_id)
    if port is None:
        raise LookupError(f"MIDI input is absent: {port_id}")
    with source.connect(port, "input connection", receive):
        return messages.get(timeout=5)
```

The callback receives `(timestamp: int, message: bytes)` directly on the native
MIDI callback thread. Owned bytes remain valid after the callback returns.
Timestamps preserve midir's microsecond values and backend origin; do not compare
unrelated connections' clocks. Return values are ignored. Exceptions are reported
to `sys.unraisablehook` with the callable as context; later messages still arrive.

Initialize all state used by the callback before calling `connect()` or
`create_virtual()`. Delivery may start during native opening, before the call
returns and its result is assigned. The callback must not depend on the variable
receiving that connection. The queue examples initialize their callback state
before opening and hand messages to the controlling thread.

Explicit close stops accepting deliveries, rechecks delivery eligibility after
waiting for Python, and waits for already admitted deliveries to finish. An
already executing callback can continue while close waits; after close returns
successfully, no callbacks remain active or will be accepted for that connection.

Keep callbacks short. The GIL, OS scheduling, and backend buffers prevent hard
real-time guarantees or a promise of no loss under load. The example uses an
application-owned queue; the binding provides no buffering or queue API.
Blocking MIDI operations from a callback or its error hook raise `RuntimeError`
before entering the backend or waiting for a native lock. This includes client
creation, discovery, metadata, port IDs/comparisons, filter changes, opening,
sending, and closing, even on an unrelated connection. Hand those operations
to the controlling thread. Reading `closed` and manipulating `Ignore` flags
remain allowed because they do not wait for native work.

The guard cannot police application locks, queues, or calls into other libraries.
Do not wait for a thread that is closing input, or block on a full queue whose
consumer has stopped to close it. Keep consumers running until draining finishes,
and choose an explicit queue overflow policy.

The default filter is `Ignore.NONE`. Combine `SYSEX`, `TIME`, and `ACTIVE_SENSE`
with `|`. `Ignore.ALL` combines these three filters; note messages still arrive.
`Ignore(bits)` accepts integer masks 0–7; `int(flags)` returns the mask. Configure
filters before connecting. Configuration survives failed opens and close.

## Virtual ports

On macOS and Linux, a virtual input receives data from other clients, while a
virtual output sends data to other clients' inputs:

```python
from queue import Queue
from midirp import midi

def receive_virtual() -> tuple[int, bytes]:
    messages: Queue[tuple[int, bytes]] = Queue()

    def receive(timestamp: int, message: bytes) -> None:
        messages.put((timestamp, message))

    source = midi.MidiInput("virtual example")
    with source.create_virtual("example input", receive):
        # Another application can connect and send while this port is alive.
        return messages.get(timeout=5)
```

Use `output.create_virtual(port_name)` for virtual output, then `send(bytes)` on
its connection. A receiver must connect while the port is alive. Both methods
return the usual owned connection. Windows raises `NotImplementedError` before
changing client ownership. An external MIDI loopback driver must be installed
and selected separately if needed.

## Ownership, errors, and shutdown

A connection owns its native resource and keeps its original Python client alive.
While connecting, connected, or closing, that client rejects other operations.
Concurrent discovery, metadata, or configuration on the same client raises
`RuntimeError` immediately if another client operation is in progress.
Failed opens restore the same native client. `close()` is synchronous and
idempotent, restores the client for reuse, and waits for concurrent teardown.
`closed` is a nonblocking ownership-state snapshot. It remains false during
teardown and becomes true after client restoration; it does not report device
health. Context-manager exit closes the connection
and propagates body exceptions. Handles cannot be constructed directly.

Reading `closed` or entering a context manager does not reserve the connection
against another thread closing it immediately afterward. Call `send()` directly
and handle its exception rather than checking `closed` first as a precondition.
For a shared connection, coordinate its lifetime through the controlling thread;
a context manager does not prevent another owner from closing it.

| Failure | Exception |
| --- | --- |
| Backend initialization | `InitError` |
| Port metadata | `PortInfoError` |
| Connect or virtual creation | `ConnectError` |
| Sending | `SendError` |
| Wrong argument type/direction | `TypeError` |
| Unknown filter bits | `ValueError` |
| Busy/unavailable client, closed connection, blocking MIDI call from callback | `RuntimeError` |
| Caught native panic or subsequently failed resource | `RuntimeError` |
| Virtual ports on Windows | `NotImplementedError` |

The four native errors inherit from `MidiError` and retain upstream detail.
There is no automatic retry, reconnection, or promise of immediate unplug
detection. Handle device failures in the application.

Caught Rust panics in native client creation, discovery/configuration, port
metadata, connect, send, or close become `RuntimeError`. Panics during client
operations permanently disable that client; connect/send/close panics also
disable the connection. Create a fresh client explicitly. An ordinary returned
native error continues to use its usual exception and ownership behavior.
A failed send may still own a native handle: close attempts teardown, but reports
the failed state even if teardown completes. `closed` indicates consumed native
ownership, including after a teardown panic; it does not prove successful OS
cleanup. Subsequent close on a failed resource reports `RuntimeError`.

Cleanup logs recoverable failures to standard error and continues with other
jobs. If a failure leaves a native handle owned, it is retained until process
exit and keeps its capacity reservation. It is not blindly destroyed or retried.
Caught native-client destruction panics are also reported without unwinding
through Python destruction. These protections do not recover process aborts,
memory corruption, allocation failure that aborts the process, or panics across
non-unwinding OS callback boundaries. Rust's panic diagnostics may also appear
on standard error.

Native open, discovery, send, and close have no guaranteed completion deadline.
For example, WinMM can retry indefinitely while a driver stays busy. A callback
or error hook that never returns can also prevent close and interpreter shutdown.
Releasing the GIL allows other Python threads to run; it cannot cancel a hung
native call or stop arbitrary callback code. A timeout around a calling thread
does not cancel its operation. Applications requiring guaranteed recovery from
these failures need a separate process boundary.

Import starts four native cleanup workers for the main interpreter, without
initializing a MIDI backend. They only tear down resources; callbacks
remain on midir's thread. At most 32 connections may be opening, live, or
awaiting cleanup. An open beyond this capacity raises `RuntimeError` before
consuming its client. Explicitly closed connections release capacity; a queued
job keeps its reservation until a worker consumes it. Each resource is queued
at most once, so cleanup storage and registry scans remain bounded. A stalled
worker leaves the others available, but four stalled workers can still prevent
cleanup and shutdown.

Destruction and cyclic GC disable delivery immediately
and queue teardown without blocking Python. Client restoration is asynchronous
in that case; use explicit close when reuse must be immediate. Callback references
are visible to Python's collector.

An `atexit` handler rejects new registrations, disables delivery, drains active
callbacks and live resources with the GIL released, and joins the cleanup workers
before finalization. A callback that never returns can prevent clean shutdown.
Forced termination bypassing `atexit` does not run Python cleanup.
Subinterpreters are rejected. Starting a fresh interpreter is required for a
child process; forking with the worker or live MIDI resources is unsupported.

## Development and native validation

```sh
uv run --frozen pytest
uv run --frozen ruff check --select B,E,F,I python test
uv run --frozen ruff format --check python test
uv run --frozen ty check python/midirp test
cargo fmt --check
PYO3_PYTHON="$PWD/.venv/bin/python" cargo clippy --locked --all-targets -- -D warnings
PYO3_PYTHON="$PWD/.venv/bin/python" cargo test --locked
```

On Windows use `.venv/Scripts/python.exe` for `PYO3_PYTHON`. maturin chooses the
extension settings for wheels; Rust tests link Python explicitly. Unit tests
open no MIDI clients or devices and include deadline-bounded native-thread and
interpreter-finalization checks. Typing ships as `midi.pyi` and `py.typed`.

Native checks are opt-in and create temporary software endpoints:

```sh
uv run pytest test/manual/loopback.py -v
# Windows unsupported-virtual-path checks, without hardware connections:
uv run pytest test/manual/windows.py -v
```

Run these only with permission on the target host. CI's manual `coremidi` option
enables the virtual-port checks on macOS arm64. Linux needs a sequencer device;
Windows I/O needs an explicitly chosen external port. See the
[implementation plan](plan/plan.md), [validation record](plan/validation.md), and
[third-party notices](THIRD_PARTY_NOTICES.md). midirp is licensed under
[MIT](LICENSE). Publication requires a separate decision after the remaining
native and artifact validation gates are resolved.
