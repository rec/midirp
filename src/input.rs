use std::sync::Arc;

#[cfg(unix)]
use midir::os::unix::VirtualInput;
use pyo3::prelude::*;
use pyo3::{PyTraverseError, PyVisit};

use crate::callback::{check_blocking_thread, Callback};
use crate::errors::{native_call, InitError, PortInfoError};
use crate::ignore::Ignore;
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
    fn id(&self, py: Python<'_>) -> PyResult<String> {
        check_blocking_thread()?;
        py.detach(|| native_call("port ID", || self.native.id()))
    }

    fn __eq__(&self, py: Python<'_>, other: &Self) -> PyResult<bool> {
        check_blocking_thread()?;
        py.detach(|| native_call("port comparison", || self.native == other.native))
    }

    fn __ne__(&self, py: Python<'_>, other: &Self) -> PyResult<bool> {
        check_blocking_thread()?;
        py.detach(|| native_call("port comparison", || self.native != other.native))
    }

    #[classattr]
    const __hash__: Option<Py<PyAny>> = None;
}

#[pymethods]
impl MidiInput {
    #[new]
    fn new(py: Python<'_>, client_name: &str) -> PyResult<Self> {
        check_blocking_thread()?;
        let native = py
            .detach(|| native_call("create input", || midir::MidiInput::new(client_name)))?
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

    fn ignore(&self, py: Python<'_>, flags: &Ignore) -> PyResult<()> {
        py.detach(|| {
            self.state.with_available(|native| {
                native.ignore(flags.native);
                Ok(())
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
        MidiInputConnection::open(py, slf, callback, |native, callback| {
            native.connect(
                &port.native,
                port_name,
                move |timestamp, bytes, ()| callback.deliver(timestamp, bytes),
                (),
            )
        })
    }

    /// Receive messages that other applications send to this virtual input.
    fn create_virtual(
        slf: Py<Self>,
        py: Python<'_>,
        port_name: &str,
        callback: Py<PyAny>,
    ) -> PyResult<Py<MidiInputConnection>> {
        check_blocking_thread()?;
        #[cfg(unix)]
        {
            MidiInputConnection::open(py, slf, callback, |native, callback| {
                native.create_virtual(
                    port_name,
                    move |timestamp, bytes, ()| callback.deliver(timestamp, bytes),
                    (),
                )
            })
        }
        #[cfg(not(unix))]
        {
            let _ = (slf, py, port_name, callback);
            Err(pyo3::exceptions::PyNotImplementedError::new_err(
                "Virtual MIDI input ports are not supported on this platform",
            ))
        }
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
        check_blocking_thread()?;
        self.resource.retire();
        py.detach(|| self.resource.close())
    }

    #[getter]
    fn closed(&self, py: Python<'_>) -> bool {
        py.detach(|| self.resource.native.closed())
    }

    fn __enter__(slf: Py<Self>, py: Python<'_>) -> PyResult<Py<Self>> {
        slf.get().resource.native.check_failed()?;
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

impl MidiInputConnection {
    fn open(
        py: Python<'_>,
        client: Py<MidiInput>,
        callable: Py<PyAny>,
        operation: impl FnOnce(
                midir::MidiInput,
                Arc<Callback>,
            )
                -> Result<midir::MidiInputConnection<()>, midir::ConnectError<midir::MidiInput>>
            + Send,
    ) -> PyResult<Py<Self>> {
        check_blocking_thread()?;
        let callback = Arc::new(Callback::new(py, callable)?);
        let resource = Arc::new(Managed::new(
            Arc::clone(&client.get().state),
            Some(Arc::clone(&callback)),
            |native: midir::MidiInputConnection<()>| native.close().0,
        ));
        let connection = Py::new(
            py,
            Self {
                resource: Arc::clone(&resource),
                client,
            },
        )?;
        lifecycle::register(resource.clone())?;
        py.detach(|| resource.connect(|native| operation(native, callback)))?;
        Ok(connection)
    }
}

impl Drop for MidiInputConnection {
    fn drop(&mut self) {
        lifecycle::defer(self.resource.clone());
    }
}
