# midirp

Typed Python bindings for [midir](https://github.com/Boddlnagg/midir), with raw
MIDI bytes, native timestamps, and explicit connection ownership. Import the
API from `midirp.midi`. There are no Python runtime dependencies.

The build matrix exercises standard, GIL-enabled CPython 3.11–3.14 on macOS
arm64/x86_64, Linux x86_64 (ALSA), and Windows x86_64 (WinMM). Build and unit
checks do not establish native MIDI support. See [validation](plan/validation.md)
for recorded results and outstanding native checks. Free-threaded Python builds
and subinterpreters are rejected on import. PyPy, musl, and other architectures
are outside this matrix.

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

## Process isolation

Each client defaults to a fresh MIDI child process:

```python
from midirp import midi

output = midi.MidiOutput("my application", isolated=True, timeout=5.0)
```

There is one process per client, which can own one live connection. Input and
output on the same physical device use separate processes. The deadline covers
native operations and their pipe communication. A timeout raises `TimeoutError`;
a crash raises `RuntimeError`. Either failure, or any returned native error,
invalidates that client and connection and terminates its worker. Create a new
client and rediscover deliberately. Sends and connections are never retried.
An idle input does not trigger a timeout or establish device health.

Set `isolated=False` on either constructor for native MIDI in the current process.
This saves child interpreters, communication threads, copies, and scheduling
overhead. Native operations then have no enforceable deadline; `timeout` does
not cancel them. Isolation adds startup cost and receive latency. It contains a
worker failure but cannot repair a shared OS MIDI service or driver.
Workers use local pipes and a fresh interpreter; no multiprocessing main guard,
network service, or picklable user callback is required. Frozen/embedded
interpreters have not been validated.

## Discover and select a port

```python
from midirp import midi

output = midi.MidiOutput("my application")
for port in output.ports():
    print(port.id(), output.port_name(port))
```

Choose a port's ID deliberately, then use `find_port_by_id(id)`. A missing ID
returns `None`; duplicate matching IDs raise `PortInfoError` in either mode.
CoreMIDI ID `"0"` raises `PortInfoError` because it indicates an unavailable ID.
An empty port list is valid. Names need not be unique. Handles
support equality and are unhashable. IDs are opaque backend identifiers; there
is no extra persistence guarantee across disconnection or reboot. Input and
output handles are different types. Discovery can race with unplugging.
Finding a port or reading its name does not reserve it: either metadata lookup
or a later connect can fail if the device disappears. Handle errors from the
operation itself; another presence check cannot prevent this race.
Isolated handles cache opaque ID snapshots; equality compares IDs within the
same direction. Opening resolves the ID again and rejects absent or ambiguous
matches. Native-mode handles retain midir's original equality behavior.

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

`send()` accepts `bytes`, `bytearray`, and `memoryview`. Mutable buffers and views
are copied into owned bytes before native work or waiting for another operation.
Strided views send their logical byte contents. A released view raises `ValueError`.
Bytes are forwarded unchanged; midir handles message validity. A native send
error invalidates an isolated worker; in-process ordinary send errors leave the
connection open. Sends and close are serialized. In-process native waits release
the GIL; isolated waits use the configured deadline.
Competing threads have no guaranteed send order or priority over close.
Starting close does not cancel an active send. Use one controlling thread for
sending and closing when message order matters.

## Receive messages

```python
from queue import Full, Queue
from midirp import midi

def receive_one(port_id: str) -> tuple[int, bytes]:
    messages: Queue[tuple[int, bytes]] = Queue(maxsize=1)

    def receive(timestamp: int, message: bytes) -> None:
        try:
            messages.put_nowait((timestamp, message))
        except Full:
            pass  # Only the first waiting message is needed.

    source = midi.MidiInput("input example")
    source.ignore(midi.Ignore.TIME | midi.Ignore.ACTIVE_SENSE)
    port = source.find_port_by_id(port_id)
    if port is None:
        raise LookupError(f"MIDI input is absent: {port_id}")
    with source.connect(port, "input connection", receive):
        return messages.get(timeout=5)
```

The callback receives `(timestamp: int, message: bytes)` on a parent dispatch
thread in isolated mode, or on the native MIDI thread in in-process mode.
Owned bytes remain valid after the callback returns.
Timestamps preserve midir's microsecond values and backend origin; do not compare
unrelated connections' clocks. Return values are ignored. Exceptions are reported
to `sys.unraisablehook` with the callable as context; later messages still arrive.

Callbacks must execute synchronously. Opening rejects coroutine functions,
generator functions, and async generator functions with `TypeError`, including
partials and callable objects whose `__call__` has those forms. The library does
not await or iterate callback results. A synchronous wrapper that returns a
coroutine or generator cannot be identified reliably at open; its result is
still ignored. Use a synchronous callback to hand work to your event loop.
If a callback's signature can be inspected, opening checks that it accepts two
positional arguments. Incompatible signatures raise `TypeError` before opening;
uninspectable callables remain accepted without executing them to check.

Initialize all state used by the callback before calling `connect()` or
`create_virtual()`. Delivery may start during native opening, before the call
returns and its result is assigned. The callback must not depend on the variable
receiving that connection. The queue examples initialize their callback state
before opening and hand messages to the controlling thread. Each example waits
for one message, keeps at most one queued message, and discards new messages
while that slot is occupied. Its callback never waits for queue capacity.

Explicit close stops accepting deliveries, rechecks delivery eligibility after
waiting for Python, and waits for already admitted deliveries to finish. An
already executing callback can continue while close waits; after close returns
successfully, no callbacks remain active or will be accepted for that connection.

Keep callbacks short. The GIL, OS scheduling, and backend buffers prevent hard
real-time guarantees or a promise of no loss under load. The example uses an
application-owned queue. Isolated delivery has two bounded queues, one in each
process, each holding at most 128 messages and 8 MiB of message payloads by
default. Configure `MidiInput(..., receive_byte_limit=8 * 1024 * 1024)` to set
each stage's positive byte budget. A message that would exceed either limit is
dropped, including a single message larger than the entire byte budget.
Read `connection.dropped_messages` for their combined overflow count. Child
counts arrive with messages/control replies and are current after successful
close; a crashed worker can lose its final count. This counts queue overflow,
not driver loss or messages discarded during close. These budgets bound queued
payloads, not Python overhead, messages in flight, user-retained messages, or
unfinished SysEx inside midir. There is no hard throughput guarantee. Native
mode reports zero, adds no receive queue, and does not apply `receive_byte_limit`.
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
filters before connecting. Configuration survives successful close. Failed native
opens disable isolated and ALSA clients, so configure a fresh client after failure.
In-process CoreMIDI/WinMM ordinary returned errors preserve configuration subject
to upstream rollback defects.

## Virtual ports

On macOS and Linux, a virtual input receives data from other clients, while a
virtual output sends data to other clients' inputs:

```python
from queue import Full, Queue
from midirp import midi

def receive_virtual() -> tuple[int, bytes]:
    messages: Queue[tuple[int, bytes]] = Queue(maxsize=1)

    def receive(timestamp: int, message: bytes) -> None:
        try:
            messages.put_nowait((timestamp, message))
        except Full:
            pass  # Only the first waiting message is needed.

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
Failed native opens invalidate isolated clients and in-process ALSA clients.
ALSA's returned client is disposed before the opening call raises `ConnectError`,
including partial queues/ports; subsequent client operations raise `RuntimeError`
requiring a new client. In-process CoreMIDI/WinMM ordinary returned errors restore
the same client, subject to upstream rollback defects. Native cleanup can still
stall in in-process mode; its timeout does not cancel a driver.
Successful `close()` is synchronous and idempotent, restores the client for
reuse, and waits for concurrent teardown.
`closed` is a nonblocking ownership-state snapshot. It remains false during
teardown and becomes true after client restoration; it does not report device
health. An isolated worker failure also makes `closed` true.
Clients and connections expose `worker_failure`: a detached exception describing
the first known worker failure, or `None` when none is recorded. It preserves
the failure's type and arguments without retaining its traceback. In-process
mode has no worker and always reports `None`; this is not a device-health test.
Input connections expose `last_message_time`, the `time.monotonic()` value in
seconds when the latest callback delivery started, or `None` before delivery.
It is recorded before application callback code and remains available after
close. Queued or dropped messages do not advance it. This clock differs from
the native microsecond timestamp passed to the callback. Neither silence nor
a successful operation proves that a device is responsive.
Context-manager exit closes the connection and propagates body exceptions.
If both the body and close fail, it raises an `ExceptionGroup` containing the
body error first and the close error second. Interrupts produce a
`BaseExceptionGroup` instead. A close error without a body error is raised
directly. Handles cannot be constructed directly.

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
| Busy/unavailable client, closed or failed resource, interpreter shutdown | `StateError` |
| Blocking MIDI call from callback or error hook | `CallbackThreadError` |
| Caught native panic | `NativePanicError` |
| Cleanup capacity or cleanup/communication thread startup failure | `ResourceError` |
| Isolated worker exit or pipe failure | `WorkerError` |
| Virtual ports on Windows | `NotImplementedError` |
| Isolated operation exceeds its configured deadline | `WorkerTimeoutError` |

The four native errors inherit from `MidiError` and retain upstream detail.
The named binding errors inherit from `RuntimeError`, except `WorkerTimeoutError`
which inherits from `TimeoutError`. Catch their types to distinguish binding
failure categories; diagnostic strings are not a stable machine-readable API.
Native error details remain backend-specific; the library does not infer a
driver diagnosis from those strings.
There is no automatic retry, reconnection, or promise of immediate unplug
detection. Handle device failures in the application.

Caught Rust panics in native client creation, discovery/configuration, port
metadata, connect, send, or close become `RuntimeError`. Panics during client
operations permanently disable that client; connect/send/close panics also
disable the connection. Create a fresh client explicitly. An ordinary returned
native error keeps its exception class; isolated mode additionally disables the
client and stops its child. The following native ownership details apply to
in-process mode.
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

In-process native open, discovery, send, and close have no guaranteed deadline.
For example, WinMM can retry indefinitely while a driver stays busy. Isolated
native operations time out and terminate their child. A parent callback or error
hook that never returns can still prevent explicit close and interpreter shutdown.
The deadline cannot cancel arbitrary Python code or a C extension retaining the
GIL. User callback draining has no timeout, in either mode.

Import starts four native cleanup workers in the main interpreter without
initializing MIDI. Each isolated child also imports its native cleanup pool.
In-process mode permits at most 32 connections opening, live, or awaiting
cleanup; stalled teardown keeps its reservation. Isolated mode starts a process
for each client with its own pool and one connection, without a global client
quota. Interpreter memory, threads, pipes, and native resources grow with client
count. Resource exhaustion can reject startup or terminate a worker.

Destruction and cyclic GC retire delivery and schedule teardown without waiting
for a native driver. Client restoration is asynchronous; use explicit close for
immediate reuse. Dropping an isolated client schedules graceful worker shutdown,
with forced termination on timeout. Worker exit reclaims its retained CoreMIDI
clients; in-process CoreMIDI clients remain retained upstream until process exit.
Callback references are visible to Python's collector. Virtual endpoints disappear
when teardown completes, which can affect applications connected to them.
Closing does not send all-notes-off or restore physical device state.

At interpreter exit, isolated delivery is retired, workers receive shutdown,
and native workers exceeding the deadline are terminated and reaped. Admitted
parent callbacks are then drained. In-process native cleanup still drains and
joins its workers with the GIL released and can hang on drivers or callbacks.
Exit-handler ordering and callback application locks remain the application's
responsibility. Forced parent termination bypasses these handlers; graceful
child cleanup is then not guaranteed. Subinterpreters are rejected. Forking
with imported MIDI runtime state or live resources is unsupported; use a fresh
interpreter. Standard GIL-enabled CPython is the tested contract.

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

Run these only with permission on the target host. CI runs only when a GitHub
release is published and excludes native device checks. Linux needs a sequencer device;
Windows I/O needs an explicitly chosen external port. See the
[implementation plan](plan/plan.md), [validation record](plan/validation.md), and
[third-party notices](THIRD_PARTY_NOTICES.md). midirp is licensed under
[MIT](LICENSE). Publication requires a separate decision after the remaining
native and artifact validation gates are resolved.
