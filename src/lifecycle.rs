//! Bounded native cleanup workers and registrations for the main interpreter.

use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::thread::{self, JoinHandle};

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

use crate::callback::Callback;
use crate::state::{Client, Connection};

pub trait Resource: Send + Sync {
    fn retire(&self);
    fn close(&self);
    fn reserve(&self) -> PyResult<Arc<AtomicU8>>;
    fn queue(&self) -> bool;
    fn finish(&self);
}

/// Separates native client ownership from the Python client's lifetime.
/// The callable's one Python reference is visited by its Python connection.
pub struct Managed<C, N> {
    pub client: Arc<Client<C>>,
    pub native: Connection<N>,
    pub callback: Option<Arc<Callback>>,
    opening: Mutex<bool>,
    cleanup: Arc<AtomicU8>,
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
            cleanup: Arc::new(AtomicU8::new(UNREGISTERED)),
            teardown,
        }
    }

    pub fn connect(
        &self,
        operation: impl FnOnce(C) -> Result<N, midir::ConnectError<C>>,
    ) -> PyResult<()> {
        crate::callback::check_blocking_thread()?;
        // Shutdown can see registration before native connect returns.
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
        if !*closed {
            *closed = true;
            if let Some(callback) = &self.callback {
                callback.drain();
            }
            self.native.close(&self.client, self.teardown);
            // A queued job keeps its reservation until a worker consumes it,
            // even if an explicit close has already completed its native work.
            let _ = self
                .cleanup
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                    (state != QUEUED).then_some(FINISHED)
                });
        }
    }

    fn reserve(&self) -> PyResult<Arc<AtomicU8>> {
        self.cleanup
            .compare_exchange(
                UNREGISTERED,
                REGISTERED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|_| {
                PyRuntimeError::new_err("MIDI connection is already registered or closed")
            })?;
        Ok(Arc::clone(&self.cleanup))
    }

    fn queue(&self) -> bool {
        self.cleanup
            .compare_exchange(REGISTERED, QUEUED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    fn finish(&self) {
        self.cleanup.store(FINISHED, Ordering::Release);
    }
}

pub fn initialize(py: Python<'_>) -> PyResult<()> {
    // SAFETY: attached to a live interpreter; process state requires the main interpreter.
    if unsafe { pyo3::ffi::PyInterpreterState_GetID(pyo3::ffi::PyInterpreterState_Get()) } != 0 {
        return Err(PyRuntimeError::new_err(
            "midirp does not support subinterpreters",
        ));
    }
    if RUNTIME.get().is_none() {
        let runtime = Runtime::start().map_err(|error| {
            PyRuntimeError::new_err(format!("start MIDI cleanup workers: {error}"))
        })?;
        let registration = PyModule::import(py, "atexit")
            .and_then(|atexit| atexit.call_method1("register", (wrap_pyfunction!(shutdown, py)?,)));
        if let Err(error) = registration {
            runtime.prepare_shutdown();
            py.detach(|| runtime.join());
            return Err(error);
        }
        // Serialized by the GIL; free-threaded builds are outside the contract.
        RUNTIME.set(runtime).unwrap_or_else(|_| unreachable!());
    }
    Ok(())
}

pub fn register(resource: Arc<dyn Resource>) -> PyResult<()> {
    runtime().register(resource)
}

/// Retires Python delivery and queues native work without waiting for teardown.
pub fn defer(resource: Arc<dyn Resource>) {
    resource.retire();
    runtime().enqueue(resource);
}

#[pyfunction]
fn shutdown(py: Python<'_>) -> PyResult<()> {
    crate::callback::check_blocking_thread()?;
    if runtime().prepare_shutdown() {
        py.detach(|| runtime().join());
    }
    Ok(())
}

fn runtime() -> &'static Runtime {
    RUNTIME.get().expect("MIDI lifecycle not initialized")
}

struct Runtime {
    cleanup: Arc<Cleanup>,
    workers: Mutex<Vec<JoinHandle<()>>>,
}

