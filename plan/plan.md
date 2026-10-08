# midirp implementation plan

## Purpose and current state

Build a small, typed Python binding for the Rust `midir` library. Python callers
should be able to discover MIDI ports, receive timestamped raw MIDI messages,
send raw MIDI messages, and manage connections explicitly.

At the planning baseline, the repository contained only `README.md`, with no
API, build system, implementation, or test suite to preserve. This document
records the design and completion gates; the current status follows below.
Writing the plan itself did not authorize MIDI operation or package publication.

Research baseline: 2026-10-05, upstream midir tag `v0.11.0`. Check released PyO3
and maturin compatibility when implementation starts, then record the chosen
versions and Rust minimum version in the build configuration and lockfiles.

### Implementation status, 2026-10-08

Slices 1 through 6 are implemented. Slices 7 and 8 now have CI, opt-in native
fixtures, source/wheel checks, metadata, third-party notices, and usage docs.
CoreMIDI software-loopback checks passed, the owner selected MIT, and all 16
cross-platform CI combinations passed. Native ALSA/WinMM I/O evidence remains
an explicit release gate; hardware behavior has not been validated.
See [validation.md](validation.md) for current evidence and remaining gates.
The build uses midir 0.11.0, PyO3 0.29.3,
maturin 1.15.0, and Rust 1.87. Cargo is the authoritative version source.
The CPython 3.11 macOS arm64 wheel builds and imports outside the checkout,
with its stub and typing marker included.

The current API includes input/output constructors, port discovery and names,
input/output connect/close/context management, port IDs/equality/lookup,
output sending, input filtering, and virtual input/output creation. The shared
ownership core tests failed-connect restoration, client exclusion, reuse, and
concurrent teardown
without MIDI devices. Python tests cover the installed-package contract,
exception hierarchy, opaque-handle construction, and argument conversion.

The planned binding API is implemented. CPython 3.11–3.14 pass local unit checks;
CoreMIDI virtual I/O and shutdown pass on macOS arm64. Hardware operation and
native ALSA/WinMM lifecycle validation remain separate unverified gates.

Slice 3 supplied the direct-delivery bridge, now compiled into the extension by
slice 4. Callbacks receive owned bytes and unchanged timestamps on midir's
thread, with exceptions reported through `sys.unraisablehook`. Explicit close
rejects callback-thread callers, releases the GIL, and waits for active delivery
and native teardown. Both connection directions have context managers.

Slice 4 implements the approved architecture revision: one documented native
cleanup worker for the main interpreter, plus a weak native-resource registry
and an `atexit` handler. Automatic destruction and cyclic collection disable
delivery and queue teardown. They do not synchronously restore the client;
explicit close remains the way to wait for restoration. GC traversal visits the
single callable reference and each connection's Python client. Native Arc clones do not add
untracked Python references. No callback dispatcher or buffering API was added.

The registry includes pending opens. Per-resource synchronization prevents native
opening after teardown and lets shutdown wait for an open already in progress.
Shutdown rejects registration, disables all delivery, drains native resources
without the GIL, and joins the worker before finalization. Subinterpreters are
rejected. Standard GIL-enabled CPython remains the supported interpreter model.

The private native-thread driver uses the production callback and managed-resource
paths. Tests cover exact bytes/timestamps/order, ignored results, exception
reporting, callable release, forbidden callback-thread close, concurrent close,
ordinary destruction, cyclic collection, the last reference disappearing inside
a callback, and real CPython finalization with a live input. Isolated subprocess
checks also cover shutdown idempotence and rejection of new registrations. The
pytest runner bounds each native process and passes only required environment
variables. The driver is not included in the Python extension.

Source review of midir 0.11.0 found why callback-thread destruction needed the
worker: ALSA joins its handler thread, CoreMIDI close locks handler data, and
WinMM resets/stops/closes under a native handle lock. Unit tests validate the
binding lifecycle protocol, not those platform implementations. No MIDI clients
or devices have been opened during verification; native backend testing remains
required before claiming runtime support.

Slice 5 completes the existing discovery surface with opaque upstream `id()`,
equality between corresponding upstream handles, and `find_port_by_id(id)` with
`None` for a missing ID. Ports remain unhashable and direction-specific. Native
ID, equality, and lookup calls release the GIL; unavailable-client lookup follows
the existing state exclusion policy.

