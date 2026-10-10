# Validation and release record

## Scope

Slices 7 and 8 add native check fixtures, CI, distribution checks, and usage
documentation. Build artifacts remain candidates for validation. Publication
and a release tag are separate decisions.

Additional work beyond the prompt

None.

## Local evidence, 2026-10-08

Host: macOS 14.5 (23F79), arm64; Rust 1.87; midir 0.11.0;
PyO3 0.29.3; maturin 1.15.0; uv 0.10.12.

| Interpreter | Binding unit checks | Native-thread lifecycle checks |
| --- | --- | --- |
| CPython 3.11.7 | 34 passed | 19 passed plus 2 isolated finalization checks |
| CPython 3.12.3 | 34 passed | Included in the Python runner |
| CPython 3.13.5 | 34 passed | Included in the Python runner |
| CPython 3.14.6 | 34 passed | Included in the Python runner |

The larger matrix exposed a test synchronization race: the native-thread stop
signal precedes client restoration. The test now waits for the connection lock
to confirm completed automatic cleanup before asserting client reuse. Production
behavior did not change.

Ruff, formatting, ty, pyupgrade, Cargo formatting, Clippy with warnings denied,
and the scoped diff check pass. After explicit owner authorization, all 25
CoreMIDI virtual-port checks passed on this host with CPython 3.11.7. No hardware
ports were connected.

## Default process isolation, 2026-10-08

