use pyo3::prelude::*;

use crate::errors::{InitError, PortInfoError};
use crate::state::Client;

/// A native MIDI input client. Callback connections are not implemented yet.
#[pyclass(frozen, module = "midirp.midi")]
pub struct MidiInput {
    state: Client<midir::MidiInput>,
}

/// An opaque input-port handle obtained from discovery.
#[pyclass(frozen, module = "midirp.midi")]
pub struct MidiInputPort {
    native: midir::MidiInputPort,
}

#[pymethods]
impl MidiInputPort {
    #[classattr]
    const __hash__: Option<Py<PyAny>> = None;
}

#[pymethods]
impl MidiInput {
    #[new]
    fn new(py: Python<'_>, client_name: &str) -> PyResult<Self> {
        let native = py
            .detach(|| midir::MidiInput::new(client_name))
            .map_err(|error| InitError::new_err(format!("create input: {error}")))?;
        Ok(Self {
            state: Client::new(native),
        })
    }

    fn ports(&self, py: Python<'_>) -> PyResult<Vec<MidiInputPort>> {
        py.detach(|| {
            self.state.with_available(|native| {
                Ok(native
                    .ports()
                    .into_iter()
                    .map(|native| MidiInputPort { native })
                    .collect())
            })
        })
    }

    fn port_name(&self, py: Python<'_>, port: &MidiInputPort) -> PyResult<String> {
        py.detach(|| {
            self.state.with_available(|native| {
                native
                    .port_name(&port.native)
                    .map_err(|error| PortInfoError::new_err(format!("input port name: {error}")))
            })
        })
    }
}