Output `send(message: bytes)` accepts only immutable Python bytes, forwards them
unchanged, and serializes native sends with close using the connection lock while
releasing the GIL. Closed connections raise `RuntimeError`. Native failures become
`SendError` with operation context and upstream text; they leave the connection
open. No additional MIDI validator or buffer-container overload was introduced.

Focused unit tests exercise the production byte boundary and ownership core for
argument rejection, unchanged bytes (including empty and invalid MIDI content),
error mapping, recovery after failed send, close during a send, and sending from
an input callback through a separate output. The pytest deadline now covers all
native unit tests as well as the isolated finalization checks. No device was
opened; upstream port identity/lookup and real sending await slice 7 validation.

Slice 6 exposes the immutable native `Ignore` value: `NONE`, `SYSEX`, `TIME`,
`ACTIVE_SENSE`, and `ALL`, with bitwise OR and equality. `Ignore(bits)` constructs
any of the eight upstream masks and rejects unknown bits with `ValueError`;
non-integer masks raise `TypeError`. `int(flags)` returns the upstream bits.
`MidiInput.ignore(flags)` requires an `Ignore` value and mutates only an available
native client under the existing state lock, with the GIL released. Native default
filtering remains `None`; configuration survives failed opens and close.

Unix virtual creation uses midir's `VirtualInput` and `VirtualOutput` traits.
Normal and virtual methods share one open path per direction for allocation,
callback validation, weak registry registration, native ownership transfer, and
cleanup. Windows methods raise `NotImplementedError` before taking native state.
Virtual input receives from other applications; virtual output sends to them.

Unit tests cover the eight masks, OR combinations without mutation, invalid bits
(including oversized integers), wrong mask/OR types, and configuration preservation
and state exclusion. No native virtual ports or devices were created. Actual
filtering, Unix loopback traffic, reopen, shutdown under traffic, and Windows
unsupported-path runtime checks remain explicitly authorized slice 7 validation.

## Additional work beyond the prompt

None.

## Proposed scope and defaults

These are proposed product decisions, not requirements supplied by the user:

- Distribution name: `midirp`, subject to package-index name availability.
- Use PyO3 for the extension and maturin as the Python build backend.
- Start with standard, GIL-enabled CPython 3.11 and later supported releases.
  Declare only the versions actually exercised by CI.
- Support the default desktop backends: CoreMIDI on macOS, ALSA on Linux, and
  WinMM on Windows. Start development on macOS; gate each platform's advertised
  support on its own build and runtime evidence.
- Preserve midir's callback-based input and raw-byte output model. There is one
  input-delivery API and one output API.
- Include virtual ports on macOS and Linux. Expose the same method on Windows,
  raising `NotImplementedError` before changing any object state.
- Preserve upstream message filtering, port identifiers, and timestamp units.
- Keep Python runtime dependencies empty. Resource handles are native classes,
  not Pydantic records; no configuration models or CLI are needed.

Exclude MIDI parsing, musical note abstractions, MIDI files, MIDI 2.0/UMP,
scheduled output, automatic reconnection, hotplug watchers, persistent device
configuration, message queues, asyncio, network transports, and databases.
Also exclude JACK, WinRT, mobile/browser targets, subinterpreters, PyPy, and
free-threaded Python from the initial support promise. These require separate
requirements and validation, not speculative hooks in the first implementation.

## Upstream contract to preserve