The public constructors now default to one fresh child per client with a
five-second configurable native-operation deadline. `isolated=False` retains the
original native path. Controlled subprocess checks exercise hung initialization,
send/close, child crash, native returned errors, fresh-client recovery, stale
connection generations, local callbacks/error hooks, two-stage overflow counts,
GC/destruction, callback draining, and interpreter-exit termination/reaping.
No OS MIDI ports are opened by these unit fixtures. The full local Python suite
passes 59 cases; Ruff, formatting, ty, pyupgrade, Cargo formatting, and Clippy
with warnings denied pass. Partial communication-thread startup failure also
terminates and reaps the already-created child. The rebuilt CPython 3.11 arm64
wheel from the sdist passes both archive checks and 58 installed-package checks
outside the checkout, including controlled worker faults and lifecycle checks.
CI now exercises those isolation checks against installed wheels on every
matrix combination. [Run 37824404624](https://github.com/rec/midirp/actions/runs/37824404624)
passed all 16 jobs for implementation commit `5b5fc29`: Python 3.11–3.14 on
Linux x86_64, Windows x86_64, and macOS arm64/Intel. Each job passed Rust checks,
59 Python checks, two archive checks, and 58 installed-wheel checks outside the
checkout. This establishes portable worker logic/builds, not ALSA/WinMM native I/O
or physical hotplug behavior.

All 52 authorized CoreMIDI software-only cases passed in both modes on CPython
3.11.7. This includes endpoint disappearance and deliberate fresh-client
rediscovery, healthy close/reopen against an endpoint kept alive, exact bytes,
filters, SysEx, bursts, duplicate identities, and shutdown. No hardware was used.
Earlier isolated topology-churn runs produced stale ID `"0"` metadata failures
and occasional native initialization failures/timeouts. Failed clients were
contained, but their backend cause is not established. Graceful healthy-worker
disposal precedes forced termination; the final passing run does not prove
shared-service, hardware hotplug, or exhaustion safety.

Native Rust checks pass 36 ordinary tests plus three isolated finalization cases
run by the Python lifecycle harness. Parent callbacks still have no cancellation
deadline. Native ALSA/WinMM I/O and physical unplug/reboot checks remain unrun.

## ALSA failed-open containment (B1), 2026-10-08

Pinned midir 0.11.0 returns `ConnectErrorKind::Other("could not start ALSA input
handler thread")` from both input opening paths after moving the sequencer into
the spawn closure. The binding now marks that client failed before publishing
its returned state. Opening preserves `ConnectError`; subsequent discovery,
configuration, and opening fail with `RuntimeError` requiring a fresh client.
Ordinary returned errors preserve their existing restoration behavior.

A device-free Rust regression injects the same error and a returned client with
its sequencer removed. It failed before the fix because native reuse was
attempted, and passes after the fix. It checks metadata/configuration gating,
reopening rejection, cleanup with no live handle, and independent fresh-client
reuse. This verifies binding containment, not real ALSA thread exhaustion or
upstream resource rollback; native Linux fault injection remains unrun. No
native MIDI ports, dependency changes, or upstream modifications were needed.
All 59 Python checks pass, including the Rust lifecycle harness with 37 ordinary
Rust cases and three isolated finalization cases. Ruff, formatting, ty,
pyupgrade, Cargo formatting, and Clippy with warnings denied pass. CI is now
release-only, so this push does not schedule a new cross-platform run.

## ALSA failed-open resource disposal (B2), 2026-10-09

The GitHub workflow was verified to contain only `release: types: [published]`.
No push, pull-request, or manual-dispatch trigger remains.

Native ALSA input and output clients now use a discard-on-failed-open policy,
including `isolated=False`. Any native returned opening error disables the client
before disposing it outside the state lock. Opening retains its `ConnectError`;
subsequent operations require a fresh client. CoreMIDI/WinMM ordinary returned
errors keep their existing in-process restoration behavior. Successful close
still restores healthy clients. No dependency or upstream source was changed.

The pinned ALSA wrapper's `Seq::drop` invokes `snd_seq_close`. The
[ALSA cleanup contract](https://www.alsa-project.org/alsa-doc/alsa-lib/group___sequencer.html)
closes the client and releases its resources; it also broadcasts client exit and
disconnects its routes. The wrapper ignores the close return value, so source
review and binding tests cannot establish successful OS cleanup under failure.

A device-free regression injects partially allocated queues/ports and the native
stale-port, invalid-name, port-creation, input-subscription, and output-subscription
errors. It failed under restoration because allocations survived. Disposal now
runs once before error return, releases the fixture allocations, and prevents
reopening. Another test injects a destruction panic and verifies that the original
opening error survives and the client stays disabled. Native ALSA resource counts,
thread exhaustion, and close failures remain unmeasured on this Mac. Queue setup
panics/ignored errors remain listed under B6 rather than being marked fixed.

Local verification passes all 59 Python checks and 39 ordinary Rust cases plus
three isolated finalization cases through the Python lifecycle harness. Cargo
formatting, Clippy with warnings denied, Ruff, Python formatting, ty, pyupgrade,
and the scoped diff check pass. The release-only workflow was also verified
against GitHub's current default-branch file and its sole active workflow.

## CI and artifacts

The [CI workflow](../.github/workflows/ci.yml) has 16 combinations: CPython
3.11–3.14 on Ubuntu 24.04 x86_64, macOS 14 arm64, macOS 15 Intel, and
Windows 2022 x86_64. It checks Rust and Python, builds interpreter-specific
wheels from an sdist, and installs/tests each wheel in a fresh environment
outside the checkout. Actions are pinned to verified commit hashes.

Linux wheels target manylinux_2_28. maturin audits and repairs the wheel;
distribution checks require bundled libasound and its matching source RPM in
the download artifact. The third-party notices include the LGPL license and
describe replacement of the bundled shared library. macOS wheels use separate
architectures and system CoreMIDI frameworks. Windows wheels use WinMM.
Actual tags, native dependency inspection, and CI results must be recorded
before advertising the resulting artifacts as verified.

[Run 37787345883](https://github.com/rec/midirp/actions/runs/37787345883) passed all
16 jobs for code commit `1291242`. Each combination passed Rust checks, 34
Python unit checks including isolated finalization, two archive checks, and
33 installed-wheel contract checks outside the checkout. All 16 artifact
downloads were uploaded successfully. Linux archive checks confirm bundled
libasound and an accompanying source RPM. macOS jobs request deployment target
11.0; the local binary inspection below verifies that target on arm64.

The Windows runner initially selected Git's `link.exe` instead of the Microsoft
linker because its environment was too restricted. It now preserves the normal
compiler environment and overrides only the chosen Python executable and Cargo
cache in a scoped context. Subprocess env arguments stay implicit, avoiding
traceback dumps of inherited values. CI sets UV_PYTHON explicitly to prevent
the checkout's default 3.11 setting from changing interpreters mid-job.

The available GitHub credentials permit status inspection but deny log and
artifact downloads. The initial failure output came from the owner; subsequent
failures were readable through CI annotations. Distribution inspection in the
matrix runs on the runners. Local artifact inspection is recorded below.
CI now runs only on published GitHub releases. Pushes and pull requests do not
trigger the workflow.

The local CPython 3.11 wheel built from the sdist, passed both archive checks,
and passed all 33 installed-package checks in a fresh environment outside the
checkout. Its tag is `cp311-cp311-macosx_11_0_arm64`. Mach-O inspection confirms
minimum macOS 11.0 and links system CoreMIDI/CoreAudio/Foundation frameworks,
without a libpython dependency. CI requests the same macOS deployment baseline.

Opt-in archive checks:

```sh
uv run pytest test/release/artifacts.py -v
```

These verify Cargo/Python version agreement, absence of Python runtime
dependencies, native module placement, unchanged stubs, the typing marker,
third-party notices, and sdist source completeness. The sdist-to-wheel build
also checks that the archive can compile independently of the checkout.

## Callback function validation (A3), 2026-10-09

Both input opening methods reject coroutine, generator, and async generator
functions with `TypeError` before opening native input or starting delivery.
Checks cover ordinary functions, partials, callable objects, and partials of
callable objects in both isolation modes. Rejected callbacks leave the client
available. Synchronous functions, partials, and callable objects still deliver
the controlled fixture's message during opening.

All 73 Python checks pass, including the existing Rust lifecycle harness; Ruff,
formatting, ty, pyupgrade, and the scoped diff check pass. These are device-free
checks. Release-only CI was preserved; this push schedules no matrix run.
Synchronous wrappers returning deferred objects remain an application concern;
callback results are still ignored. Other A3 limitations remain in the inventory.

## Unsupported free-threaded build rejection (L6), 2026-10-09

Native lifecycle initialization now rejects free-threaded Python builds before
starting cleanup workers or registering exit cleanup, even if such a build has
temporarily enabled its GIL. Detection uses
[Python's documented build flag](https://docs.python.org/3/howto/free-threading-python.html#identifying-free-threaded-python),
`sysconfig.get_config_var("Py_GIL_DISABLED")`. This enforces the existing standard
GIL-enabled interpreter contract without new dependencies or build tooling.

A fresh-interpreter regression injects that flag, verifies the rejection before
exit registration and private-module publication, then successfully imports
after restoring the ordinary build flag. This tests the guard on standard
CPython, not a native free-threaded build or concurrency support.
All 74 Python checks, Cargo formatting, Clippy with warnings denied, Ruff,
Python formatting, ty, pyupgrade, and the scoped diff check pass.

## Bounded receive examples (R2), 2026-10-09

Both README receive examples now use a single-slot application queue and
nonblocking insertion. When occupied, they discard the newest message; each
example only needs its first received message. The callback cannot wait for
the consumer to free queue capacity, including during connection close.
This removes the examples' unbounded application buffering without changing
the library API or imposing a policy on applications. The remaining native
SysEx and library resource limits stay in the issue inventory.

Documentation review and the scoped diff check pass. No Python or data file
changed, so the test suite was not rerun for this documentation-only commit.

## Unhashable port typing (A4), 2026-10-09

Both public port stubs now declare `__hash__: ClassVar[None]`, matching their
existing runtime behavior. A focused type-checker probe returned each port as
`collections.abc.Hashable`: ty accepted both before the change and now rejects
both with incompatible `__hash__` diagnostics. The project type check still
passes. This establishes the corrected protocol surface with the existing ty
checker; it does not claim every checker diagnoses every direct `hash()` call.
All 74 Python checks, Ruff, formatting, ty, pyupgrade, and the scoped diff check
pass after the stub change.

## Mutable send buffers (A2), 2026-10-09

Public `send()` accepts bytes, bytearray, and memoryview, copying mutable buffers
and views into bytes before native work or serial waiting. Strided views retain
their logical contents; native and isolated paths receive the same snapshot.
Device-free checks cover all three added forms in both modes, including mutation
of the caller's buffer after the snapshot. All 80 Python checks, Ruff, formatting,
ty, pyupgrade, and the scoped diff check pass.

## Callback signature admission (A3), 2026-10-09

Opening rejects inspectable callbacks that cannot accept two positional
arguments, without invoking the callback or consuming client ownership.
Uninspectable callables remain accepted. Static `__call__` descriptors are
inspected through their actual callable to avoid inspect.signature dropping a
valid argument. Deferred-function checks retain their earlier error precedence.
Device-free checks exercise both opening methods and both modes for incompatible
signatures, plus valid callable objects, partials, and uninspectable callables.
All 83 Python checks, Ruff, formatting, ty, pyupgrade, and the scoped diff check pass.

## Context body and cleanup failures (A2), 2026-10-09

Public input/output context managers preserve simultaneous body and close errors
in an exception group, ordered body then close. Interrupts use BaseExceptionGroup;
ordinary errors use ExceptionGroup. Without a body error, close errors retain
their original exception type. Device-free checks cover both directions, both
modes, ordinary body errors, interrupts, and isolated close timeouts.
All 91 Python checks, Ruff, formatting, ty, pyupgrade, and the scoped diff check pass.

## Receive payload budgets (R2), 2026-10-09

`MidiInput(receive_byte_limit=8_388_608)` sets an 8 MiB default payload budget for
each isolated receive stage, alongside the existing 128-message limit. Both
stages reuse one queue implementation; admission and byte release occur under
its existing mutex. Oversized/full-budget messages are dropped without waiting
and counted. In-process mode adds no queue and does not apply this limit.

Device-free checks cover budget exhaustion, oversized single messages, capacity
recovery after dequeue, invalid settings, and configured child-stage drops.
All 98 Python checks, Ruff, formatting, ty, pyupgrade, and the scoped diff check
pass. Queue budgets exclude interpreter overhead, in-flight messages, native
unfinished SysEx, and application-retained messages; those R2 risks remain open.

## Binding error categories (A2/A4), 2026-10-09

The public API exports StateError, CallbackThreadError, NativePanicError,
ResourceError, WorkerError, and WorkerTimeoutError. They preserve the existing
RuntimeError/TimeoutError bases while distinguishing failures without parsing
messages. Native returned errors retain their four existing exception classes
and backend-specific detail. Worker replies preserve named categories.

Existing fault checks now assert actual state, callback-thread, thread-quota,
worker-exit, timeout, native-panic, and cleanup-capacity categories. Contract
checks cover all six public exception types and their bases. This does not
translate unreported backend events or infer device diagnoses from native text.
All 104 Python checks, Rust formatting, Clippy with warnings denied, Ruff,
Python formatting, ty, pyupgrade, and the scoped diff check pass.

## Worker failure and input activity observations (D1), 2026-10-09

Clients and connections expose worker_failure, retaining the first known worker
failure's type and arguments without its traceback. Fatal replies are recorded
before subsequent EOF can obscure their native error category. In-process mode
has no worker and reports None. No automatic reconnection or device-health
diagnosis was added.

Input last_message_time records Python monotonic seconds when callback delivery
starts, before application code, and survives close. Native and parent delivery
reuse the existing Rust callback gate, preserving the original callable in
sys.unraisablehook. The captured clock reference is GC-visible and released
outside the state lock during retirement. Queued/dropped messages do not advance
the observation. Device-free checks cover quiet input, early delivery, both
modes, callback error context, worker crash/timeout/native errors, and detached
failure tracebacks.
All 108 repository Python checks, Rust formatting, Clippy with warnings denied,
Ruff, Python formatting, ty, pyupgrade, and the scoped diff check pass. The
CPython 3.11 arm64 wheel rebuilt from the sdist passes both archive checks and
107 installed-wheel checks from outside the checkout. Ten focused, previously
authorized CoreMIDI virtual-port checks pass in both modes: successful/context
close, 128/1,024/16,384-byte SysEx including activity observations, and ordered
bursts. No hardware was used. These results do not establish WinMM/ALSA native
fault behavior, an upstream SysEx assembly bound, or device health. Release-only
CI remains unchanged and this push schedules no cross-platform run.

## Native backend checks

`test/manual/loopback.py` explicitly selects unique temporary software endpoints
by name and never opens an enumerated hardware device. It covers both virtual
directions, all eight filter masks and the default, exact note/CC/realtime/SysEx
bytes, timestamp type and monotonic ordering, duplicate names and distinct IDs,
missing/stale lookup, client reuse, context exceptions, and interpreter exit
with queued traffic. The shutdown case runs in a subprocess with a deadline.
Additional cases preserve 128-, 1,024-, and 16,384-byte SysEx messages after
close and check an ordered burst of 1,000 CC messages.

`test/manual/windows.py` checks unsupported virtual creation and preserved client
state without connecting hardware. It still initializes the native backend and
must be explicitly selected. Windows I/O requires a separately authorized,
explicitly chosen physical port or installed loopback driver.

| Backend | Recorded native I/O result |
| --- | --- |
| CoreMIDI | 25 software-loopback checks passed; macOS 14.5 arm64, CPython 3.11.7 |
| ALSA | Not run; requires a host with sequencer access |
| WinMM | Not run; requires a selected device/driver for I/O |

Native checks are selected locally with explicit authorization. Release CI
does not initialize an OS MIDI client. These checks do not
cover unplug/replug, driver failure, hardware timing, or a universal throughput
bound. The local 1,000-message CC burst measured about 165,567 messages/s;
send-to-callback median 0.174 ms, p99 0.692 ms, and maximum 0.753 ms. This is one
software-loopback sample under the host's current load, including GIL scheduling.
It is not a hardware latency measurement, timing guarantee, or loss threshold.

## Upstream close/drop review

Reviewed the pinned midir 0.11.0 backend sources, rather than assuming that
the three implementations have identical shutdown behavior:

- CoreMIDI input close takes the handler-data mutex and extracts its client/data;
  native port objects are disposed as the connection is consumed. The Python
  bridge retires and drains delivery before that native close.
- ALSA input close signals its pipe and joins the input thread before
  unsubscribing, freeing its timestamp queue, and deleting the port. Drop also
  uses this close path. It must not run on its input callback thread.
- WinMM input close takes its native handle lock, resets/stops input, unprepares
  and releases SysEx buffers, then closes the handle. Reset can invoke callbacks;
  the retired bridge refuses Python delivery during teardown. Output close/drop
  uses the backend's native handle cleanup.

The production cleanup worker and GIL release address these ownership and
callback constraints. Source review does not prove driver shutdown safety.

## Release gates

- Public API, stubs, byte ownership, exceptions, state restoration, concurrent
  close, GC, and finalization have device-free coverage.
- Package name `midirp`: PyPI JSON endpoint returned HTTP 404 on 2026-10-08.
  This is a point-in-time check, not a reservation or guarantee of availability.
- Project license: MIT, explicitly chosen by the owner. LICENSE and the
  third-party notices are included in source and wheel distributions.
- Cross-platform artifact validation and native I/O results remain separate gates.
- No publication credentials, publish workflow, release, or tag were added.

## Port identity mitigation (D2), 2026-10-09

Public ID lookup now enumerates both modes and rejects duplicate matches with
`PortInfoError`; a missing ID still returns `None`. CoreMIDI ID `"0"` is rejected
when reading a handle, looking up an ID, reading metadata, or opening. Isolated
worker discovery and resolution apply the same sentinel guard. Linux/Windows
identifiers are not interpreted as CoreMIDI IDs. Focused device-free regressions
cover both directions, both modes, unique/missing/duplicate lookup, and the
platform-specific sentinel, including rejection before native metadata/open.
The remaining D2 text describes reused IDs, stale equality, and backend metadata
limits that this mitigation cannot remove.

All 120 repository tests pass, including the native lifecycle harness. Ruff,
formatting, ty, pyupgrade, and the scoped diff check pass. No native MIDI ports
were opened. Release-only CI remains unchanged.

## Deferred callback result mitigation (A3), 2026-10-09

The shared Rust callback bridge now reports returned coroutines, generators,
async generators, and other awaitables as `TypeError` through
`sys.unraisablehook`, with the original callable as context. Ordinary results
remain ignored; later deliveries continue. The check runs in the existing
callback gate without a callable wrapper. Its cached type references are tracked
for GC and retired outside the state lock. Only unstarted native coroutines are
closed to avoid duplicate unawaited-coroutine warnings; suspended coroutines
and other deferred objects are not executed or cancelled.

Device-free tests cover all four deferred result forms through public input
connections in both modes, original error context, subsequent delivery, no
deferred execution, and coroutine warning suppression. A further regression
checks that a suspended coroutine is reported without cancellation. Resolved
silent-deferred-result text is removed from A3; callback signature uncertainty,
thread constraints, and application synchronization remain documented.

All 129 repository tests pass, including 39 ordinary Rust tests and three
isolated finalization checks through the Python harness. Ruff, formatting, ty,
pyupgrade, Cargo formatting, Clippy with warnings denied, and the scoped diff
check pass. No native MIDI ports were opened. The workflow still contains only
the published-release trigger; no cross-platform CI run was scheduled.

## CPython 3.15 compatibility, 2026-10-10

Host: macOS 14.5 arm64; Homebrew standard GIL-enabled CPython 3.15.0;
Rust 1.99.0; pinned midir 0.11.0, PyO3 0.29.3, and maturin 1.15.0.
Separate environments preserve the checkout's Python 3.11 default.
`pyproject.toml` now declares the Python 3.15 classifier; `requires-python`
remains `>=3.11`. No dependency versions, lockfile, Python minimum, or release
workflow changed. The release matrix still covers 3.11–3.14 and runs only on
published releases.

All 129 repository tests pass under 3.15, including 39 ordinary Rust tests and
three isolated finalization checks through the Python lifecycle harness. A
release wheel built from the source archive passes all 128 applicable tests
outside the checkout. Wheel/source metadata checks confirm the 3.15 classifier,
Python minimum, lack of runtime dependencies, and current packaged Python
sources/stubs. The artifact is
`midirp-0.1.0-cp315-cp315-macosx_11_0_arm64.whl`.

Ruff, formatting, ty, pyupgrade with the retained 3.11 minimum, Cargo formatting,
and the scoped diff check pass. Strict Clippy with Rust 1.99 fails on the existing
`AtomicU8::fetch_update` deprecation in `src/lifecycle.rs:104`; the successful
release build reports the same warning. This is a compiler-version finding,
not a Python 3.15 build failure. No warning suppression, Rust-version increase,
or unrelated implementation change was made.

Eight authorized software-only CoreMIDI close/reopen and SysEx checks were
attempted against the installed 3.15 wheel in both execution modes. All failed
at client construction with `InitError`, before any virtual endpoint was
created. The two close/reopen comparison cases also fail at initialization
under the checkout's CPython 3.11.7. This does not establish a 3.15-specific
defect; the native initialization failure's cause remains undiagnosed. Native
MIDI I/O under 3.15, Linux/Windows 3.15 builds, and cross-platform 3.15 fault
behavior remain unvalidated. No hardware was accessed.
