# Issues and failure review

## Scope and evidence

Source review dated 2026-10-08, against binding commit `b7f50d0`, midir
0.11.0, PyO3 0.29.3, and the transitive CoreMIDI wrapper coremidi 0.9.2.
This is an issue inventory and proposed investigation plan, not a claim that
every failure below has been reproduced. Later fixes and authorized checks
are reflected below; backend defects remain unless explicitly resolved.
Open findings are updated as fixes land; resolved issues are removed.
Remove resolved issue text in the last commit of its fix series.

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
| B2 | Medium | Confirmed in source | Failed ALSA opens can leave queues or ports allocated on the restored client. |
| B3 | High | Confirmed cleanup gaps; possible unsafe callback | WinMM partial input-open failures lack complete native rollback. |
| B4 | High | Possible | WinMM reset/requeue lock interaction and failed buffer unpreparation need native fault checks. |
| B5 | High | Confirmed parser assumption; possible trigger | Truncated CoreMIDI packets can panic in native callback parsing. |
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
boundaries. The backend-specific unsafe/error paths below still need their own
fixes. Resources retained after failed cleanup keep capacity until process exit.
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

Free-threaded Python is outside the tested contract. The runtime explicitly
assumes GIL-serialized initialization; the binding does not add its own explicit
free-threaded-build rejection. This needs a contract-enforcement check against
PyO3's module/GIL behavior, not an assumption that every unsupported build
either safely works or cleanly fails.

## Backend-specific exceptional paths

### B2. ALSA failed opens can retain native allocations

**Medium; confirmed in source, accumulation not measured.** Input allocates its
timestamp queue before validating the remote port and connection name. Returned
errors on these paths do not free that queue while returning the sequencer to
the caller. After creating a local input/output port, subscription failure
returns the client without deleting that port. Repeated failures on a retained
client can accumulate queues or ports until native client destruction or quota
exhaustion. Pipe descriptor wrappers do have RAII cleanup; this is not a claim
that all failed opens leak all allocations.

Queue allocation and initialization also use `unwrap()`, so exhaustion can
produce a panic rather than `ConnectError`. Some queue start/drain errors are
ignored, allowing a connection to be returned without confirming all setup.

**Proposed next work:** measure queues/ports before and after repeated stale-port,
invalid-name, subscription, and allocation failures; investigate upstream
transactional rollback.

### B3. WinMM partial initialization lacks complete rollback

**High; confirmed missing cleanup, possible native memory-safety consequences.**
After `midiInOpen`, midir allocates four raw 1,024-byte SysEx buffers and prepares
and queues each header. Failures from `midiInPrepareHeader` or `midiInAddBuffer`
return without closing the native input handle or releasing every allocation.
The source contains TODOs acknowledging these gaps. Handler data is then
dropped even though its pointer was supplied as the native callback context.
If the still-open driver subsequently invokes a callback that uses that
context, it can access freed memory. That callback occurrence has not been
demonstrated here. The start-failure path also lacks the complete ordinary
buffer teardown sequence.

Raw allocation uses `alloc()` without checking for a null pointer before
handing the buffer to WinMM. Memory exhaustion cannot be assumed to become a
clean Python `MemoryError` or `ConnectError`.

**Proposed next work:** controlled WinMM failure injection at each preparation,
queueing, start, and allocation step, with native memory diagnostics and an
upstream fix review before promising exhaustion-safe input creation.

### B4. WinMM shutdown depends on reset and buffer-return behavior

