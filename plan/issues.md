# Issues and failure review

## Scope and evidence

Source review dated 2026-10-08, against binding commit `b7f50d0`, midir
0.11.0, PyO3 0.29.3, and the transitive CoreMIDI wrapper coremidi 0.9.2.
This is an issue inventory and proposed investigation plan, not a claim that
every failure below has been reproduced. Later fixes and authorized checks
are reflected below; backend defects remain unless explicitly resolved.
Open findings are updated as fixes land; resolved issues are removed.
Remove resolved issue text in the last commit of its fix series.

Preparation of the four upstream reports B3–B6 is complete. They have been
transferred to [Suggestions for midir maintainers](midir-suggestions.md) for the
owner to discuss at release time and removed from this implementation backlog.
This is a handoff, not a claim that their native defects or risks are fixed.

Additional work beyond the prompt

None.

Evidence labels used below:

- **Confirmed in source:** the implementation contains the described behavior
  or failure path. This does not imply that a particular driver has exercised it.
- **Possible:** a failure follows under stated scheduling, device, or driver
  conditions that have not been reproduced.
- **Validation gap:** existing checks do not establish the relevant behavior.
- **API limitation:** deliberate behavior that can nevertheless surprise users.

Severity describes potential impact, not likelihood. High means process failure,
indefinite waiting, unusable ownership state, or unsafe native resource handling;
medium means message loss, misleading results, resource growth, or API surprises.

The [validation record](validation.md) reports passing device-free lifecycle
checks, the 16-job wheel/build matrix, and 25 CoreMIDI software-loopback checks.
Those checks are useful evidence for ordinary ownership, GC, and finalization.
They do not exercise ALSA or WinMM native I/O, unplug/replug, driver crashes,
resource exhaustion, malformed native input, or an indefinitely busy device.
The successful CoreMIDI burst is one measurement, not a throughput guarantee.

Primary binding evidence is in [state.rs](../src/state.rs),
[lifecycle.rs](../src/lifecycle.rs), [callback.rs](../src/callback.rs),
[input.rs](../src/input.rs), [output.rs](../src/output.rs),
[ignore.rs](../src/ignore.rs), and [the public stub](../python/midirp/midi.pyi).
Upstream evidence was read from the exact Cargo registry sources selected by
[Cargo.lock](../Cargo.lock). Source links below identify those versions; line
references and function names refer to that pinned source, not a newer release.

## Most consequential findings

| ID | Severity | Evidence | Concern |
| --- | --- | --- | --- |
| R1 | Medium | Confirmed in source | macOS clients are deliberately retained until process termination upstream. |
| R2 | Medium/High | Confirmed unbounded paths | Unfinished SysEx and application queues can exhaust memory; failed cleanup retains bounded native resources. |
| D1 | Medium | API limitation | Open/closed state does not identify unplugged, rebooted, or unresponsive devices. |
| A1 | Medium | API limitation | This is an adapted Python API, not an exact Rust API mirror. |

## Liveness, concurrency, and shutdown

Default isolation now bounds native-operation waits, terminates a failed child,
and requires a fresh client. In-process mode remains available explicitly.
Worker hangs/crashes, overflow accounting, and lifecycle behavior have controlled
subprocess coverage. Remaining liveness limits concern parent callbacks,
in-process native waits, interpreter lifecycle, and shared OS services.

### L6. Interpreter shutdown and process lifecycle have additional constraints

**API limitation and possible hangs.** Cleanup runs through Python `atexit`.
Recoverable Rust unwinding does not cover allocator/process aborts, memory
corruption, double panics during unwinding, or non-unwinding OS callback
boundaries. The native unsafe/error paths in the
[upstream report](midir-suggestions.md) still need their own fixes. Resources
retained after failed cleanup keep capacity until process exit.
Application callback waits can still deadlock draining: a callback must not
wait for its closer, acquire a lock held by its closer, or block on a full queue
whose consumer has stopped to close input. The MIDI API guard does not detect
application synchronization or calls into other libraries.
In-process mode still cannot progress if all four cleanup workers are stuck in
native teardown or callback draining. Default isolation contains those native
waits through child termination; it does not cancel parent callback code or
repair an OS MIDI service. OS failure to terminate/reap a child also prevents an
absolute whole-system shutdown guarantee. Parent startup/resource-allocation
work is not covered by the native-operation deadline.
Handler ordering matters: a callback waiting for an event set by a later-running
exit handler can stop MIDI shutdown from completing. A callback running a C
extension that blocks while retaining the GIL can prevent Python shutdown from
even reaching the GIL-releasing cleanup code. Error hooks and callable
destructors may run application code during retirement or shutdown.

