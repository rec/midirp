//! One native cleanup worker and a weak resource registry for the main interpreter.

use std::sync::{mpsc, Arc, Mutex, OnceLock, Weak};
use std::thread::{self, JoinHandle};

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

use crate::callback::Callback;
use crate::state::{Client, Connection};

pub trait Resource: Send + Sync {
    fn retire(&self);
    fn close(&self);
}

/// Separates native client ownership from the Python client's lifetime.
/// The callable's one Python reference is
/// visited by its Python connection; native Arc clones do not duplicate it.
pub struct Managed<C, N> {
    pub client: Arc<Client<C>>,
    pub native: Connection<N>,
    pub callback: Option<Arc<Callback>>,
    opening: Mutex<bool>,
    teardown: fn(N) -> C,
}

impl<C, N> Managed<C, N> {
    pub fn new(
        client: Arc<Client<C>>,
        callback: Option<Arc<Callback>>,
        teardown: fn(N) -> C,
    ) -> Self {
        Self {
            client,
            native: Connection::new(),
            callback,
            opening: Mutex::new(false),
            teardown,
        }
    }

    pub fn connect(
        &self,
        operation: impl FnOnce(C) -> Result<N, midir::ConnectError<C>>,
    ) -> PyResult<()> {
        // Shutdown can see a registered connection before native connect returns.
        // Publish the handle and restore failures before allowing teardown.
        let closed = self.opening.lock().expect("opening lock poisoned");
        if *closed {
            return Err(PyRuntimeError::new_err(
                "MIDI connection was closed before opening completed",
            ));
        }
        self.client.connect(&self.native, operation)
    }
}

impl<C: Send, N: Send> Resource for Managed<C, N> {
    fn retire(&self) {
        if let Some(callback) = &self.callback {
            callback.retire();
        }
    }

    fn close(&self) {
        let mut closed = self.opening.lock().expect("opening lock poisoned");
        *closed = true;
        if let Some(callback) = &self.callback {
            callback.drain();
        }
        self.native.close(&self.client, self.teardown);
    }
}

pub fn initialize(py: Python<'_>) -> PyResult<()> {
    // SAFETY: the caller holds the GIL in a live interpreter. The process-wide
    // coordinator is deliberately restricted to CPython's main interpreter.
    if unsafe { pyo3::ffi::PyInterpreterState_GetID(pyo3::ffi::PyInterpreterState_Get()) } != 0 {
        return Err(PyRuntimeError::new_err(
            "midirp does not support subinterpreters",
        ));
    }
    if RUNTIME.get().is_none() {
        let (sender, receiver) = mpsc::channel::<Option<Arc<dyn Resource>>>();
        let worker = thread::Builder::new()
            .name("midirp cleanup".into())
            .spawn(move || {
                while let Ok(Some(resource)) = receiver.recv() {
                    resource.close();
                }
            })
            .map_err(|error| {
                PyRuntimeError::new_err(format!("start MIDI cleanup worker: {error}"))
            })?;
        let runtime = Runtime {
            registry: Mutex::new(Registry {
                closing: false,
                resources: Vec::new(),
                sender,
            }),
            worker: Mutex::new(Some(worker)),
        };
        let registration = PyModule::import(py, "atexit")
            .and_then(|atexit| atexit.call_method1("register", (wrap_pyfunction!(shutdown, py)?,)));
        if let Err(error) = registration {
            // A failed import must not leave an unregistered cleanup worker.
            runtime
                .registry
                .lock()
                .expect("registry lock poisoned")
                .sender
                .send(None)
                .expect("cleanup worker stopped early");
            py.detach(|| {
                runtime
                    .worker
                    .lock()
                    .expect("worker lock poisoned")
                    .take()
                    .unwrap()
                    .join()
                    .expect("cleanup worker panicked")
            });
            return Err(error);
        }
        // Initialization is serialized by the GIL; free-threaded builds are not supported.
        RUNTIME.set(runtime).unwrap_or_else(|_| unreachable!());
    }
    Ok(())
}

pub fn register(resource: Arc<dyn Resource>) -> PyResult<()> {
    let mut registry = runtime().registry.lock().expect("registry lock poisoned");
    if registry.closing {
        return Err(PyRuntimeError::new_err(
            "MIDI interpreter shutdown has begun",
        ));
    }
    registry.resources.retain(|r| r.strong_count() != 0);
    registry.resources.push(Arc::downgrade(&resource));
    Ok(())
}

