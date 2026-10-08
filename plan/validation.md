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