Forced exit, process crash, or termination bypassing `atexit` does not execute
the binding's graceful cleanup. Physical note/controller state is not restored
by process exit. Forking after import inherits mutex/runtime state without a
usable copy of the cleanup thread and is unsupported. Use a fresh interpreter
in the child, as documented. Subinterpreters are rejected. Reinitializing an
embedded main interpreter in the same process is unvalidated: the process-wide
`OnceLock` survives and its registry may already be marked closing.

## Device disappearance, reboot, and responsiveness

### D1. Connection state is not device health

**API limitation.** `closed` checks whether the binding owns a native connection.
There is no disconnect notification API, heartbeat, device-health state,
automatic reopen, or native backend event stream. An unplugged input can
simply stop delivering while `closed` remains false. Silent hardware, an
unresponsive device, and a legitimately idle device can look identical.
Successful send means the backend accepted the operation as implemented, not
that the physical receiver processed it or stayed responsive.

| Event | Possible visible result | Application implication |
| --- | --- | --- |
| Unplug before connect | Missing lookup, metadata error, connect error, or stale identity | Rediscover deliberately; discovery is not atomic with connect. |
| Unplug during input | Silence, discarded/partial messages, backend logging, or native failure | No guaranteed immediate Python notification. |
| Unplug during output | Send error, accepted data later lost, or blocked driver operation | Delivery is not guaranteed; isolated waits have a deadline. |
| Replug or device reboot | Changed ports/IDs, reused IDs, fresh driver handle, or continued silence on old connection | Existing handles are not a reconnection mechanism. |
| Driver/MIDI service crash or restart | Invalid handles, stale metadata, native errors, hanging calls, or process failure | Default mode discards the affected worker; OS-service recovery is not established. |
| Device stays connected but stops responding | Normal-looking sends and no useful replies | Detect using device-specific request/reply deadlines if its protocol supports them. |
| Process crashes while notes are active | OS reclaims some resources; physical notes/controller state may persist | Cleanup is not musical-state restoration. |

If application recovery needs retry, distinguish safe queries from non-idempotent
commands. A failed or interrupted operation may already have reached the device;
blind resend can duplicate notes, transport commands, or device configuration.
The library supplies no transaction or acknowledgment layer. Filters that
suppress active sensing or timing also suppress those signals for an
application using them to infer health.

During authorized isolated CoreMIDI virtual-port churn, a long-lived discovery
worker returned a stale endpoint ID `"0"`; name lookup raised `PortInfoError`.
Its worker was invalidated as designed, and a fresh client could discover and
exchange messages with the replacement endpoint. Some earlier churn runs also
produced initialization errors or a five-second initialization timeout. Their
cause is unestablished. The final 52-case software-only run passed after healthy
worker disposal was made graceful; that does not prove driver/hotplug stability.

### D2. IDs and names are useful selectors, not permanent identities

**Confirmed in source and API limitation.** Names can duplicate. ALSA IDs are
client/port addresses, which can be reused after client disappearance. macOS
IDs depend on native unique-ID property reads; failed reads become the string
`"0"` upstream. The binding rejects that sentinel with `PortInfoError`; it does
not turn it into a usable identity.
CoreMIDI equality also returns false when those properties cannot be read, so
even self-comparison of a stale native-mode handle can behave unexpectedly.
Isolated handles compare cached IDs and reject ambiguous matches at open, but
cannot detect an ID reused for a different endpoint.

WinMM enumerates interfaces and can skip ports when metadata retrieval fails.
Its stored name is returned without a fresh liveness check. An empty discovery
result can therefore reflect missing/inaccessible interfaces, and successful
name retrieval need not establish that the port still exists. The native
interface identifier is converted from UTF-16 lossily.

**Application implication:** rediscover after device changes, validate the
selected endpoint, and do not use only a name or old ID to prove that a rebooted
device is the intended receiver. `find_port_by_id()` rejects duplicate matches
and returns `None` for a missing ID. A unique match still does not prove enduring identity or permission.

## Dropping references and effects on the rest of the system

### R1. What is being created or dropped matters

Creating `MidiInput` or `MidiOutput` creates a **client**, not a physical MIDI
device. Creating a virtual connection advertises a software endpoint. Connecting
to a hardware endpoint opens a native connection; it does not create hardware.

