use std::io::{self, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};

use pyo3::create_exception;
use pyo3::exceptions::{PyException, PyRuntimeError, PyTimeoutError};
use pyo3::PyResult;

/// Callers must invalidate affected ownership when a native call unwinds.
pub fn native_call<T>(operation: &str, call: impl FnOnce() -> T) -> PyResult<T> {
    catch_unwind(AssertUnwindSafe(call)).map_err(|payload| {
        let detail = payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| payload.downcast_ref::<&str>().copied())
            .unwrap_or("unknown panic");
        NativePanicError::new_err(format!(
            "native MIDI {operation} panicked; recreate affected resources: {detail}"
        ))
    })
}

pub fn failed_error() -> pyo3::PyErr {
    StateError::new_err("MIDI resource failed; create a new client")
}

/// Diagnostics must not turn cleanup failures into another unwind.
pub fn report_failure(message: &str) {
    let _ = writeln!(io::stderr().lock(), "midirp: {message}");
}

create_exception!(
    midirp.midi,
    MidiError,
    PyException,
    "A native MIDI operation failed."
);
create_exception!(
    midirp.midi,
    InitError,
    MidiError,
    "The MIDI backend could not initialize."
);
create_exception!(
    midirp.midi,
    PortInfoError,
    MidiError,
    "Port information could not be retrieved."
);
create_exception!(
    midirp.midi,
    ConnectError,
    MidiError,
    "A MIDI connection could not be opened."
);
create_exception!(
    midirp.midi,
    SendError,
    MidiError,
    "A MIDI message could not be sent."
);

create_exception!(
    midirp.midi,
    StateError,
    PyRuntimeError,
    "MIDI ownership state prevents this operation."
);
create_exception!(
    midirp.midi,
    CallbackThreadError,
    PyRuntimeError,
    "A blocking MIDI operation was attempted from a callback."
);
create_exception!(
    midirp.midi,
    NativePanicError,
    PyRuntimeError,
    "A recoverable native MIDI panic occurred."
);
create_exception!(
    midirp.midi,
    ResourceError,
    PyRuntimeError,
    "MIDI lifecycle resources could not be allocated."
);
create_exception!(
    midirp.midi,
    WorkerError,
    PyRuntimeError,
    "The MIDI worker exited or communication failed."
);
create_exception!(
    midirp.midi,
    WorkerTimeoutError,
    PyTimeoutError,
    "A MIDI worker operation exceeded its deadline."
);
