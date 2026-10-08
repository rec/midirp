use std::sync::Arc;

use pyo3::prelude::*;
use pyo3::{PyTraverseError, PyVisit};

use crate::callback::{check_close_thread, Callback};
use crate::errors::{InitError, PortInfoError};
use crate::lifecycle::{self, Managed, Resource};
use crate::state::Client;

/// A MIDI input client that is unavailable while its connection is open.
#[pyclass(frozen, module = "midirp.midi")]
pub struct MidiInput {
    state: Arc<Client<midir::MidiInput>>,
}

/// An opaque input-port handle obtained from discovery.
#[pyclass(frozen, module = "midirp.midi")]
pub struct MidiInputPort {
    native: midir::MidiInputPort,
}

#[pymethods]
impl MidiInputPort {
    /// The backend's opaque identifier; no additional persistence guarantee.
    fn id(&self, py: Python<'_>) -> String {
        py.detach(|| self.native.id())
    }

    fn __eq__(&self, py: Python<'_>, other: &Self) -> bool {
        py.detach(|| self.native == other.native)
    }

    fn __ne__(&self, py: Python<'_>, other: &Self) -> bool {
        py.detach(|| self.native != other.native)
    }

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
            state: Arc::new(Client::new(native)),
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

    fn find_port_by_id(&self, py: Python<'_>, id: &str) -> PyResult<Option<MidiInputPort>> {
        py.detach(|| {
            self.state.with_available(|native| {
                Ok(native
                    .find_port_by_id(id)
                    .map(|native| MidiInputPort { native }))
            })
        })
    }

    fn connect(
        slf: Py<Self>,
        py: Python<'_>,
        port: &MidiInputPort,
        port_name: &str,
        callback: Py<PyAny>,
    ) -> PyResult<Py<MidiInputConnection>> {
        let callback = Arc::new(Callback::new(py, callback)?);
        let resource = Arc::new(Managed::new(
            Arc::clone(&slf.get().state),
            Some(Arc::clone(&callback)),
            |native: midir::MidiInputConnection<()>| native.close().0,
        ));
        let connection = Py::new(
            py,
            MidiInputConnection {
                resource: Arc::clone(&resource),
                client: slf,
            },
        )?;
        lifecycle::register(resource.clone())?;
        py.detach(|| {
            resource.connect(|native| {
                native.connect(
                    &port.native,
                    port_name,
                    move |timestamp, bytes, ()| callback.deliver(timestamp, bytes),
                    (),
                )
            })
        })?;
        Ok(connection)
    }
}

/// Owns the single GC-visible callable and its original Python client.
#[pyclass(frozen, weakref, module = "midirp.midi")]
pub struct MidiInputConnection {
    resource: Arc<Managed<midir::MidiInput, midir::MidiInputConnection<()>>>,
    client: Py<MidiInput>,
}

#[pymethods]
impl MidiInputConnection {
    fn close(&self, py: Python<'_>) -> PyResult<()> {
        check_close_thread()?;
        self.resource.retire();
        py.detach(|| self.resource.close());
        Ok(())
    }

    #[getter]
    fn closed(&self, py: Python<'_>) -> bool {
        py.detach(|| self.resource.native.closed())
    }

    fn __enter__(slf: Py<Self>, py: Python<'_>) -> PyResult<Py<Self>> {
        if slf.get().closed(py) {
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
    ) -> PyResult<()> {
        self.close(py)
    }

    fn __traverse__(&self, visit: PyVisit<'_>) -> Result<(), PyTraverseError> {
        visit.call(&self.client)?;
        self.resource
            .callback
            .as_ref()
            .expect("input callback missing")
            .traverse(visit)
    }

    fn __clear__(&self) {
        lifecycle::defer(self.resource.clone());
    }
}

impl Drop for MidiInputConnection {
    fn drop(&mut self) {
        lifecycle::defer(self.resource.clone());
    }
}
