# Suggestions for midir maintainers

## Purpose and review baseline

This report collects four issue groups found while developing midirp, a Python
binding for midir. It is intended for discussion with midir's developers when
midirp is released. The IDs B3–B6 preserve the identifiers from our original
review; they are not upstream GitHub issue numbers. B6 contains several related
portability and error-reporting concerns, which could warrant separate upstream
issues.

The source observations below were checked against the Cargo registry copy of
**midir 0.11.0** selected by midirp's Cargo.lock on **2026-10-09**. The CoreMIDI
wrapper dependency is **coremidi 0.9.2**. Source links use the midir `v0.11.0` tag;
line numbers refer to that version. We have not established whether a later
release or the development branch already addresses these findings. Please
recheck that before submitting or implementing a patch.

**Status:** preparation and transfer out of midirp's implementation backlog are
complete. These findings are not fixed by this report, and this document has
not been sent to the maintainers. The owner will approach them at release time.

Additional work beyond the prompt

None.

## Evidence and priorities

| ID | Platform | Source observation | Consequence not reproduced here | Suggested priority |
| --- | --- | --- | --- | --- |
| B3 | WinMM | Incomplete rollback after input initialization errors; unchecked raw allocation result | Live driver callback accessing freed handler state; exhaustion failure | High: native lifetime safety |
| B4 | WinMM | Reset/stop run with the requeue mutex held; storage freed despite unprepare failure | Callback/close deadlock; driver access to freed buffers | High: shutdown safety and liveness |
| B5 | CoreMIDI | Status-derived short-message length used in an unchecked slice | OS-delivered truncated packet causing process failure | High: parser robustness |
| B6 | WinMM, ALSA, CoreMIDI | Different SysEx framing and validation; setup and delivery errors inconsistently exposed | Persistent-error busy loop, loss under faults, cross-platform application failures | Medium overall; ALSA initialization panics merit earlier attention |

“Confirmed in source” describes the implementation, not a successful hardware
reproduction. Our passing software-only CoreMIDI loopback checks exercise
well-formed messages and ordinary lifecycle behavior. They do not demonstrate
safety under malformed packets, WinMM driver faults, ALSA resource exhaustion,
unplug/replug, or persistent errors. No such native fault tests were run to
prepare this document. Proposed tests below remain proposals.

## B3. WinMM input initialization needs complete rollback

### Source evidence