| Object/lifetime event | Current behavior and external effect |
| --- | --- |
| Import `midirp.midi` | Starts four native cleanup threads and registers exit cleanup, without initializing MIDI. Import can fail if that thread cannot be started. |
| Create then immediately discard an unused client on ALSA | Opens then drops its sequencer resource. Registration changes may be visible to other clients. No MIDI messages are intentionally sent. |
| Create then immediately discard an unused client on WinMM | Constructors retain filter state or an empty output client and ignore client names; native handles open at connect time. |
| Create then immediately discard an unused client on CoreMIDI | Allocates a native CoreMIDI client that upstream deliberately retains until process termination. In-process destruction does not dispose it; default worker exit reclaims it. |
| Drop the client variable while retaining its connection | The connection keeps the original Python client alive. The connection remains usable. |
| Drop the last connection reference | Retires input delivery and queues native close. Completion and client restoration are asynchronous. |
| Drop a connection participating in a Python reference cycle | GC can find the tracked callback/client edges, but collection timing is not deterministic. External references or application objects can retain it. |
| Discard the return value of `connect()` or `create_virtual()` | The new connection can be retired immediately. Briefly exposed endpoints and short-lived traffic are possible; there is no useful persistent connection. |
| Close/drop a virtual connection | Its endpoint is removed when native cleanup completes. Other applications connected to that endpoint can lose their route or receive topology notifications. |