/// Never waits for native work, including when called by GC or a MIDI callback.
pub fn defer(resource: Arc<dyn Resource>) {
    resource.retire();
    let registry = runtime().registry.lock().expect("registry lock poisoned");
    if !registry.closing {
        registry
            .sender
            .send(Some(resource))
            .expect("cleanup worker stopped early");
    }
    // During shutdown the registry's snapshot already owns every live resource.
}

#[pyfunction]
fn shutdown(py: Python<'_>) -> PyResult<()> {
    crate::callback::check_close_thread()?;
    let resources = {
        let mut registry = runtime().registry.lock().expect("registry lock poisoned");
        if registry.closing {
            return Ok(());
        }
        registry.closing = true;
        registry
            .resources
            .drain(..)
            .filter_map(|r| r.upgrade())
            .collect::<Vec<_>>()
    };
    // Disable EVERY gate while attached before waiting for any one callback.
    // Clearing the callable here leaves queued/native cleanup with no Python owners.
    for resource in &resources {
        resource.retire();
    }
    py.detach(|| {
        for resource in resources {
            resource.close();
        }
        runtime()
            .registry
            .lock()
            .expect("registry lock poisoned")
            .sender
            .send(None)
            .expect("cleanup worker stopped early");
        if let Some(worker) = runtime()
            .worker
            .lock()
            .expect("worker lock poisoned")
            .take()
        {
            worker.join().expect("cleanup worker panicked");
        }
    });
    Ok(())
}

fn runtime() -> &'static Runtime {
    RUNTIME.get().expect("MIDI lifecycle not initialized")
}

struct Runtime {
    registry: Mutex<Registry>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

struct Registry {
    closing: bool,
    resources: Vec<Weak<dyn Resource>>,
    sender: mpsc::Sender<Option<Arc<dyn Resource>>>,
}

static RUNTIME: OnceLock<Runtime> = OnceLock::new();

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn teardown_before_native_open_prevents_late_connection_creation() {
        let client = Arc::new(Client::new(37));
        let resource = Managed::new(Arc::clone(&client), None, |native: i32| native);
        resource.close();
        assert!(resource
            .connect(|_| panic!("closed resource must not open"))
            .is_err());
        assert!(resource.native.closed());
        assert_eq!(client.with_available(|native| Ok(*native)).unwrap(), 37);
    }
    #[test]
    fn teardown_waits_for_an_open_in_progress_and_restores_the_client() {
        use std::time::Duration;
        let client = Arc::new(Client::new(37));
        let resource = Managed::new(Arc::clone(&client), None, |native: i32| native);
        let (started, opening) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let (closing, closing_started) = mpsc::channel();
        let (done, completed) = mpsc::channel();
        thread::scope(|scope| {
            scope.spawn(|| {
                resource
                    .connect(move |native| {
                        started.send(()).unwrap();
                        released.recv_timeout(Duration::from_secs(5)).unwrap();
                        Ok(native)
                    })
                    .unwrap()
            });
            opening.recv_timeout(Duration::from_secs(5)).unwrap();
            scope.spawn(|| {
                closing.send(()).unwrap();
                resource.close();
                done.send(()).unwrap();
            });
            closing_started
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
            assert_eq!(
                completed.recv_timeout(Duration::from_millis(50)),
                Err(mpsc::RecvTimeoutError::Timeout)
            );
            release.send(()).unwrap();
            completed.recv_timeout(Duration::from_secs(5)).unwrap();
        });
        assert!(resource.native.closed());
        assert_eq!(client.with_available(|native| Ok(*native)).unwrap(), 37);
    }

    #[test]
    #[ignore = "must run alone in a fresh process: finalizes CPython"]
    fn shutdown_rejects_new_registrations_and_is_idempotent() {
        // SAFETY: this test runs alone in its own process and returns no Python values.
        unsafe {
            pyo3::with_embedded_python_interpreter(|py| {
                initialize(py).unwrap();
                let resource = Arc::new(Managed::new(
                    Arc::new(Client::new(37)),
                    None,
                    |native: i32| native,
                ));
                register(resource.clone()).unwrap();
                resource.connect(Ok).unwrap();
                shutdown(py).unwrap();
                assert!(resource.native.closed());
                assert!(register(resource).is_err());
                shutdown(py).unwrap();
            });
        }
    }
}