In [WinMM input initialization, lines 238–337](https://github.com/Boddlnagg/midir/blob/v0.11.0/src/backend/winmm/mod.rs#L238-L337),
`MidiInput::connect` allocates boxed `HandlerData`, then passes its address as the
callback context to `midiInOpen`. After a successful open, it allocates four
1,024-byte raw SysEx buffers and boxed `MIDIHDR` structures. Each header is
prepared and queued separately.

A failure from `midiInPrepareHeader` or `midiInAddBuffer` immediately returns a
`ConnectError`. These branches do not reset/close the native input handle or
unprepare and reclaim the already-created headers and payload allocations.
Comments in this code explicitly acknowledge missing error-path cleanup.
Dropping the boxed handler does not reclaim the allocations stored as raw
pointers in its SysEx array.

The raw payload allocation uses `std::alloc::alloc` without checking its return
value for null before supplying it to WinMM. This path does not establish a
safe, recoverable out-of-memory result.

The `midiInStart` failure branch attempts `midiInClose`, ignores its result, and
returns without the ordinary reset/unprepare/free sequence. Microsoft documents
that close fails while queued input buffers remain outstanding and that reset
returns them through the callback. An attempted close alone therefore does not
establish that the handle has stopped using its context.
[Microsoft: midiInClose](https://learn.microsoft.com/en-us/windows/win32/api/mmeapi/nf-mmeapi-midiinclose).

### Possible impact and uncertainty

The missing cleanup is source-confirmed. The more serious lifetime concern is
conditional: if a driver remains open and later delivers a data or long-data
notification, the [callback dereferences the supplied context, lines 21–28](https://github.com/Boddlnagg/midir/blob/v0.11.0/src/backend/winmm/handler.rs#L21-L28),
which may already have been freed. We have not reproduced that callback sequence.
Notifications that return before dereferencing the context do not establish this
failure on their own.

Depending on the failed step, native handles, headers, and payloads can remain
allocated. Repeating failed opens could consume process or driver resources.
A null payload allocation could reach a native API; the eventual effect has not
been measured. A returned `ConnectError` should not be treated as proof that all
native activity and resources have been retired.

### Suggested change

Track the initialization state explicitly: handle opened, each payload allocated,
each header prepared, and each header queued. Use one rollback path for every
failure after native open, including start failure. Maintain callback context
and buffer lifetime until the driver can no longer access them. Check allocation
results or use an owned allocation whose failure policy is explicit.

Rollback must not simply free everything after an unsuccessful reset, unprepare,
or close. B4 covers the same ownership problem during normal teardown. If a
failed driver prevents safe reclamation, preserve memory safety and expose the
failure rather than freeing storage still potentially owned by the driver.
The exact API and retention policy need maintainer review.

### Proposed verification

Inject prepare/add failures at each of the four buffer positions, plus start
and payload-allocation failure. Verify both earlier resources and the resource
at the failed step. Exercise callbacks during rollback and after an attempted
close failure. Check lifetime, handle/allocation accounting, and absence of
use-after-free with Windows native memory diagnostics. Use controlled native
stubs for deterministic failure paths and a separately authorized driver test
for real callback behavior.

## B4. WinMM shutdown needs callback-safe locking and buffer ownership

### Source evidence

[WinMM input teardown, lines 354–386](https://github.com/Boddlnagg/midir/blob/v0.11.0/src/backend/winmm/mod.rs#L354-L386)
holds `handler_data.in_handle`'s mutex while calling `midiInReset` and
`midiInStop`, then unprepares/frees buffers and closes the handle.

The [long-message callback requeue path, lines 84–105](https://github.com/Boddlnagg/midir/blob/v0.11.0/src/backend/winmm/handler.rs#L84-L105)
locks that same mutex when a returned buffer contains bytes. It avoids requeueing
empty buffers, but there is no explicit closing state excluding nonempty buffers
from requeue during teardown.

For each header, teardown calls `midiInUnprepareHeader`, then unconditionally
frees the payload and header before inspecting the result. Failure is logged as
a warning. Reset, stop, and final close results are ignored.

Microsoft documents that reset returns pending buffers through callbacks. It
also documents `MIDIERR_STILLPLAYING` when unprepare sees a queued buffer and
requires the driver to finish with the buffer before unpreparation/freeing.
[Microsoft: midiInReset](https://learn.microsoft.com/en-us/windows/win32/api/mmeapi/nf-mmeapi-midiinreset),
[Microsoft: midiInUnprepareHeader](https://learn.microsoft.com/en-us/windows/win32/api/mmeapi/nf-mmeapi-midiinunprepareheader).

### Possible impact and uncertainty

A potential lock cycle is: close owns the handle mutex and waits in reset;
a callback caused by reset receives a nonempty buffer and waits for the same
mutex; reset waits for that callback to finish. The code contains both lock
acquisitions. Whether the necessary callback scheduling and waiting occur for a
particular driver is untested. Empty-buffer behavior does not prove safety for
nonempty reset returns.

An unprepare failure does not establish that the driver released its buffer.
Freeing it anyway could leave the driver referencing invalid memory. Likewise,
a failed final close does not establish that callback context can safely be
dropped. These are conditional native lifetime consequences, not reproduced
crashes on ordinary Windows devices.

Suppressing the application's callback does not remove the native handler's
requeue path. Catching a Rust panic cannot resolve a mutex cycle or repair
freed native storage.

### Suggested change

Introduce a shutdown state visible to the native callback, prevent requeue once
shutdown starts, and avoid holding a callback-needed mutex across native calls
that may invoke or wait for that callback. Preserve the necessary coordination
between reset, callback completion, and native handle lifetime; merely removing
the mutex is not a demonstrated safe fix.

Check reset, stop, unprepare, and close results. Free storage only after native
ownership is safely relinquished. Define a safe failed-cleanup policy and an
observable outcome for callers. The existing consuming `close()` interface may
need an API decision to expose errors; `Drop` still needs safe behavior without
assuming it can return an error.

### Proposed verification

Use a controlled driver/API substitute to return empty and nonempty buffers
synchronously and asynchronously during reset. Inject reset, stop, unprepare,
and close errors, including queued-buffer conditions. Assert that shutdown
cannot requeue buffers or free driver-owned storage. Then run separately
authorized Windows checks with partial SysEx, traffic during close, and device
removal. Put hang tests in externally supervised processes with deadlines and
retain diagnostics before termination.

## B5. CoreMIDI short-message parsing needs packet-length checks

### Source evidence

In [CoreMIDI input parsing, lines 124–182](https://github.com/Boddlnagg/midir/blob/v0.11.0/src/backend/coremidi/mod.rs#L124-L182),
`handle_input` expects a status byte, derives a message size, and slices
`pdata[cur_byte..(cur_byte + size)]` without confirming sufficient remaining
packet data. For example, a packet containing only `90 3C` selects a three-byte
note message but contains two bytes. A packet ending in `F1`, `F2`, or `F3`
without its required data bytes has the same unchecked-length issue when that
message is admitted by the filters.

The out-of-bounds slice would panic if those bytes reach this parser. That
consequence follows directly from the source, but we have not shown that a
native sender/driver will deliver such a truncated packet to midir.

### Possible impact and uncertainty

Parsing runs before the user's MIDI callback. A wrapper around that callback
cannot validate the packet first or catch a panic that has already happened.
A panic may reach the native callback boundary and terminate the process,
depending on the callback wrapper and unwind boundary. We have not reproduced
an OS-delivered malformed-packet abort.

Running status, interleaved realtime messages, and multiple messages within a
packet also deserve explicit parser tests. They are coverage suggestions here,
not claims that each case contains a demonstrated defect.

### Suggested change

Validate the remaining byte count before every short-message slice. Define what
happens to incomplete input: discard/report the malformed remainder, or preserve
it for continuation only if CoreMIDI's packet contract requires that behavior.
Maintainers should choose that contract; blindly combining packets could change
message semantics. Ensure malformed input cannot unwind across the native
callback boundary. Define how parsing resynchronizes after an invalid packet.

### Proposed verification

Test every two- and three-byte status with all truncated lengths, including an
otherwise valid packet ending in an incomplete message. Include empty packets,
multiple complete messages, filter combinations, running-status data,
interleaved realtime, and fragmented SysEx. Assert no panic, no fabricated
callback bytes, and well-defined handling of the next valid input. Prefer pure
parser tests for deterministic coverage. Any OS-level malformed-packet test
must run in an isolated process under an external deadline.

## B6. Framing, setup failures, and input/output errors need a clearer contract

### WinMM SysEx callback boundaries and loss reporting

The [WinMM handler, lines 73–122](https://github.com/Boddlnagg/midir/blob/v0.11.0/src/backend/winmm/handler.rs#L73-L122)
forwards the contents of a returned SysEx buffer, invokes the callback, and
clears its message vector. Its comment explicitly notes that messages longer
than the 1,024-byte buffer are split and could be reassembled. CoreMIDI and ALSA
instead accumulate segmented SysEx before delivery. Applications cannot assume
one callback means one complete SysEx message across all three backends.

`MM_MIM_LONGERROR` payloads are not delivered as ordinary messages or exposed
through a separate error callback. A failed `midiInAddBuffer` during requeue is
logged. A reduced buffer supply can affect later input without a structured
notification to the application. Exact loss under those failures is untested.

Suggested resolution: document a uniform callback framing contract, then either
align the backends or explicitly expose fragments. If assembling complete
messages, define an assembly size limit, interrupted-message policy, and loss
notification. Provide a way to observe long-message and requeue failures rather
than requiring applications to infer them from silence.

A related resource limit is visible in
[CoreMIDI SysEx continuation](https://github.com/Boddlnagg/midir/blob/v0.11.0/src/backend/coremidi/mod.rs#L109-L121)
and [ALSA SysEx handling](https://github.com/Boddlnagg/midir/blob/v0.11.0/src/backend/alsa/mod.rs#L884-L888):
continued input without terminating `F7` can grow native assembly storage before
any user callback runs. This is the upstream portion of midirp's broader R2
resource review, not a fifth report group. A post-callback queue budget cannot
limit that storage. A bound and defined resynchronization policy would mitigate
malformed or abandoned SysEx; allocator exhaustion has not been reproduced.

### ALSA input setup failures

[ALSA queue initialization, lines 230–244](https://github.com/Boddlnagg/midir/blob/v0.11.0/src/backend/alsa/mod.rs#L230-L244)
uses `unwrap()` for queue allocation, tempo allocation, and tempo configuration.
Failures can therefore panic instead of producing `ConnectError`.
[Queue startup, lines 280–286](https://github.com/Boddlnagg/midir/blob/v0.11.0/src/backend/alsa/mod.rs#L280-L286)
discards control/start and drain results. Opening can proceed without checking
those setup outcomes. Precisely how a failed queue affects input/timestamps
needs a native fault test.

Suggested resolution: propagate fallible setup through the existing connection
error path and unwind partially completed setup safely. Check queue-start and
required drain outcomes. A binding's disposal of a failed client contains some
consequences but does not make ignored setup errors observable.

### ALSA persistent input errors

[ALSA input processing, lines 786–847](https://github.com/Boddlnagg/midir/blob/v0.11.0/src/backend/alsa/mod.rs#L786-L847)
logs overrun, `EAGAIN`, and other read errors, then continues. Polling is entered
when the pending-event query returns `Ok(0)`; other outcomes do not establish a
wait before another read. Persistent failures could therefore cause repeated
work/logging or a busy loop, depending on the ALSA return sequence. That CPU and
liveness scenario has not been reproduced here.

Suggested resolution: distinguish transient readiness changes, recoverable
loss, and terminal errors; ensure repeated errors cannot spin indefinitely or
hide a stop request. Expose terminal failure/loss through a defined application
mechanism. Rust logging can be useful, but a caller without a configured logger
may receive no visible diagnostic. midirp currently has no Python logging
bridge for these upstream records.

### ALSA send semantics and backend validation

[ALSA send, lines 715–736](https://github.com/Boddlnagg/midir/blob/v0.11.0/src/backend/alsa/mod.rs#L715-L736)
discards the encoder's consumed-byte count, emits one encoded event, and ignores
`drain_output()` failure. A buffer containing concatenated MIDI messages can
therefore be only partly represented by that one event without the consumed
length being checked. CoreMIDI packet parsing and WinMM's short-message send
path apply different validation; the latter
[rejects non-SysEx buffers longer than three bytes](https://github.com/Boddlnagg/midir/blob/v0.11.0/src/backend/winmm/mod.rs#L655-L661).

Suggested resolution: state whether `send()` accepts exactly one complete
message or a byte stream. If one message, reject incomplete/trailing input
consistently rather than silently accepting a prefix. If a stream, encode and
send all consumed messages with a defined partial-failure result. Determine
whether a drain is required after direct output; propagate relevant errors or
remove an unnecessary operation rather than assuming every ignored result
implies lost delivery. We have not measured a direct-output loss caused by a
drain failure.

### Proposed verification

Compare callback bytes and boundaries for SysEx below, at, and above the WinMM
buffer size, including multiple buffers, realtime interleaving, missing `F7`,
interruption, and recovery after dropping an oversized assembly. Inject long
errors, failed requeue, ALSA queue allocation/configuration/start failure,
overrun, repeated `EAGAIN`, terminal input failure, and output failure.

Measure callback/loss notification, CPU use, shutdown progress, and retained
resources under persistent errors. For sending, test complete single messages,
truncated input, concatenated messages, encoder partial consumption, and drain
failure according to the chosen contract. Establish backend-specific expected
results before calling any happy-path test a portability guarantee.

## Current midirp containment and upstream handoff

midirp defaults to a fresh child process per client, deadlines around native
operations, and disposal of a failed worker. This contains many native hangs
and process crashes. It does not repair native lifetime errors, recover lost
messages, cap unfinished native SysEx, or repair a shared OS MIDI service.
Explicit in-process mode retains the native backend risks in the application
process. Parent callback waits are outside the native-operation deadline.

Our receive queues have count and payload-byte budgets, but those apply after
native parsing/assembly. We recommend one well-formed message per send and do
not promise that success is physical device acknowledgment.

Before approaching maintainers, recheck current upstream source and existing
issues. Present B3/B4 as lifetime-safety reviews with conditional driver triggers,
B5 as a concrete unchecked parser operation with an untested native trigger,
and B6 as a contract/error-handling discussion with several candidate patches.
Provide reproducible native fault cases when available and keep source review
separate from hardware evidence. No upstream issue, patch, dependency change,
or fault-test execution is performed by this handoff.
