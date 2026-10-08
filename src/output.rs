use pyo3::prelude::*;

use crate::errors::{InitError, PortInfoError};
use crate::state::{Client, Connection};

/// A MIDI output client that is unavailable while its connection is open.
#[pyclass(frozen, module = "midirp.midi")]
pub struct MidiOutput {
    state: Client<midir::MidiOutput>,
}

/// An opaque output-port handle obtained from discovery.
#[pyclass(frozen, module = "midirp.midi")]
pub struct MidiOutputPort {
    native: midir::MidiOutputPort,
}

#[pymethods]
impl MidiOutputPort {
    #[classattr]
    const __hash__: Option<Py<PyAny>> = None;
}

/// Owns a native output connection and keeps its original Python client alive.
#[pyclass(frozen, module = "midirp.midi")]
pub struct MidiOutputConnection {
    native: Connection<midir::MidiOutputConnection>,
    client: Py<MidiOutput>,
}

#[pymethods]
impl MidiOutput {
    #[new]
    fn new(py: Python<'_>, client_name: &str) -> PyResult<Self> {
        let native = py
            .detach(|| midir::MidiOutput::new(client_name))
            .map_err(|error| InitError::new_err(format!("create output: {error}")))?;
        Ok(Self {
            state: Client::new(native),
        })
    }

    fn ports(&self, py: Python<'_>) -> PyResult<Vec<MidiOutputPort>> {
        py.detach(|| {
            self.state.with_available(|native| {
                Ok(native
                    .ports()
                    .into_iter()
                    .map(|native| MidiOutputPort { native })
                    .collect())
            })
        })
    }

    fn port_name(&self, py: Python<'_>, port: &MidiOutputPort) -> PyResult<String> {
        py.detach(|| {
            self.state.with_available(|native| {
                native
                    .port_name(&port.native)
                    .map_err(|error| PortInfoError::new_err(format!("output port name: {error}")))
            })
        })
    }

    fn connect(
        slf: Py<Self>,
        py: Python<'_>,
        port: &MidiOutputPort,
        port_name: &str,
    ) -> PyResult<Py<MidiOutputConnection>> {
        // Allocate first: a Python allocation failure must not consume the client.
        let connection = Py::new(
            py,
            MidiOutputConnection {
                native: Connection::new(),
                client: slf.clone_ref(py),
            },
        )?;
        py.detach(|| {
            slf.get().state.connect(&connection.get().native, |native| {
                native.connect(&port.native, port_name)
            })
        })?;
        Ok(connection)
    }
}

#[pymethods]
impl MidiOutputConnection {
    /// Close once and restore the original client. Waits for concurrent close.
    fn close(&self, py: Python<'_>) {
        py.detach(|| {
            self.native
                .close(&self.client.get().state, midir::MidiOutputConnection::close)
        });
    }

    #[getter]
    fn closed(&self, py: Python<'_>) -> bool {
        py.detach(|| self.native.closed())
    }

    fn __enter__(slf: Py<Self>, py: Python<'_>) -> PyResult<Py<Self>> {
        if py.detach(|| slf.get().native.closed()) {
            return Err(pyo3::exceptions::PyRuntimeError::new_err(
                "MIDI connection is closed",
            ));
        }
        Ok(slf)
    }

    fn __exit__(
        &self,
        py: Python<'_>,
        _exc_type: &Bound<'_, PyAny>,
        _exc_value: &Bound<'_, PyAny>,
        _traceback: &Bound<'_, PyAny>,
    ) {
        self.close(py);
    }
}

impl Drop for MidiOutputConnection {
    fn drop(&mut self) {
        // There are no Python input callbacks yet. Native output teardown does
        // not attach to Python and restores the client on ordinary destruction.
        self.native
            .close(&self.client.get().state, midir::MidiOutputConnection::close);
    }
}
