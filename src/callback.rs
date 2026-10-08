//! Direct native-thread delivery with a GC-visible callable and a drain gate.

use std::cell::Cell;
use std::sync::{Condvar, Mutex};

use pyo3::exceptions::{PyRuntimeError, PyTypeError};
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use pyo3::{PyTraverseError, PyVisit};

/// Owns the callable and drains all admitted deliveries before native teardown.
pub struct Callback {
    state: Mutex<DispatchState>,
    drained: Condvar,
}

impl Callback {
    pub fn new(py: Python<'_>, callable: Py<PyAny>) -> PyResult<Self> {
        if !callable.bind(py).is_callable() {
            return Err(PyTypeError::new_err("MIDI callback must be callable"));
        }
        Ok(Self {
            state: Mutex::new(DispatchState {
                callable: Some(callable),
                accepting: true,
                active: 0,
            }),
            drained: Condvar::new(),
        })
    }

    pub fn deliver(&self, timestamp: u64, message: &[u8]) {
        let _delivery = {
            let mut state = self.state.lock().expect("callback state lock poisoned");
            if !state.accepting {
                return;
            }
            state.active += 1;
            Delivery { callback: self }
        };

        // The shutdown registry disables and drains this gate before finalization.
        Python::try_attach(|py| {
            let _thread = CallbackThread::enter();
            let callable = {
                let state = self.state.lock().expect("callback state lock poisoned");
                // Close may have started while this thread waited for the GIL.
                if !state.accepting {
                    return;
                }
                state
                    .callable
                    .as_ref()
                    .expect("open callback missing")
                    .clone_ref(py)
            };
            let callable = callable.bind(py);
            let bytes = PyBytes::new(py, message);
            if let Err(error) = callable.call1((timestamp, bytes)) {
                error.write_unraisable(py, Some(callable));
            }
        });
    }

    pub fn traverse(&self, visit: PyVisit<'_>) -> Result<(), PyTraverseError> {
        let state = self.state.lock().expect("callback state lock poisoned");
        visit.call(&state.callable)
    }

    pub fn retire(&self) {
        let retired = {
            let mut state = self.state.lock().expect("callback state lock poisoned");
            state.accepting = false;
            state.callable.take()
        };
        // Decref can invoke arbitrary Python destructors. Never do it under a lock.
        drop(retired);
    }

    pub fn drain(&self) {
        let mut state = self.state.lock().expect("callback state lock poisoned");
        state.accepting = false;
        while state.active != 0 {
            state = self
                .drained
                .wait(state)
                .expect("callback state lock poisoned");
        }
    }
}

pub fn check_close_thread() -> PyResult<()> {
    if IN_CALLBACK.get() {
        return Err(PyRuntimeError::new_err(
            "Cannot close a MIDI connection from a callback; close it from the controlling thread",
        ));
    }
    Ok(())
}

struct DispatchState {
    callable: Option<Py<PyAny>>,
    accepting: bool,
    active: usize,
}

struct Delivery<'a> {
    callback: &'a Callback,
}

impl Drop for Delivery<'_> {
    fn drop(&mut self) {
        let mut state = self
            .callback
            .state
            .lock()
            .expect("callback state lock poisoned");
        state.active -= 1;
        if state.active == 0 {
            self.callback.drained.notify_all();
        }
    }
}

struct CallbackThread {
    previous: bool,
}

impl CallbackThread {
    fn enter() -> Self {
        Self {
            previous: IN_CALLBACK.replace(true),
        }
    }
}

impl Drop for CallbackThread {
    fn drop(&mut self) {
        IN_CALLBACK.set(self.previous);
    }
}

