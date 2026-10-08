use std::sync::Arc;

#[cfg(unix)]
use midir::os::unix::VirtualOutput;
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use pyo3::{PyTraverseError, PyVisit};

use crate::callback::check_close_thread;
use crate::errors::{InitError, PortInfoError};
use crate::lifecycle::{self, Managed, Resource};
use crate::state::Client;

/// A MIDI output client that is unavailable while its connection is open.
#[pyclass(frozen, module = "midirp.midi")]
pub struct MidiOutput {
    state: Arc<Client<midir::MidiOutput>>,
}

/// An opaque output-port handle obtained from discovery.
#[pyclass(frozen, module = "midirp.midi")]
pub struct MidiOutputPort {
    native: midir::MidiOutputPort,
}

#[pymethods]
impl MidiOutputPort {
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

/// Owns a native output connection and keeps its original Python client alive.
#[pyclass(frozen, module = "midirp.midi")]
pub struct MidiOutputConnection {
    resource: Arc<Managed<midir::MidiOutput, midir::MidiOutputConnection>>,
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
            state: Arc::new(Client::new(native)),
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

    fn find_port_by_id(&self, py: Python<'_>, id: &str) -> PyResult<Option<MidiOutputPort>> {
        py.detach(|| {
            self.state.with_available(|native| {
                Ok(native
                    .find_port_by_id(id)
                    .map(|native| MidiOutputPort { native }))
            })
        })
    }

    fn connect(
        slf: Py<Self>,
        py: Python<'_>,
        port: &MidiOutputPort,
        port_name: &str,
    ) -> PyResult<Py<MidiOutputConnection>> {
        MidiOutputConnection::open(py, slf, |native| native.connect(&port.native, port_name))
    }

    /// Send messages to applications connected to this virtual output.
    fn create_virtual(
        slf: Py<Self>,
        py: Python<'_>,
        port_name: &str,
    ) -> PyResult<Py<MidiOutputConnection>> {
        #[cfg(unix)]
        {
            MidiOutputConnection::open(py, slf, |native| native.create_virtual(port_name))
        }
        #[cfg(not(unix))]
        {
            let _ = (slf, py, port_name);
            Err(pyo3::exceptions::PyNotImplementedError::new_err(
                "Virtual MIDI output ports are not supported on this platform",
            ))
        }
    }
}

#[pymethods]
impl MidiOutputConnection {
    /// Send immutable bytes unchanged, serialized with native close.
    fn send(&self, py: Python<'_>, message: &Bound<'_, PyBytes>) -> PyResult<()> {
        self.resource
            .native
            .send(py, message, midir::MidiOutputConnection::send)
    }

    /// Close once and restore the original client. Waits for concurrent close.
    fn close(&self, py: Python<'_>) -> PyResult<()> {
        check_close_thread()?;
        py.detach(|| self.resource.close());
        Ok(())
    }

    #[getter]
    fn closed(&self, py: Python<'_>) -> bool {
        py.detach(|| self.resource.native.closed())
    }

    fn __enter__(slf: Py<Self>, py: Python<'_>) -> PyResult<Py<Self>> {
        if py.detach(|| slf.get().resource.native.closed()) {
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
        visit.call(&self.client)
    }

    fn __clear__(&self) {
        lifecycle::defer(self.resource.clone());
    }
}

impl MidiOutputConnection {
    fn open(
        py: Python<'_>,
        client: Py<MidiOutput>,
        operation: impl FnOnce(
                midir::MidiOutput,
            )
                -> Result<midir::MidiOutputConnection, midir::ConnectError<midir::MidiOutput>>
            + Send,
    ) -> PyResult<Py<Self>> {
        // Allocate first: a Python allocation failure must not consume the client.
        let connection = Py::new(
            py,
            Self {
                resource: Arc::new(Managed::new(
                    Arc::clone(&client.get().state),
                    None,
                    midir::MidiOutputConnection::close,
                )),
                client,
            },
        )?;
        lifecycle::register(connection.get().resource.clone())?;
        py.detach(|| connection.get().resource.connect(operation))?;
        Ok(connection)
    }
}

impl Drop for MidiOutputConnection {
    fn drop(&mut self) {
        lifecycle::defer(self.resource.clone());
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::path::Path;
    use std::sync::Mutex;

    use super::*;
    use crate::state::Connection;

    /// Records calls through the production bytes boundary and connection gate.
    /// No MIDI backend is created or simulated.
    #[pyclass(frozen)]
    pub(crate) struct OutputDriver {
        connection: Connection<()>,
        client: Client<()>,
        pub(crate) messages: Mutex<Vec<Vec<u8>>>,
    }

    #[pymethods]
    impl OutputDriver {
        fn send(&self, py: Python<'_>, message: &Bound<'_, PyBytes>) -> PyResult<()> {
            self.connection.send(py, message, |(), bytes| {
                self.messages.lock().unwrap().push(bytes.to_vec());
                Ok(())
            })
        }

        fn close(&self, py: Python<'_>) -> PyResult<()> {
            check_close_thread()?;
            py.detach(|| self.connection.close(&self.client, |()| ()));
            Ok(())
        }
    }

    impl OutputDriver {
        pub(crate) fn new(py: Python<'_>) -> Py<Self> {
            let driver = Py::new(
                py,
                Self {
                    connection: Connection::new(),
                    client: Client::new(()),
                    messages: Mutex::new(Vec::new()),
                },
            )
            .unwrap();
            driver
                .get()
                .client
                .connect(&driver.get().connection, Ok)
                .unwrap();
            driver
        }
    }

    #[test]
    fn send_rejects_mutable_buffers_and_other_non_bytes_arguments() {
        Python::initialize();
        Python::attach(|py| {
            let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("test/native");
            PyModule::import(py, "sys")
                .unwrap()
                .getattr("path")
                .unwrap()
                .call_method1("insert", (0, path.to_str().unwrap()))
                .unwrap();
            let driver = OutputDriver::new(py);
            PyModule::import(py, "midirp_callbacks")
                .unwrap()
                .call_method1("verify_send_boundary", (driver.bind(py),))
                .unwrap();
            assert_eq!(
                *driver.get().messages.lock().unwrap(),
                vec![vec![0x90, 60, 127]]
            );
            driver.get().close(py).unwrap();
            let error = driver
                .bind(py)
                .call_method1("send", (PyBytes::new(py, &[0xf8]),))
                .unwrap_err();
            assert!(error.is_instance_of::<pyo3::exceptions::PyRuntimeError>(py));
        });
    }
}