The [coremidi 0.9.2 client source](https://docs.rs/crate/coremidi/0.9.2/source/src/client.rs)
has its `Drop` implementation commented out, intentionally avoiding explicit
`MIDIClientDispose`. Its comment explains the upstream concern about disposing
the last client and later recreating it. Repeated client creation in a long-lived
in-process runtime therefore retains native clients until process exit. Reuse
clients when possible. This is an upstream lifetime policy, not a leaked Python
callback or a reason to dispose clients blindly in this binding.

Native ports and virtual endpoints have their own disposal paths; retaining a
CoreMIDI client does not imply retaining every closed virtual endpoint. Conversely,
automatic connection close does not promise instantaneous endpoint removal.
Default unused-client destruction schedules graceful child shutdown with forced
termination on timeout. In-process unused-client destruction can run on the
Python destructor thread and is not protected by detached connection close.

**Answer to the system-disturbance question:** ordinary creation/discard does
not intentionally reset unrelated devices or send MIDI. It can change visible
client/port topology, and in-process macOS native clients accumulate. Creating
then dropping a virtual endpoint affects peers using that endpoint. Windows
output teardown calls `midiOutReset` on its own open handle; broader effects
depend on driver/device semantics. There is no tested guarantee of zero effects
on other users of a shared driver under failure or resource exhaustion.

Closing does not send a portable all-notes-off sequence, undo controller changes,
or guarantee delivery of pending traffic. In particular, dropping an output
after note-on without arranging note-off can leave a sounding note. An exception
between the README example's note-on and note-off also has this problem, even
though the context manager closes correctly.

### R2. Memory, CPU, threads, handles, and other resource limits

**Confirmed unbounded paths; exhaustion consequences partly untested.**

| Resource | How it is used | Failure or growth risk |
| --- | --- | --- |
| Python/native heap | Owned input `bytes`, native message vectors, outgoing SysEx copies/buffers, port lists | Large messages and retained messages consume memory; allocator failures are not uniformly translated to Python exceptions. |
| SysEx assembly storage | CoreMIDI/ALSA append until completion | Missing `F7` plus continuing packets can grow buffers without a size/deadline cap; no callback is delivered for the unfinished message. |
| Isolated receive accounting | Queued payloads are bounded by count and bytes; drop newest and count | Worker crashes can lose the final counter; driver loss is not counted. In-flight payloads and allocation overhead are outside the queue budgets. |
| In-process cleanup queue | Up to 32 reserved resources and four workers | Stalled jobs retain their native resources and clients; all four stalled workers prevent further progress. |
| CPU/GIL | Per-message Python attachment, copies, callback calls, error hooks | Floods, slow callbacks, or repeated exceptions consume CPU and delay unrelated Python work; no backpressure, batch API, or real-time deadline. |
| Threads/stack | Four cleanup workers per interpreter; two communication threads per isolated client, a parent dispatch thread and child forwarding thread per isolated input; ALSA input reader per input connection; OS-managed backend callback work | OS thread quotas or memory limits can stop import/open. |
| Child processes | One interpreter per isolated client, no global client quota | Startup memory, process quotas, and scheduling overhead can limit client count. |
| File descriptors/handles | Two stdio pipes per worker; ALSA sequencer and stop-pipe descriptors; WinMM handles; native endpoints and queues | OS/user quotas can reject setup or trigger upstream panic/rollback defects. |
| Native clients/ports | CoreMIDI retained clients, ALSA queues/ports, virtual endpoints | Churn can consume native quotas and generate topology work in other applications. |
| Disk | Installation/build artifacts and caches; no recording or MIDI data files in normal binding operation | Full disk mainly affects installation/builds or application/error-hook logging, rather than the MIDI byte path. |
| Network | No binding-owned sockets or network protocol | OS network-MIDI endpoints and user callbacks may depend on network transport; packet loss or peer failure remains subject to D1. |

Messages retained by application code remain valid because the bridge copies
them into owned bytes. That safety property also means retention costs real
memory. There is no native input message-size limit or throughput admission
control. Isolated queue overflow is counted, but unfinished
native SysEx grows before it reaches those queues. An
unfinished native SysEx buffer can retain its allocated capacity after it is
cleared.

Blocking disk/network logging in an error hook or callback can become MIDI
liveness failure. Full disk can hide diagnostics if the application's logger
fails. Network MIDI provided by an OS driver does not turn `send()` into a
remote-delivery acknowledgment. The library itself adds neither disk persistence
nor network reconnection logic.

## API fidelity and traps for Python users

### A1. Names mostly match; the API is not exactly the Rust API

Compared against [midir's common API](https://github.com/Boddlnagg/midir/blob/v0.11.0/src/common.rs)
and [Ignore](https://github.com/Boddlnagg/midir/blob/v0.11.0/src/lib.rs):

| Rust surface | Python surface | Difference |
| --- | --- | --- |
| `MidiInput`, `MidiOutput`, both port and connection types | Same class names in `midirp.midi` | Package/module namespace changes. |
| `MidiInput::new`, `MidiOutput::new` | Class constructors | No public `.new()` method. |
| `ports`, `port_name`, `find_port_by_id`, `ignore`, `connect`, `id`, `send`, `close` | Same method spellings where exposed | Ownership, types, results, and errors are adapted. |
| `port_count()` | Not exposed | Use `len(client.ports())`, which constructs the discovered list. |
| Unix `VirtualInput` / `VirtualOutput` traits and `create_virtual` | Class methods named `create_virtual` | Windows method exists but raises `NotImplementedError`; trait types are not exposed. |
| `Ignore::None`, `Sysex`, `Time`, `ActiveSense`, `All` | `NONE`, `SYSEX`, `TIME`, `ACTIVE_SENSE`, `ALL` | Constants deliberately differ; `None` is a Python keyword. |
| `SysexAndTime`, `SysexAndActiveSense`, `TimeAndActiveSense` | No named constants | Construct the corresponding masks with `\|` or `Ignore(bits)`. |
| `Ignore::contains()` | Not exposed | Constructor, integer conversion, equality, and `\|` are supplied. |
| Callback `(u64, &[u8], &mut T)` with explicit user data | Callback `(int, bytes)` | Capture state in a Python callable; no third user-data argument. |
| Consuming `connect(self, ...)` and `close(self)` | Mutable lifetime state on existing Python objects | Original client becomes unavailable; normal completed close restores it. |
| Input close returns `(MidiInput, T)`; output close returns `MidiOutput` | `close()` returns `None` | Reuse the original client variable, not the return value. |
| `Result`, `Option`, generic `ConnectError<T>`, error kinds | Exceptions, `None`, four `MidiError` subclasses | No returned failed-client object or structured native error-kind API. |
| Rust port `Clone`, `MidiIO`, port-vector aliases | Python handles/lists | No exposed clone method, generic trait, or collection aliases. |
| No matching Rust connection convenience surface | `closed`, context managers, `worker_failure`, input `last_message_time`/`dropped_messages`; module `__version__`, `MidiError` base and binding error categories | Python additions. |

This table is a comparison of the project's intended default backend surface,
not a claim to bind every optional Rust feature or backend.

### A2. Ownership and argument surprises

**API limitations.**

- Retain the returned connection. Keeping only the client does not keep its
  newly opened connection alive. Immediate reuse after dropping a connection
  can fail while automatic cleanup is still pending; explicit close is the
  deterministic normal reuse path.
- A client supports one open connection at a time. Even discovery and filter
  changes reject use while it is connecting, connected, or closing. Applications
  needing concurrent connections/discovery need appropriately separate clients.
- `send()` does not accept a list of MIDI integers or a parsed-message object.
  The binding does not apply uniform
  cross-backend MIDI validation or concatenate/parse arbitrary message streams.
- Port IDs and `closed` are methods/property respectively: use `port.id()` and
  `connection.closed`. Ports are opaque, direction-specific, unhashable handles;
  they are not manually constructible device descriptions or durable dict keys.
- `Ignore` is not a Python `IntFlag` API. Only the exposed operations are
  promised. `ALL` filters SysEx, timing, and active sensing, not all MIDI. Default
  `NONE` admits these categories; `TIME` should not be interpreted as filtering
  every transport or timestamp-related message uniformly across backends.
- A `MidiError` handler does not catch wrong-type errors, unavailable-state
  `RuntimeError`, Windows `NotImplementedError`, Python callback exceptions, or
  every native panic/allocation failure. Native backend error details are not
  a stable structured taxonomy for recovery decisions.
- Context-manager exit waits for close. Exceptional teardown can block.
  Idempotence is a normal successful-teardown property, not a driver-failure
  recovery guarantee.

### A3. Callback traps

**Confirmed in source.** An uninspectable callable with incompatible arguments,
or a callable that raises, fails later for every message through
`sys.unraisablehook`; it does not raise in the thread that opened the port.
Ordinary return values are ignored. Return behavior cannot be established
without calling application code; opening does not execute the callback to
inspect its result. Diagnosing a deferred result cannot make the callback process
its message or establish whether application code already scheduled that object.

Callbacks execute on parent dispatch threads by default, or native delivery
threads in-process, rather than the main thread or an asyncio event loop.
GUI/main-thread APIs and application state
need their own thread handoff. Exceptions, including callback-raised exit or
interrupt exceptions, do not provide a reliable way to stop the controlling
thread. A slow error hook is part of callback duration and drain time.

Timestamps are microseconds with backend-specific origins and precision, not
Python wall-clock seconds. WinMM derives them from a 32-bit millisecond value,
so long-running connections need rollover consideration, roughly every 49.7
days. Equal timestamps are possible; unrelated connection clocks must not be
compared without an established conversion. Native callback admission can also
fail during interpreter shutdown through `try_attach`, skipping Python delivery.

### A4. Documentation and supported-platform boundaries

**Validation gaps.** Native exception detail remains a human-readable string
rather than a structured error kind. Cross-platform SysEx framing remains a
validation gap.
`requires-python = ">=3.11"` has no upper bound, while validation covers only
standard CPython 3.11 through 3.14 across the release matrix, plus local macOS
arm64 builds and unit checks on CPython 3.15. Native CoreMIDI initialization
failed during the 3.15 check and a 3.11 comparison; native I/O on 3.15 remains
unvalidated. A source build on a later interpreter is not validated merely
because package metadata permits an attempted install. Linux
needs sequencer availability and permissions independently of wheel import;
Windows devices may be exclusively owned elsewhere. musl, PyPy, free-threaded
Python, other architectures, and alternate optional midir backends are outside
the recorded matrix. Dependency pins make this review reproducible but do not
establish that upstream driver/parser defects have been fixed.

## Proposed follow-up order

These are recommendations for future authorized work, not changes made by this
document. Native fault checks need explicit runtime authorization and selected
software endpoints or hardware; ordinary CI should not probe arbitrary devices.

1. Reproduce bounded subprocess liveness cases for application wait cycles,
   queued SysEx teardown, and exit-handler ordering. Distinguish native hangs
   from Python lock cycles.
2. Run authorized unplug/replug, device/driver restart, long SysEx, overload,
   and long-duration timestamp tests on each backend. Record message loss,
   cleanup time, surviving resources, exceptions, and identity changes.
3. Validate the new default process boundary on each native backend under faults.
   Device-health events and framing changes remain
   separate API decisions; process isolation does not establish device health.
4. Update user guidance for client retention, asynchronous destruction,
   callback constraints, SysEx framing, API differences, and the limits of
   failed-open restoration after resolving or explicitly accepting those risks.

For every native hang test, use an external subprocess deadline and retain
diagnostics before terminating the test process. Do not inject faults into a
shared driver or hardware device without authorization. Passing happy-path
loopback checks must remain separate from evidence about driver failure,
exhaustion safety, and bounded shutdown.