thread_local! {
    static IN_CALLBACK: Cell<bool> = const { Cell::new(false) };
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::{mpsc, Arc};
    use std::thread;
    use std::time::Duration;

    use pyo3::types::PyList;

    use super::*;
    use crate::lifecycle::{self, Managed, Resource};
    use crate::state::Client;

    static TEST_LOCK: Mutex<()> = Mutex::new(());
    const DEADLINE: Duration = Duration::from_secs(5);

    // Private driver: its worker calls the actual bridge on a native thread.
    // It never initializes or simulates an OS MIDI backend.
    #[pyclass(frozen, weakref)]
    struct InputDriver {
        callback: Arc<Callback>,
        resource: Arc<Managed<(), NativeThread>>,
    }

    #[pymethods]
    impl InputDriver {
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

        fn __traverse__(&self, visit: PyVisit<'_>) -> Result<(), PyTraverseError> {
            self.callback.traverse(visit)
        }

        fn __clear__(&self) {
            lifecycle::defer(self.resource.clone());
        }
    }

    impl Drop for InputDriver {
        fn drop(&mut self) {
            lifecycle::defer(self.resource.clone());
        }
    }

    struct NativeThread {
        commands: mpsc::Sender<Command>,
        worker: thread::JoinHandle<()>,
        stopped: Option<mpsc::Sender<()>>,
    }

    enum Command {
        Emit(u64, Vec<u8>, mpsc::Sender<()>),
        Stop,
    }

    fn fixtures(py: Python<'_>) -> Bound<'_, PyModule> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("test/native");
        PyModule::import(py, "sys")
            .unwrap()
            .getattr("path")
            .unwrap()
            .call_method1("insert", (0, path.to_str().unwrap()))
            .unwrap();
        PyModule::import(py, "midirp_callbacks").unwrap()
    }

    fn driver(
        py: Python<'_>,
        callable: &Bound<'_, PyAny>,
        stopped: Option<mpsc::Sender<()>>,
    ) -> (Py<InputDriver>, mpsc::Sender<Command>) {
        lifecycle::initialize(py).unwrap();
        let callback = Arc::new(Callback::new(py, callable.clone().unbind()).unwrap());
        let resource = Arc::new(Managed::new(
            Arc::new(Client::new(())),
            Some(Arc::clone(&callback)),
            |native: NativeThread| {
                native.commands.send(Command::Stop).unwrap();
                native.worker.join().unwrap();
                if let Some(stopped) = native.stopped {
                    stopped.send(()).unwrap();
                }
            },
        ));
        let driver = Py::new(
            py,
            InputDriver {
                callback: Arc::clone(&callback),
                resource: Arc::clone(&resource),
            },
        )
        .unwrap();
        let (commands, receiver) = mpsc::channel();
        lifecycle::register(resource.clone()).unwrap();
        resource
            .connect(|()| {
                let worker = thread::spawn(move || {
                    for command in receiver {
                        match command {
                            Command::Emit(timestamp, bytes, done) => {
                                callback.deliver(timestamp, &bytes);
                                let _ = done.send(());
                            }
                            Command::Stop => break,
                        }
                    }
                });
                Ok(NativeThread {
                    commands: commands.clone(),
                    worker,
                    stopped,
                })
            })
            .unwrap();
        (driver, commands)
    }

    fn emit(
        commands: &mpsc::Sender<Command>,
        timestamp: u64,
        bytes: Vec<u8>,
    ) -> mpsc::Receiver<()> {
        let (done, receiver) = mpsc::channel();
        commands
            .send(Command::Emit(timestamp, bytes, done))
            .unwrap();
        receiver
    }

    #[test]
    fn non_callable_is_rejected_before_a_worker_starts() {
        let _test = TEST_LOCK.lock().unwrap();
        Python::initialize();
        Python::attach(|py| {
            let error = Callback::new(py, PyList::empty(py).into_any().unbind())
                .err()
                .unwrap();
            assert!(error.is_instance_of::<PyTypeError>(py));
        });
    }

    #[test]
    fn explicit_close_releases_the_python_callable() {
        let _test = TEST_LOCK.lock().unwrap();
        Python::initialize();
        Python::attach(|py| {
            let recorder = fixtures(py).getattr("Recorder").unwrap().call0().unwrap();
            let reference = PyModule::import(py, "weakref")
                .unwrap()
                .call_method1("ref", (&recorder,))
                .unwrap();
            let (driver, _) = driver(py, &recorder, None);
            drop(recorder);
            assert!(!reference.call0().unwrap().is_none());
            driver.get().close(py).unwrap();
            assert!(reference.call0().unwrap().is_none());
        });
    }

    #[test]
    fn native_delivery_preserves_owned_bytes_timestamps_and_order() {
        let _test = TEST_LOCK.lock().unwrap();
        Python::initialize();
        Python::attach(|py| {
            let recorder = fixtures(py).getattr("Recorder").unwrap().call0().unwrap();
            let main_thread: u64 = PyModule::import(py, "threading")
                .unwrap()
                .call_method0("get_ident")
                .unwrap()
                .extract()
                .unwrap();
            let (driver, commands) = driver(py, &recorder, None);
            let first = emit(&commands, 0, vec![0x90, 60, 127]);
            let second = emit(&commands, u64::MAX, vec![0xf0, 0, 1, 0xf7]);
            py.detach(move || {
                first.recv_timeout(DEADLINE).unwrap();
                second.recv_timeout(DEADLINE).unwrap();
            });
            driver.get().close(py).unwrap();
            driver.get().close(py).unwrap();
            assert!(driver.get().closed(py));
            // Late native delivery cannot call a retired Python callable.
            let callback = Arc::clone(&driver.get().callback);
            py.detach(|| {
                thread::spawn(move || callback.deliver(17, &[0xf8]))
                    .join()
                    .unwrap()
            });
            let messages: Vec<(u64, Vec<u8>)> =
                recorder.getattr("messages").unwrap().extract().unwrap();
            assert_eq!(
                messages,
                vec![(0, vec![0x90, 60, 127]), (u64::MAX, vec![0xf0, 0, 1, 0xf7])]
            );
            let threads: Vec<u64> = recorder.getattr("threads").unwrap().extract().unwrap();
            assert_eq!(threads.len(), 2);
            assert_ne!(threads[0], main_thread);
            assert_eq!(threads[0], threads[1]);
            recorder.call_method0("verify_bytes").unwrap();
        });
    }

    #[test]
    fn callback_errors_are_reported_and_later_messages_still_arrive() {
        let _test = TEST_LOCK.lock().unwrap();
        Python::initialize();
        Python::attach(|py| {
            let module = fixtures(py);
            let context = module.call_method0("capture_errors").unwrap();
            let errors = context.call_method0("__enter__").unwrap();
            let recorder = module.getattr("RaisingRecorder").unwrap().call0().unwrap();
            let (driver, commands) = driver(py, &recorder, None);
            errors
                .setattr("targets", vec![driver.clone_ref(py)])
                .unwrap();
            let first = emit(&commands, 1, vec![0xf8]);
            let second = emit(&commands, 2, vec![0xfe]);
            py.detach(move || {
                first.recv_timeout(DEADLINE).unwrap();
                second.recv_timeout(DEADLINE).unwrap();
            });
            driver.get().close(py).unwrap();
            context
                .call_method1("__exit__", (py.None(), py.None(), py.None()))
                .unwrap();
            let messages: Vec<(u64, Vec<u8>)> =
                recorder.getattr("messages").unwrap().extract().unwrap();
            assert_eq!(messages.len(), 2);
            let reports: Vec<String> = errors.getattr("errors").unwrap().extract().unwrap();
            assert_eq!(reports, vec!["callback failed"]);
            let close_errors: Vec<String> =
                errors.getattr("close_errors").unwrap().extract().unwrap();
            assert_eq!(close_errors.len(), 1);
            assert!(close_errors[0].contains("controlling thread"));
            assert!(errors
                .getattr("objects")
                .unwrap()
                .get_item(0)
                .unwrap()
                .is(&recorder));
        });
    }

    #[test]
    fn callback_cannot_close_its_own_or_an_unrelated_connection() {
        let _test = TEST_LOCK.lock().unwrap();
        Python::initialize();
        Python::attach(|py| {
            let module = fixtures(py);
            let recorder = module.getattr("CloseRecorder").unwrap().call0().unwrap();
            let unrelated_recorder = module.getattr("Recorder").unwrap().call0().unwrap();
            let (own, commands) = driver(py, &recorder, None);
            let (unrelated, _) = driver(py, &unrelated_recorder, None);
            recorder
                .setattr("targets", vec![own.clone_ref(py), unrelated.clone_ref(py)])
                .unwrap();
            let first = emit(&commands, 1, vec![0xf8]);
            let second = emit(&commands, 2, vec![0xfe]);
            py.detach(move || {
                first.recv_timeout(DEADLINE).unwrap();
                second.recv_timeout(DEADLINE).unwrap();
            });
            assert!(!own.get().closed(py));
            assert!(!unrelated.get().closed(py));
            let errors: Vec<String> = recorder.getattr("errors").unwrap().extract().unwrap();
            assert_eq!(errors.len(), 4);
            assert!(errors.iter().all(|e| e.contains("controlling thread")));
            own.get().close(py).unwrap();
            unrelated.get().close(py).unwrap();
        });
    }

    #[test]
    fn concurrent_close_releases_the_gil_and_waits_for_a_blocked_callback() {
        let _test = TEST_LOCK.lock().unwrap();
        Python::initialize();
        Python::attach(|py| {
            let recorder = fixtures(py)
                .getattr("BlockingRecorder")
                .unwrap()
                .call0()
                .unwrap();
            let (driver, commands) = driver(py, &recorder, None);
            let delivered = emit(&commands, 1, vec![0xf8]);
            assert!(recorder
                .getattr("started")
                .unwrap()
                .call_method1("wait", (5,))
                .unwrap()
                .extract::<bool>()
                .unwrap());
            let (started_tx, started_rx) = mpsc::channel();
            let (done_tx, done_rx) = mpsc::channel();

            thread::scope(|scope| {
                for _ in 0..2 {
                    let driver = driver.clone_ref(py);
                    let started = started_tx.clone();
                    let done = done_tx.clone();
                    scope.spawn(move || {
                        Python::attach(|py| {
                            started.send(()).unwrap();
                            driver.get().close(py).unwrap();
                            done.send(()).unwrap();
                        })
                    });
                }
                let done_rx = py.detach(move || {
                    started_rx.recv_timeout(DEADLINE).unwrap();
                    started_rx.recv_timeout(DEADLINE).unwrap();
                    assert_eq!(
                        done_rx.recv_timeout(Duration::from_millis(50)),
                        Err(mpsc::RecvTimeoutError::Timeout)
                    );
                    done_rx
                });
                recorder
                    .getattr("release")
                    .unwrap()
                    .call_method0("set")
                    .unwrap();
                py.detach(move || {
                    done_rx.recv_timeout(DEADLINE).unwrap();
                    done_rx.recv_timeout(DEADLINE).unwrap();
                    delivered.recv_timeout(DEADLINE).unwrap();
                });
            });
            assert!(driver.get().closed(py));
            assert_eq!(
                driver
                    .get()
                    .resource
                    .client
                    .with_available(|()| Ok(37))
                    .unwrap(),
                37
            );
        });
    }
    #[test]
    fn ordinary_destruction_restores_the_client_without_blocking_python() {
        let _test = TEST_LOCK.lock().unwrap();
        Python::initialize();
        Python::attach(|py| {
            let recorder = fixtures(py).getattr("Recorder").unwrap().call0().unwrap();
            let (stopped, receiver) = mpsc::channel();
            let (driver, _) = driver(py, &recorder, Some(stopped));
            let resource = Arc::clone(&driver.get().resource);
            let client = Arc::clone(&driver.get().resource.client);
            drop(driver);
            py.detach(move || {
                receiver.recv_timeout(DEADLINE).unwrap();
                // The stop signal precedes client restoration. Taking the
                // connection lock waits for automatic cleanup to complete.
                assert!(resource.native.closed());
            });
            assert_eq!(client.with_available(|()| Ok(37)).unwrap(), 37);
        });
    }

    #[test]
    fn cyclic_collection_releases_the_connection_and_callable() {
        let _test = TEST_LOCK.lock().unwrap();
        Python::initialize();
        Python::attach(|py| {
            let recorder = fixtures(py)
                .getattr("CloseRecorder")
                .unwrap()
                .call0()
                .unwrap();
            let (stopped, receiver) = mpsc::channel();
            let (driver, _) = driver(py, &recorder, Some(stopped));
            recorder
                .setattr("targets", vec![driver.clone_ref(py)])
                .unwrap();
            let weakref = PyModule::import(py, "weakref").unwrap();
            let connection_ref = weakref.call_method1("ref", (driver.bind(py),)).unwrap();
            let callable_ref = weakref.call_method1("ref", (&recorder,)).unwrap();
            drop(driver);
            drop(recorder);
            PyModule::import(py, "gc")
                .unwrap()
                .call_method0("collect")
                .unwrap();
            assert!(connection_ref.call0().unwrap().is_none());
            assert!(callable_ref.call0().unwrap().is_none());
            py.detach(move || receiver.recv_timeout(DEADLINE).unwrap());
        });
    }

    #[test]
    fn last_connection_reference_can_disappear_on_the_callback_thread() {
        let _test = TEST_LOCK.lock().unwrap();
        Python::initialize();
        Python::attach(|py| {
            let recorder = fixtures(py)
                .getattr("DroppingRecorder")
                .unwrap()
                .call0()
                .unwrap();
            let (stopped, receiver) = mpsc::channel();
            let (driver, commands) = driver(py, &recorder, Some(stopped));
            let resource = Arc::clone(&driver.get().resource);
            let client = Arc::clone(&driver.get().resource.client);
            recorder
                .setattr("targets", vec![driver.clone_ref(py)])
                .unwrap();
            let reference = PyModule::import(py, "weakref")
                .unwrap()
                .call_method1("ref", (driver.bind(py),))
                .unwrap();
            drop(driver);
            let delivered = emit(&commands, 1, vec![0xf8]);
            py.detach(move || {
                delivered.recv_timeout(DEADLINE).unwrap();
                receiver.recv_timeout(DEADLINE).unwrap();
                assert!(resource.native.closed());
            });
            assert!(reference.call0().unwrap().is_none());
            assert_eq!(client.with_available(|()| Ok(37)).unwrap(), 37);
        });
    }

    #[test]
    fn native_input_callback_can_send_through_a_separate_output_connection() {
        let _test = TEST_LOCK.lock().unwrap();
        Python::initialize();
        Python::attach(|py| {
            let output = crate::output::tests::OutputDriver::new(py);
            let recorder = fixtures(py)
                .getattr("SendingRecorder")
                .unwrap()
                .call1((output.bind(py),))
                .unwrap();
            let (input, commands) = driver(py, &recorder, None);
            let delivered = emit(&commands, 37, vec![0xf0, 0, 1, 0xf7]);
            py.detach(move || delivered.recv_timeout(DEADLINE).unwrap());
            input.get().close(py).unwrap();
            assert_eq!(
                *output.get().messages.lock().unwrap(),
                vec![vec![0xf0, 0, 1, 0xf7]]
            );
            output.bind(py).call_method0("close").unwrap();
        });
    }

    #[test]
    #[ignore = "must run alone in a fresh process: finalizes CPython"]
    fn interpreter_finalization_drains_a_live_input() {
        let (stopped, receiver) = mpsc::channel();
        // SAFETY: pytest runs only this test in a fresh native test process.
        // No Python objects or errors escape this closure or are used afterwards.
        unsafe {
            pyo3::with_embedded_python_interpreter(|py| {
                let module = fixtures(py);
                let blocked = module.getattr("BlockingRecorder").unwrap().call0().unwrap();
                let (active, commands) = driver(py, &blocked, Some(stopped.clone()));
                let _delivered = emit(&commands, 2, vec![0xfe]);
                assert!(blocked
                    .getattr("started")
                    .unwrap()
                    .call_method1("wait", (5,))
                    .unwrap()
                    .extract::<bool>()
                    .unwrap());
                module.setattr("active_connection", active).unwrap();
                // Registered later, this releases the blocked callback immediately
                // before our cleanup handler. It still needs the GIL to return.
                PyModule::import(py, "atexit")
                    .unwrap()
                    .call_method1(
                        "register",
                        (blocked.getattr("release").unwrap().getattr("set").unwrap(),),
                    )
                    .unwrap();
                let recorder = module.getattr("CloseRecorder").unwrap().call0().unwrap();
                let (driver, commands) = driver(py, &recorder, Some(stopped));
                recorder
                    .setattr("targets", vec![driver.clone_ref(py)])
                    .unwrap();
                module.setattr("live_connection", driver).unwrap();
                // A native delivery may be waiting for the GIL when atexit begins.
                let _delivered = emit(&commands, 1, vec![0xf8]);
            });
        }
        receiver.recv_timeout(DEADLINE).unwrap();
        receiver.recv_timeout(DEADLINE).unwrap();
    }
}