impl Runtime {
    fn start() -> io::Result<Self> {
        let cleanup = Arc::new(Cleanup {
            registry: Mutex::new(Registry {
                closing: false,
                stopping: false,
                resources: Vec::with_capacity(CONNECTION_CAPACITY),
                pending: VecDeque::with_capacity(CONNECTION_CAPACITY),
            }),
            ready: Condvar::new(),
        });
        let runtime = Self {
            cleanup,
            workers: Mutex::new(Vec::with_capacity(CLEANUP_WORKERS)),
        };
        for index in 0..CLEANUP_WORKERS {
            let cleanup = Arc::clone(&runtime.cleanup);
            let worker = thread::Builder::new()
                .name(format!("midirp cleanup {index}"))
                .spawn(move || {
                    loop {
                        let resource = {
                            let mut registry =
                                cleanup.registry.lock().expect("registry lock poisoned");
                            loop {
                                if let Some(resource) = registry.pending.pop_front() {
                                    break Some(resource);
                                }
                                if registry.stopping {
                                    break None;
                                }
                                registry = cleanup
                                    .ready
                                    .wait(registry)
                                    .expect("registry lock poisoned");
                            }
                        };
                        let Some(resource) = resource else {
                            return;
                        };
                        resource.close();
                        resource.finish();
                        // Native client destruction also runs outside the registry lock.
                    }
                });
            match worker {
                Ok(worker) => runtime.workers.lock().unwrap().push(worker),
                Err(error) => {
                    runtime.prepare_shutdown();
                    runtime.join();
                    return Err(error);
                }
            }
        }
        Ok(runtime)
    }

    fn register(&self, resource: Arc<dyn Resource>) -> PyResult<()> {
        let mut registry = self
            .cleanup
            .registry
            .lock()
            .expect("registry lock poisoned");
        if registry.closing {
            return Err(PyRuntimeError::new_err(
                "MIDI interpreter shutdown has begun",
            ));
        }
        // Inspect separate registration flags without temporarily owning native resources.
        registry.resources.retain(|(resource, state)| {
            resource.strong_count() != 0 && state.load(Ordering::Acquire) != FINISHED
        });
        if registry.resources.len() == CONNECTION_CAPACITY {
            return Err(PyRuntimeError::new_err(format!("MIDI connection capacity exhausted ({CONNECTION_CAPACITY}); close existing connections and allow cleanup to finish")));
        }
        let state = resource.reserve()?;
        registry.resources.push((Arc::downgrade(&resource), state));
        Ok(())
    }

    fn enqueue(&self, resource: Arc<dyn Resource>) {
        let mut registry = self
            .cleanup
            .registry
            .lock()
            .expect("registry lock poisoned");
        if !registry.closing && resource.queue() {
            registry.pending.push_back(resource);
            self.cleanup.ready.notify_one();
        }
        // During shutdown the snapshot owns every registered live resource.
    }

    fn prepare_shutdown(&self) -> bool {
        let resources = {
            let mut registry = self
                .cleanup
                .registry
                .lock()
                .expect("registry lock poisoned");
            if registry.closing {
                return false;
            }
            registry.closing = true;
            registry
                .resources
                .drain(..)
                .filter(|(_, state)| state.load(Ordering::Acquire) != FINISHED)
                .filter_map(|(resource, _)| resource.upgrade())
                .collect::<Vec<_>>()
        };
        // Disable every Python gate before asking any worker to drain a live callback.
        for resource in &resources {
            resource.retire();
        }
        let scheduled = resources
            .into_iter()
            .filter(|resource| resource.queue())
            .collect::<Vec<_>>();
        {
            let mut registry = self
                .cleanup
                .registry
                .lock()
                .expect("registry lock poisoned");
            registry.pending.extend(scheduled);
            registry.stopping = true;
            self.cleanup.ready.notify_all();
        }
        true
    }

    fn join(&self) {
        let workers = std::mem::take(&mut *self.workers.lock().expect("worker lock poisoned"));
        for worker in workers {
            worker.join().expect("cleanup worker panicked");
        }
    }
}

struct Cleanup {
    registry: Mutex<Registry>,
    ready: Condvar,
}

struct Registry {
    closing: bool,
    stopping: bool,
    resources: Vec<(Weak<dyn Resource>, Arc<AtomicU8>)>,
    pending: VecDeque<Arc<dyn Resource>>,
}

