# Optional native MIDI process isolation

## Agreed behavior

`MidiInput(name, *, isolated=True, timeout=5.0)` and the corresponding
`MidiOutput` constructor default to one child process per client. The ownership
model allows one live connection per client; input and output ports of the same
physical device therefore use separate processes. `isolated=False` keeps native
MIDI in the caller's process. No automatic physical-device grouping is inferred.

Each isolated native operation has a configurable positive finite deadline.
Timeout, child exit, or a native error invalidates the affected client and
connection and terminates the child. Reopening requires a new client/worker.
Neither reconnection nor uncertain sends are retried automatically. Expected
argument, busy-client, and unsupported-platform errors do not invalidate it.
Silence does not establish that a device disconnected; no health guarantee or
hotplug detection is invented.

Callbacks remain in the parent process on a dispatch thread. Both child and
parent receive queues each hold 128 messages and have fixed capacities and drop new messages on overflow.
The input connection exposes `dropped_messages`, including both queue stages.
Capacity counts messages, not bytes; exceptionally large messages still use
memory. Native timestamps and bytes are forwarded unchanged. The existing Rust
callback gate supplies exception reporting, callback-thread restrictions,
callable retirement, and draining in the parent. User callbacks are not pickled.

Explicit close retires incoming delivery and waits for admitted parent callbacks
as well as native teardown. The native-operation timeout cannot cancel arbitrary
parent Python code. Destruction schedules cleanup without waiting for a driver. Client disposal
requests graceful child shutdown before forced termination on its deadline.
Interpreter exit stops isolated children even when their drivers cannot close;
process termination does not reset hardware notes or repair an OS driver.

The worker is started with a fresh Python interpreter and a module entry point,
so applications do not need multiprocessing's guarded-main convention. Parent
and child use private local pipes, not network I/O. Communication uses only the
standard library and sends messages, identifiers, and error descriptions, never
native handles or user callables. No new dependencies or version changes.

## API and identity

Keep the public compiled `midirp.midi` module, error classes, and `Ignore` values.
Register the original native client/port/connection classes in an internal native
module; install one public Python ownership facade supporting both modes.
Keep `__init__.py` empty and retain the existing native implementation.

Isolated port handles contain direction and opaque ID snapshots; their equality
compares these values. Native-mode handles use native equality. Opening an
isolated connection resolves the ID against current discovery and rejects absent
or ambiguous matches. IDs are not authenticated physical-device identities and
may change or be reused; users must deliberately rediscover after failure.

## Implementation and verification

1. Expose the internal native module and reusable parent callback gate without
   changing native ownership behavior.
2. Add deadline-bounded worker communication, bounded receive delivery, child
   invalidation/reaping, and interpreter-exit cleanup.
3. Install the unified public facade, constructor settings, dropped-message
   counter, and typed API. Preserve native-mode behavior and validate arguments
   before initializing native resources.
4. Exercise transport failures, timeout, overflow accounting, callback gating,
   ownership, and resource cleanup using controlled subprocess fixtures that do
   not open OS MIDI devices. Run required Python/Rust checks and packaging checks.
5. Run the previously authorized CoreMIDI software-only loopback checks in both
   modes. Cross-platform CI verifies builds, portable logic, and wheel contents;
   physical hotplug and native ALSA/WinMM I/O remain separate validation gaps.
6. Update usage and outstanding issues in the last implementation commit. Remove
   only resolved issue text; retain parent-callback, driver, interpreter, and
   device-health limitations.

## Additional work beyond the prompt

None.