**High; possible deadlock and unsafe failure handling.** Input close holds the
native handle mutex while calling `midiInReset` and `midiInStop`. The
[WinMM callback handler](https://github.com/Boddlnagg/midir/blob/v0.11.0/src/backend/winmm/handler.rs)
takes the same mutex to requeue a nonempty SysEx buffer. If reset delivers such
a callback and waits for its completion, this creates a lock cycle. Retiring
Python delivery does not bypass upstream requeue logic. Empty returned buffers
are specially excluded upstream, but nonempty reset behavior needs validation.

Close calls unprepare, then frees buffer/header storage even when unprepare
failed. It only logs a warning. Reset, stop, and final close return values are
ignored. A driver still referencing that storage after unsuccessful cleanup
would make this unsafe. These are conditional consequences, not evidence that
ordinary close on every Windows driver fails.

**Proposed next work:** native teardown with queued, partial, and malformed
SysEx, unplugging during reception, failed reset/unprepare, and drivers that
return nonempty buffers during reset. Use isolated, deadline-bounded processes.

### B5. CoreMIDI parsing trusts complete short messages

**High; confirmed unchecked slice, malformed-packet trigger untested.** The
[CoreMIDI backend](https://github.com/Boddlnagg/midir/blob/v0.11.0/src/backend/coremidi/mod.rs)
infers a message length from its status byte, then slices
`pdata[cur_byte..cur_byte + size]` without checking the packet has that many
remaining bytes. A truncated note or other multi-byte message can panic before
the binding sees it. A native peer able to submit malformed packets is a
possible trigger; existing loopback checks send well-formed data.

**Proposed next work:** focused upstream parser tests with truncated messages,
running status, interleaved realtime, and multiple messages per packet. Any
native malformed-input reproduction belongs in an isolated process because a
panic can cross the OS callback boundary.

### B6. Message framing, loss, and errors differ between backends

**Confirmed in source; medium API portability risk.** WinMM forwards large
SysEx in chunks from its 1,024-byte input buffers and explicitly does not
reassemble them. CoreMIDI and ALSA accumulate segmented SysEx. Consequently one
callback is not a portable promise of one complete SysEx message. WinMM long
error notifications and failed buffer requeue are not surfaced as a structured
Python device-error event. Its four native buffers do not establish a maximum
whole-message size or a no-loss guarantee.

ALSA logs overrun and other reader errors, then continues. Persistent input
errors can cause repeated work/logging rather than an application-visible
disconnect, and potentially a busy loop depending on readiness/error behavior.
Rust `log` output has no configured Python logging bridge here. An application
may receive neither a Python exception nor an observable warning about loss.

ALSA output ignores `drain_output()` failure after direct output. Its encoder
returns a consumed-byte count which midir discards; sending concatenated
messages should not be assumed to forward every byte as separate MIDI events.
The backends do different message validation. CoreMIDI can split packet data
into short messages, while WinMM rejects non-SysEx buffers longer than three
bytes. Use one well-formed message per send and handle SysEx framing explicitly
where cross-platform reception requires it. Existing CoreMIDI large-message
checks do not validate Windows framing.

## Device disappearance, reboot, and responsiveness

### D1. Connection state is not device health

**API limitation.** `closed` checks whether the binding owns a native connection.
There is no disconnect notification API, heartbeat, last-received timestamp,
health state, automatic reopen, or native-error stream. An unplugged input can
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
`"0"` upstream instead of raising an error. Multiple such failures can collide.
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
device is the intended receiver. `find_port_by_id()` returns the first matching
port or `None`, not a uniqueness or permission diagnosis.

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
| Isolated receive queues | 128 messages in the child and 128 in the parent; drop newest and count | Capacity is in messages, not bytes. Worker crashes can lose the final counter; driver loss is not counted. |
| Application buffering | README examples use unbounded `Queue()` | A producer faster than its consumer grows memory outside the binding. A bounded blocking queue can deadlock callback draining if its consumer stops. |
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
memory. There is no input message-size limit, throughput admission control,
or byte-based memory budget. Isolated queue overflow is counted, but unfinished
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
| No matching Rust connection convenience surface | `closed`, context managers; module `__version__`, `MidiError` base | Python additions. |

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
- `send()` accepts immutable `bytes`, not a list of MIDI integers, `bytearray`,
  `memoryview`, or a parsed-message object. The binding does not apply uniform
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
  every native panic/allocation failure. Native error messages are not a stable
  structured error taxonomy for recovery decisions.
- Context-manager exit waits for close. Exceptional teardown can block or
  replace the exception raised by the context body. Idempotence is a normal
  successful-teardown property, not a driver-failure recovery guarantee.

### A3. Callback traps

**Confirmed in source.** Only callability is checked at open. Wrong argument
count or a callable that raises fails later for every message through
`sys.unraisablehook`; it does not raise in the thread that opened the port.
Return values are ignored. Passing an `async def` or generator function can
return an unawaited coroutine or uniterated generator instead of processing
messages. An async function remains callable, so opening does not reject it.

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
The stub describes handles as unhashable but does not explicitly declare a
`__hash__ = None` surface; static-tool behavior should be checked before calling
this a type-checker defect.

`requires-python = ">=3.11"` has no upper bound, while validation covers only
standard CPython 3.11 through 3.14. A source build on a later interpreter is not
validated merely because package metadata permits an attempted install. Linux
needs sequencer availability and permissions independently of wheel import;
Windows devices may be exclusively owned elsewhere. musl, PyPy, free-threaded
Python, other architectures, and alternate optional midir backends are outside
the recorded matrix. Dependency pins make this review reproducible but do not
establish that upstream driver/parser defects have been fixed.

## Proposed follow-up order

These are recommendations for future authorized work, not changes made by this
document. Native fault checks need explicit runtime authorization and selected
software endpoints or hardware; ordinary CI should not probe arbitrary devices.

1. Address ALSA failed-open allocation rollback and WinMM partial
   initialization/teardown risks first. Decide whether to contribute upstream
   changes or adopt an already verified upstream fix before changing pins.
2. Investigate allocator aborts, native busy loops, backend failed rollback, and
   unsafe callback boundaries in isolated processes. These are outside the
   binding's recoverable-panic containment.
3. Reproduce bounded subprocess liveness cases for application wait cycles,
   queued SysEx teardown, and exit-handler ordering. Distinguish native hangs
   from Python lock cycles.
4. Run authorized unplug/replug, device/driver restart, long SysEx, overload,
   and long-duration timestamp tests on each backend. Record message loss,
   cleanup time, surviving resources, exceptions, and identity changes.
5. Validate the new default process boundary on each native backend under faults.
   Health events, byte budgets, framing, and structured error changes remain
   separate API decisions; process isolation does not establish device health.
6. Update user guidance for client retention, asynchronous destruction,
   callback constraints, SysEx framing, API differences, and the limits of
   failed-open restoration after resolving or explicitly accepting those risks.

For every native hang test, use an external subprocess deadline and retain
diagnostics before terminating the test process. Do not inject faults into a
shared driver or hardware device without authorization. Passing happy-path
loopback checks must remain separate from evidence about driver failure,
exhaustion safety, and bounded shutdown.