const CLEANUP_WORKERS: usize = 4;
const CONNECTION_CAPACITY: usize = 32;
const UNREGISTERED: u8 = 0;
const REGISTERED: u8 = 1;
const QUEUED: u8 = 2;
const FINISHED: u8 = 3;
static RUNTIME: OnceLock<Runtime> = OnceLock::new();

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn cleanup_continues_while_another_native_teardown_is_blocked() {
        let runtime = Runtime::start().unwrap();
        let (started, starting) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let blocked = Arc::new(Managed::new(
            Arc::new(Client::new(37)),
            None,
            |native: (i32, mpsc::Sender<()>, mpsc::Receiver<()>)| {
                native.1.send(()).unwrap();
                native.2.recv_timeout(Duration::from_secs(5)).unwrap();
                native.0
            },
        ));
        runtime.register(blocked.clone()).unwrap();
        blocked
            .connect(|value| Ok((value, started, released)))
            .unwrap();
        runtime.enqueue(blocked.clone());
        starting.recv_timeout(Duration::from_secs(5)).unwrap();
        let (finished, finishing) = mpsc::channel();
        let other = Arc::new(Managed::new(
            Arc::new(Client::new(19)),
            None,
            |native: (i32, mpsc::Sender<()>)| {
                native.1.send(()).unwrap();
                native.0
            },
        ));
        runtime.register(other.clone()).unwrap();
        other.connect(|value| Ok((value, finished))).unwrap();
        runtime.enqueue(other.clone());
        runtime.enqueue(other.clone());
        let result = finishing.recv_timeout(Duration::from_secs(1));
        release.send(()).unwrap();
        runtime.prepare_shutdown();
        runtime.join();
        result.unwrap();
        assert_eq!(finishing.try_recv(), Err(mpsc::TryRecvError::Disconnected));
        assert!(blocked.native.closed());
        assert!(other.native.closed());
    }

    #[test]
    fn capacity_rejects_unopened_resources_and_explicit_close_releases_a_slot() {
        let runtime = Runtime::start().unwrap();
        let mut resources = Vec::new();
        for _ in 0..CONNECTION_CAPACITY {
            let resource = Arc::new(Managed::new(
                Arc::new(Client::new(37)),
                None,
                |native: i32| native,
            ));
            runtime.register(resource.clone()).unwrap();
            resource.connect(Ok).unwrap();
            resources.push(resource);
        }
        let next = Arc::new(Managed::new(
            Arc::new(Client::new(19)),
            None,
            |native: i32| native,
        ));
        let error = runtime.register(next.clone()).unwrap_err();
        Python::initialize();
        Python::attach(|py| assert!(error.is_instance_of::<PyRuntimeError>(py)));
        runtime.enqueue(next.clone());
        assert_eq!(next.client.with_available(|value| Ok(*value)).unwrap(), 19);
        resources[0].close();
        runtime.register(next.clone()).unwrap();
        next.connect(Ok).unwrap();
        runtime.prepare_shutdown();
        runtime.join();
        assert!(next.native.closed());
        assert!(resources.iter().all(|resource| resource.native.closed()));
    }

    #[test]
    fn completed_cleanup_releases_capacity_while_python_resources_remain_alive() {
        let runtime = Runtime::start().unwrap();
        let resource = Arc::new(Managed::new(
            Arc::new(Client::new(37)),
            None,
            |native: i32| native,
        ));
        runtime.register(resource.clone()).unwrap();
        resource.connect(Ok).unwrap();
        runtime.enqueue(resource.clone());
        let deadline = Instant::now() + Duration::from_secs(5);
        while resource.cleanup.load(Ordering::Acquire) != FINISHED {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        runtime.enqueue(resource.clone());
        let mut resources = Vec::new();
        for _ in 0..CONNECTION_CAPACITY {
            let next = Arc::new(Managed::new(
                Arc::new(Client::new(19)),
                None,
                |native: i32| native,
            ));
            runtime.register(next.clone()).unwrap();
            resources.push(next);
        }
        runtime.prepare_shutdown();
        runtime.join();
        assert!(resource.native.closed());
    }

    #[test]
    fn queued_cleanup_keeps_capacity_reserved_until_a_worker_consumes_it() {
        let runtime = Runtime::start().unwrap();
        let (started, starting) = mpsc::channel();
        let mut releases = Vec::new();
        let mut resources = Vec::new();
        for _ in 0..CLEANUP_WORKERS {
            let (release, released) = mpsc::channel();
            let resource = Arc::new(Managed::new(
                Arc::new(Client::new(())),
                None,
                |native: (mpsc::Sender<()>, mpsc::Receiver<()>)| {
                    native.0.send(()).unwrap();
                    native.1.recv_timeout(Duration::from_secs(5)).unwrap();
                },
            ));
            runtime.register(resource.clone()).unwrap();
            resource
                .connect(|()| Ok((started.clone(), released)))
                .unwrap();
            runtime.enqueue(resource.clone());
            releases.push(release);
            resources.push(resource);
        }
        for _ in 0..CLEANUP_WORKERS {
            starting.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        let mut pending = Vec::new();
        for _ in CLEANUP_WORKERS..CONNECTION_CAPACITY {
            let resource = Arc::new(Managed::new(Arc::new(Client::new(())), None, |()| ()));
            runtime.register(resource.clone()).unwrap();
            resource.connect(Ok).unwrap();
            runtime.enqueue(resource.clone());
            resource.close();
            runtime.enqueue(resource.clone());
            pending.push(resource);
        }
        let next = Arc::new(Managed::new(Arc::new(Client::new(())), None, |()| ()));
        let result = runtime.register(next.clone());
        for release in releases {
            release.send(()).unwrap();
        }
        runtime.prepare_shutdown();
        runtime.join();
        assert!(result.is_err());
        assert!(pending.iter().all(|resource| resource.native.closed()));
    }

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