The tagged [common API source](https://github.com/Boddlnagg/midir/blob/v0.11.0/src/common.rs)
is the reference for binding behavior:

- `MidiInput` and `MidiOutput` create clients and enumerate direction-specific
  port handles. Names are display metadata, not unique keys.
- Port handles expose opaque identifiers through `id()`, and clients provide
  `find_port_by_id()`. Preserve upstream identity; do not substitute list indexes
  or invent a stronger guarantee across unplugging, reboot, or backend changes.
- Connecting consumes the Rust client. A failed connection returns the client
  inside `ConnectError`; closing a connection returns the client.
- Input callbacks receive a `u64` timestamp and a borrowed message slice. The
  timestamp is microseconds relative to an unspecified origin fixed for that
  connection. It is not wall-clock time or a shared clock across connections.
- Input ignores no messages by default. Preserve the filters in the tagged
  [Ignore definition](https://github.com/Boddlnagg/midir/blob/v0.11.0/src/lib.rs).
- Output takes a byte slice containing a valid MIDI message. Do not introduce a
  second MIDI validator or silently rewrite bytes.

Document Python adaptations explicitly: a Python closure replaces Rust's generic
callback userdata; connection close restores the original Python client and
returns `None`; repeated close is harmless.

## Package and source layout

Use a mixed package with a public native submodule, avoiding import re-exports
and handwritten wrapper classes that merely delegate to Rust:

```text
Cargo.toml
Cargo.lock
pyproject.toml
uv.lock
.python-version
src/
    lib.rs                 # Extension registration
    ports.rs               # Input/output port handles
    input.rs               # Input client, connection, callback bridge
    output.rs              # Output client and connection
    errors.rs              # Error translation
python/
    midirp/
        __init__.py        # Empty
        midi.pyi           # Public native API signatures and docstrings
        py.typed
test/
    test_contract.py
    test_lifecycle.py
    test_errors.py
    native/                # Rust-driven Python callback lifecycle fixtures
    manual/                # Explicitly invoked native-backend checks
plan/
    plan.md
.github/workflows/
    ci.yml
```

Configure maturin with `python-source = "python"` and
`module-name = "midirp.midi"`. Set the Rust module name accordingly and build a
`cdylib`. The canonical import is `from midirp.midi import MidiInput`.
Keep `__init__.py` empty and define native objects' Python module as
`midirp.midi`, so documentation and exceptions agree with imports.

Use the [maturin mixed-project and typing guidance](https://www.maturin.rs/project_layout.html).
Inspect the built wheel to verify the extension, stub, and `py.typed` are present.
Do not add a separate Python facade, C ABI, or ctypes/cffi implementation.

## Python API

All parameters and returns must appear in the stub. Connection constructors are
internal: only clients create connections. Port handles come from discovery,
not arbitrary strings or integers.

| Type | Public interface |
| --- | --- |
| `MidiInput` | `MidiInput(client_name: str)` |
| | `ports() -> list[MidiInputPort]` |
| | `port_name(port: MidiInputPort) -> str` |
| | `find_port_by_id(id: str) -> MidiInputPort \| None` |
| | `ignore(flags: Ignore) -> None` |
| | `connect(port: MidiInputPort, port_name: str, callback: Callable[[int, bytes], None]) -> MidiInputConnection` |
| | `create_virtual(port_name: str, callback: Callable[[int, bytes], None]) -> MidiInputConnection` |
| `MidiOutput` | `MidiOutput(client_name: str)` |
| | `ports() -> list[MidiOutputPort]` |
| | `port_name(port: MidiOutputPort) -> str` |
| | `find_port_by_id(id: str) -> MidiOutputPort \| None` |
| | `connect(port: MidiOutputPort, port_name: str) -> MidiOutputConnection` |
| | `create_virtual(port_name: str) -> MidiOutputConnection` |
| `MidiInputPort`, `MidiOutputPort` | `id() -> str`; equality using the corresponding upstream handle |
| `MidiInputConnection` | `close() -> None`, read-only `closed: bool`, context-manager methods |
| `MidiOutputConnection` | `send(message: bytes) -> None`, `close() -> None`, read-only `closed: bool`, context-manager methods |

Omit `port_count()` because `len(client.ports())` already expresses that operation.
Input and output ports are distinct types. Do not make ports hashable until
there is an upstream identity contract sufficient to support a consistent hash.
Do not offer name-based or index-based connection overloads.

Expose `Ignore` as a native bitmask value with `NONE`, `SYSEX`, `TIME`,
`ACTIVE_SENSE`, and `ALL`, and bitwise OR. The values correspond to upstream
bits; reject unknown bits at the boundary. This is not a string enum.
Default to `Ignore.NONE`, including delivery of timing and active-sensing traffic.
Explain that `ALL` combines these filters, not a filter for every MIDI message.

`send()` initially accepts immutable `bytes` only. Callers explicitly convert
other containers. Received messages are independently owned Python `bytes`,
safe to retain after the callback returns. Return values from callbacks are
ignored. Do not expose pointers, borrowed buffers, or zero-copy lifetimes.

## Ownership and connection state

Preserve one native client per Python client and one active connection per client.
Users construct additional clients for simultaneous connections.

1. A new client is `available` and owns its native midir client.
2. `connect()` or `create_virtual()` transitions it through `connecting`.
   Validate Python argument types before taking ownership of native state.
3. Success makes it `connected`; the connection owns the native connection and
   a strong reference to the Python client. The client does not own the Python
   connection. Client operations while unavailable raise `RuntimeError`.
4. Failure restores the same native client using `ConnectError::into_inner()`
   before raising a Python exception. Do not reconstruct it and lose filters.
5. Close transitions through `closing`, disables future callback dispatch,
   waits for in-flight work, and restores the native client to `available`.
6. A closed connection cannot reopen. Reuse the restored client to make a new
   connection. `send()` on a closed output raises `RuntimeError`.

`close()` is idempotent, including concurrent callers: a caller returning from
close must observe completed teardown, not merely that another thread started
it. Context exit calls close and does not suppress exceptions from the body.
Dropping an unconnected client releases its native client.

Use explicit state and Rust synchronization only where ownership and concurrent
access require it. midir objects are not universally `Sync`; verify target-specific
traits and use safe locking around `Send` resources. Never add unsafe `Send` or
`Sync` implementations to satisfy the binding compiler. Follow
[PyO3's thread-safety guidance](https://pyo3.rs/main/class/thread-safety).

## Callback delivery, shutdown, and the GIL

This is the first technical gate, before completing discovery or packaging.
Use direct callback delivery from midir's callback thread, without introducing
another dispatcher thread or a buffering policy.

- Retain an owned Python callable for the open connection. Attach to Python for
  callback execution using the selected PyO3 release's supported APIs. Never
  carry a borrowed Python reference across native callback invocations.
- Copy the incoming message into Python `bytes` during the callback, preserving
  byte content and integer timestamps exactly. Do not rescale timestamps.
- Invoke callbacks without holding client/connection locks or a mutable PyO3
  borrow. A callback may send through a separate output connection.
- Detach from Python for native close and other blocking operations, including
  waits for state locks. Never join a callback thread while holding the GIL it
  needs. Follow [PyO3's interpreter-lock guidance](https://pyo3.rs/main/doc/pyo3/marker/struct.python).
- Reject connection-closing operations from a MIDI callback thread before
  acquiring locks or changing state. Apply this to any connection, preventing
  mutual close between callback threads as well as self-join. Raise
  `RuntimeError` instructing the caller to close from its controlling thread.
- If a callback raises, report the exception through Python's unraisable-error
  mechanism with the callable as context, then continue receiving. Never unwind
  through the native backend or silently swallow the failure. The application
  can signal its controlling thread to close the connection.
- State that callbacks must be short and that Python/GIL scheduling prevents
  hard real-time guarantees. Do not claim bounded delivery latency or no loss
  under overload merely because the native library is described as realtime.

Automatic destruction needs separate proof from explicit close. Python callbacks
can capture the connection or client, creating cycles invisible to ordinary Rust
reference counting. Make all owned Python references visible to the collector
using [PyO3 GC integration](https://pyo3.rs/main/class/protocols.html#garbage-collector-integration).
Do not retain untracked duplicate strong Python references inside native closures.

The lifecycle prototype must demonstrate safe ordinary destruction, cyclic
collection, the last reference disappearing during a callback, and interpreter
shutdown with a live input. A callback-thread guard on explicit `close()` alone
does not solve native destruction on that thread. Finalization must stop dispatch
before Python teardown and must not attach to a finalized interpreter.

The approved slice 4 revision permits the documented cleanup worker and weak
shutdown registry described above. It preserves direct callback delivery. Do not
paper over a failed lifecycle proof with leaks, daemon threads, retries, or an
additional hidden global service. If direct callback delivery cannot meet these
requirements with the selected backends, stop implementation and present the
concrete failure and a revised ownership/delivery design for approval. A queued
input design changes the public contract and is not an automatic fallback.

## Error behavior

Use a small native exception hierarchy with `MidiError` as the base:

| Condition | Python behavior |
| --- | --- |
| Native initialization fails | `InitError(MidiError)` with upstream detail |
| Port metadata query fails | `PortInfoError(MidiError)` |
| Connection or virtual-port creation fails | `ConnectError(MidiError)` after restoring the client |
| Native send fails | `SendError(MidiError)` |
| Wrong argument or port direction | `TypeError` |
| Invalid binding bitmask | `ValueError` |
| Client unavailable, connection closed, or forbidden callback-thread close | `RuntimeError` |
| Virtual port unsupported on this platform | `NotImplementedError` |

Preserve meaningful native error text and operation context without parsing error
strings into invented structured codes. A missing identifier returns `None`;
an empty port list is successful discovery. Device disappearance can race with
discovery; propagate the eventual native error without retrying or reconnecting.
Do not promise immediate detection of unplugging when the backend does not provide
it. Input callback errors follow the reporting policy above, not this hierarchy.

## Implementation sequence and completion gates

The implementation is split into the eight slices agreed in discussion:

1. Build configuration, package layout, and import.
2. Client ownership, connection states, and error mapping. Include the minimal
   discovery and output connect/close surface needed to exercise ownership.
3. Callback bridge and explicit input shutdown.
4. Garbage collection and interpreter shutdown.
5. Complete port discovery, identifiers, and output sending.
6. Input filtering and virtual ports.
7. Native platform validation and CI.
8. Wheels, typing, documentation, and release checks.

The phases below group related slices. Phase 2's callback lifecycle proof belongs
to slices 3 and 4, not the ownership-only slice 2.

### 1. Establish the build and import contract

- Record the selected released midir, PyO3, and maturin versions and minimum Rust
  toolchain. Start from midir 0.11.0 unless review identifies a concrete blocker.
- Add Cargo configuration and a minimal extension using the layout above.
- Add Python metadata, maturin configuration, uv development dependencies,
  lockfiles, and narrow generated-output ignores. Keep uv metadata/lock changes
  in their own commit as required by repository instructions.
- Establish version metadata from one authoritative project version and ensure
  Cargo and wheel metadata agree. Do not add a release-management framework.
- Build and install a wheel into a clean environment; check the canonical import
  from outside the checkout and inspect distribution contents.

Gate: reproducible local build and import, correct module location and metadata,
and no MIDI backend initialization as a side effect of import.

### 2. Prove ownership and callback lifecycle

- Implement the minimal native state transitions, callback bridge, close path,
  and Python-reference traversal needed for the lifecycle experiments above.
- Use a private test-only native driver to invoke the real binding callback bridge
  on a native thread. Do not add a public fake backend or simulate OS MIDI APIs.
- Exercise close during a blocked callback, concurrent closes, reentrant sends,
  callback errors, GC cycles, and final-reference destruction.
- Put interpreter-exit scenarios in separate executable test files and run them
  as subprocesses with timeouts. Avoid Python programs embedded in strings.
- Review each supported backend's close/drop behavior before calling the design
  portable. Record unresolved platform limitations explicitly.

Gate: no hangs, use-after-free, lost Python exceptions, hidden reference cycles,
or attachment after finalization. Resolve failures before expanding the API.

### 3. Complete discovery and output

- Add native input/output port wrappers, identity, discovery, naming, and lookup.
- Implement output connect/send/close and input ignore configuration.
- Implement the error mapping and restore native state on connection failures.
- Add typed stubs alongside each implemented public method.

Gate: the documented state transitions and byte-conversion behavior have focused
tests; invalid state fails consistently without damaging resources.

### 4. Complete input and virtual ports

- Connect the proven callback bridge to real midir input connections.
- Implement Unix virtual input/output creation with the same ownership rules.
- Implement the explicit unsupported-platform behavior on Windows.
- Document virtual-port direction: a virtual input receives data from other
  clients; a virtual output sends data to other clients' inputs.

Gate: explicitly authorized loopback checks pass on available native platforms,
including filtering, SysEx, reopen, and shutdown under traffic. Record the tested
OS/backend combination; do not infer platform support from compilation alone.

### 5. Finish packaging, CI, and documentation

- Add usage documentation for discovery, output, input callbacks, context
  managers, virtual ports, filters, errors, and shutdown. Examples must close
  resources and must not assume the first enumerated device is the desired one.
- Add standard CI for Rust checks, binding unit tests, wheel builds, installed
  package imports, and typing checks. Keep device-dependent checks opt-in.
- Write Linux system dependency and device-access instructions and source-build
  prerequisites for each OS.
- Verify sdist-to-wheel builds, typing contents, license notices, and package
  metadata. Confirm the project's own license with the owner before publication;
  upstream midir's MIT license does not choose a license for this repository.

Gate: release artifacts can be installed and imported on their advertised
platforms, examples match the implemented API, and remaining runtime limitations
are stated. Package publication is a separate explicitly authorized operation.

## Verification strategy

### Tests without MIDI devices

Test binding logic, not external MIDI services:

- Python type conversion, direction-specific handles, filter combinations, and
  immutable retained message bytes.
- Native error translation and ownership restoration using a narrow test-only
  seam around state transitions, not a parallel MIDI implementation.
- Available/connecting/connected/closing/closed behavior, repeated close, and
  context-manager exception propagation.
- Delivery with a native-thread test driver: exact bytes and timestamps, raising
  callbacks, concurrent teardown, GC, and interpreter exit.
- Stubs, wheel contents, and clean-environment imports as packaging checks.

Use `pytest` for Python tests and Rust's built-in harness for Rust logic. Keep
tests focused on observable behavior. Use `pytest-regressions` for larger fixture
values when needed rather than introducing another snapshot framework. MIDI
messages are not digital audio; these tests do not need WAV fixtures.

### Explicit native-backend validation

These are manual/opt-in integration checks, not default unit tests and not
authorized to run by this planning task:

- macOS CoreMIDI and Linux ALSA: create virtual endpoints and connect a second
  client; exchange known note, control-change, realtime, and SysEx bytes.
- Windows WinMM: use an explicitly selected hardware or installed loopback device;
  the library does not install a virtual MIDI driver.
- Verify default filters and each relevant filter combination, distinct duplicate
  port names, stale-port failure, repeated open/close, and teardown under traffic.
- Check timestamp units and ordering as supported by the backend, without assuming
  a zero origin or comparing unrelated connections' clocks.
- Measure representative callback throughput/latency and SysEx sizes, recording
  environment and load. Do not invent universal performance thresholds.
- Exercise unplug/replug and driver errors manually; report backend behavior
  separately from binding defects.

### Developer checks for future implementation

Use uv for Python tooling and Cargo for Rust tooling. Before implementation
commits, run the applicable repository-required pytest, Ruff fix/format, `ty`,
and pyupgrade checks on actual Python/stub/test paths, plus `cargo fmt --check`,
`cargo clippy --all-targets -- -D warnings`, and `cargo test` for native changes.
Adapt the supplied instructions' `recs` path to this repository's actual layout;
do not run commands against unrelated sibling projects. Configure native test
linking explicitly if PyO3's extension build mode conflicts with Rust test binaries.

Run `git diff --check` and review the final scoped diff. Do not run Python checks
for documentation-only changes. Every implementation commit must be independently
valid, committed on the current branch, and pushed according to repository policy.
No application launch, device traffic, or broad runtime flow is implied by a unit
test command.

## Distribution and platform matrix

Start with interpreter-specific wheels to minimize ABI and build-policy choices.
Consider abi3 only after the lifecycle and platform gates pass and there is a
measured distribution benefit. Follow the released versions of
[maturin's binding configuration](https://www.maturin.rs/bindings.html) and
[build configuration](https://www.maturin.rs/config).

| Target | Initial backend | Artifact validation |
| --- | --- | --- |
| macOS arm64 and x86_64 | CoreMIDI | Separate architecture wheels; validate deployment targets and framework linkage |
| Linux x86_64 | ALSA | Choose and test a manylinux baseline; audit/repair native dependencies and document runtime requirements |
| Windows x86_64 | WinMM | Native wheel; validate import and Windows callback teardown |

Linux source builds require ALSA development libraries and `pkg-config`; users
also need access to the ALSA sequencer for MIDI operations. A CI container with no
sequencer device can validate imports and unit logic but cannot prove MIDI I/O.
Audit `libasound` dependency treatment in the wheel rather than assuming that a
successful build implies a portable wheel.

Do not claim Linux arm64, musl, Windows arm64, or universal2 support until requested
and tested. Do not enable optional backend Cargo features in standard wheels.
Select a finite tested CPython matrix during build setup and reject unsupported
build modes clearly rather than silently publishing unvalidated artifacts.

## Release acceptance checklist

- [x] Public API and typing match the proposal or documented approved revisions.
- [x] Native ownership is restored after close and failed connect.
- [x] Received bytes remain valid after callbacks return; timestamps are preserved.
- [x] Callback exceptions are visible and cannot unwind into the backend.
- [x] Explicit close, concurrent close, GC, and interpreter shutdown pass lifecycle tests.
- [x] No Python delivery starts after completed close (native-thread driver).
- [ ] Each advertised backend has recorded native runtime validation.
- [x] Local wheel and sdist checks include typing; wheel installs need no Rust compiler.
- [x] Linux wheel dependencies and the local macOS deployment target are verified.
- [x] No duplicate delivery API, unsolicited dependencies, or hidden worker service.
- [x] Documentation states GIL, timing, device-disconnection, and platform limits.
- [x] Repository checks pass and changes are committed and pushed in coherent units.
- [ ] Package name, project license, and publication authorization are resolved before release.
