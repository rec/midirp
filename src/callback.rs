//! Direct native-thread delivery with a GC-visible callable and a drain gate.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, LockResult, Mutex, MutexGuard};

use pyo3::exceptions::{PyRuntimeError, PyTypeError};
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use pyo3::{PyTraverseError, PyVisit};

use crate::errors::failed_error;

/// Reuses the native callback contract on a parent-process dispatch thread.
#[pyclass(frozen, module = "midirp._native")]
pub struct CallbackBridge {
    callback: Callback,
}

#[pymethods]
impl CallbackBridge {
    #[new]
    fn new(py: Python<'_>, callable: Py<PyAny>) -> PyResult<Self> {
        Ok(Self {
            callback: Callback::new(py, callable)?,
        })
    }

    fn deliver(&self, timestamp: u64, message: &Bound<'_, PyBytes>) {
        self.callback.deliver(timestamp, message.as_bytes());
    }

    fn retire(&self) {
        self.callback.retire();
    }

    fn drain(&self, py: Python<'_>) -> PyResult<()> {
        py.detach(|| self.callback.drain())
    }

    fn __traverse__(&self, visit: PyVisit<'_>) -> Result<(), PyTraverseError> {
        self.callback.traverse(visit)
    }

    fn __clear__(&self) {
        self.callback.retire();
    }
}

#[pyfunction]
pub fn check_thread() -> PyResult<()> {
    check_blocking_thread()
}

/// Owns the callable and drains all admitted deliveries before native teardown.
pub struct Callback {
    state: Mutex<DispatchState>,
    drained: Condvar,
    failed: AtomicBool,
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
            failed: AtomicBool::new(false),
        })
    }

    pub fn deliver(&self, timestamp: u64, message: &[u8]) {
        let _delivery = {
            let mut state = self.recover(self.state.lock());
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
                let state = self.recover(self.state.lock());
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
        let state = self.recover(self.state.lock());
        visit.call(&state.callable)
    }

    pub fn retire(&self) {
        let retired = {
            let mut state = self.recover(self.state.lock());
            state.accepting = false;
            state.callable.take()
        };
        // Decref can invoke arbitrary Python destructors. Never do it under a lock.
        drop(retired);
    }

    pub fn drain(&self) -> PyResult<()> {
        let mut state = self.recover(self.state.lock());
        state.accepting = false;
        while state.active != 0 {
            state = self.recover(self.drained.wait(state));
        }
        if self.failed.load(Ordering::Acquire) {
            Err(failed_error())
        } else {
            Ok(())
        }
    }

    fn recover<'a>(
        &self,
        result: LockResult<MutexGuard<'a, DispatchState>>,
    ) -> MutexGuard<'a, DispatchState> {
        match result {
            Ok(state) => state,
            Err(poisoned) => {
                // No user/native calls mutate this gate under its lock. Preserve
                // admitted-delivery accounting, but permanently refuse new work.
                let mut state = poisoned.into_inner();
                state.accepting = false;
                self.failed.store(true, Ordering::Release);
                state
            }
        }
    }
}

pub fn check_blocking_thread() -> PyResult<()> {
    if IN_CALLBACK.get() {
        return Err(PyRuntimeError::new_err(
            "Cannot perform blocking MIDI operations from a callback; use the controlling thread",
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
        let mut state = self.callback.recover(self.callback.state.lock());
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
    use std::time::{Duration, Instant};

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
            check_blocking_thread()?;
            self.resource.retire();
            py.detach(|| self.resource.close())
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
            Arc::new(Client::new((), false)),
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
    fn callback_can_receive_before_native_connect_returns() {
        let _test = TEST_LOCK.lock().unwrap();
        Python::initialize();
        Python::attach(|py| {
            let recorder = fixtures(py).getattr("Recorder").unwrap().call0().unwrap();
            let callback = Arc::new(Callback::new(py, recorder.clone().unbind()).unwrap());
            let resource = Managed::new(
                Arc::new(Client::new((), false)),
                Some(Arc::clone(&callback)),
                |()| (),
            );
            py.detach(|| {
                resource.connect(|()| {
                    let (done, delivered) = mpsc::channel();
                    let worker = thread::spawn(move || {
                        callback.deliver(17, &[0x90, 60, 127]);
                        done.send(()).unwrap();
                    });
                    // Delivery completes while the native open still owns control.
                    delivered.recv_timeout(DEADLINE).unwrap();
                    worker.join().unwrap();
                    Ok(())
                })
            })
            .unwrap();
            resource.retire();
            py.detach(|| resource.close()).unwrap();
            let messages: Vec<(u64, Vec<u8>)> =
                recorder.getattr("messages").unwrap().extract().unwrap();
            assert_eq!(messages, vec![(17, vec![0x90, 60, 127])]);
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
                // The native stop signal precedes publication of completed cleanup.
                let deadline = Instant::now() + DEADLINE;
                while !resource.native.closed() {
                    assert!(
                        Instant::now() < deadline,
                        "automatic cleanup did not finish"
                    );
                    thread::sleep(Duration::from_millis(1));
                }
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
                let deadline = Instant::now() + DEADLINE;
                while !resource.native.closed() {
                    assert!(
                        Instant::now() < deadline,
                        "automatic cleanup did not finish"
                    );
                    thread::sleep(Duration::from_millis(1));
                }
            });
            assert!(reference.call0().unwrap().is_none());
            assert_eq!(client.with_available(|()| Ok(37)).unwrap(), 37);
        });
    }

    #[test]
    fn callback_and_error_hook_cannot_send_but_the_controlling_thread_can() {
        let _test = TEST_LOCK.lock().unwrap();
        Python::initialize();
        Python::attach(|py| {
            let output = crate::output::tests::OutputDriver::new(py);
            let context = fixtures(py).call_method0("capture_errors").unwrap();
            let errors = context.call_method0("__enter__").unwrap();
            errors
                .setattr("outputs", vec![output.clone_ref(py)])
                .unwrap();
            let recorder = fixtures(py)
                .getattr("SendingRecorder")
                .unwrap()
                .call1((output.bind(py),))
                .unwrap();
            let (input, commands) = driver(py, &recorder, None);
            let delivered = emit(&commands, 37, vec![0xf0, 0, 1, 0xf7]);
            py.detach(move || delivered.recv_timeout(DEADLINE).unwrap());
            input.get().close(py).unwrap();
            assert!(output.get().messages.lock().unwrap().is_empty());
            let reports: Vec<String> = errors.getattr("errors").unwrap().extract().unwrap();
            let hook_errors: Vec<String> =
                errors.getattr("send_errors").unwrap().extract().unwrap();
            assert_eq!(reports.len(), 1);
            assert_eq!(hook_errors, reports);
            assert!(reports[0].contains("controlling thread"));
            context
                .call_method1("__exit__", (py.None(), py.None(), py.None()))
                .unwrap();
            output
                .bind(py)
                .call_method1("send", (PyBytes::new(py, &[0xf8]),))
                .unwrap();
            assert_eq!(*output.get().messages.lock().unwrap(), vec![vec![0xf8]]);
            output.bind(py).call_method0("close").unwrap();
        });
    }

    #[test]
    fn callbacks_reject_native_client_creation_before_initializing_a_backend() {
        let _test = TEST_LOCK.lock().unwrap();
        Python::initialize();
        Python::attach(|py| {
            let recorder = fixtures(py)
                .getattr("ConstructorRecorder")
                .unwrap()
                .call1((vec![
                    py.get_type::<crate::input::MidiInput>().into_any(),
                    py.get_type::<crate::output::MidiOutput>().into_any(),
                ],))
                .unwrap();
            let (input, commands) = driver(py, &recorder, None);
            let delivered = emit(&commands, 1, vec![0xf8]);
            py.detach(move || delivered.recv_timeout(DEADLINE).unwrap());
            input.get().close(py).unwrap();
            let errors: Vec<String> = recorder.getattr("errors").unwrap().extract().unwrap();
            assert_eq!(errors.len(), 2);
            assert!(errors
                .iter()
                .all(|error| error.contains("controlling thread")));
        });
    }

    #[test]
    fn callback_rejects_discovery_configuration_and_opening_before_native_work() {
        Python::initialize();
        Python::attach(|py| {
            let client = Client::new(37, false);
            let connection = crate::state::Connection::<i32>::new();
            let resource = Managed::new(Arc::new(Client::new(19, false)), None, |value: i32| value);
            let _callback = CallbackThread::enter();
            let discovery: PyResult<()> = client.with_available(|_| panic!("must not enumerate"));
            let configure: PyResult<()> = client.with_available(|_| panic!("must not configure"));
            let connect = client.connect(&connection, |_| panic!("must not open"));
            let virtual_open = resource.connect(|_| panic!("must not open"));
            for error in [discovery, configure, connect, virtual_open] {
                let error = error.unwrap_err();
                assert!(error.is_instance_of::<PyRuntimeError>(py));
                assert!(error.value(py).to_string().contains("controlling thread"));
            }
        });
    }

    #[test]
    fn poisoned_callback_gate_drains_admitted_delivery_and_retires_python_references() {
        let _test = TEST_LOCK.lock().unwrap();
        Python::initialize();
        Python::attach(|py| {
            let recorder = fixtures(py)
                .getattr("BlockingRecorder")
                .unwrap()
                .call0()
                .unwrap();
            let reference = PyModule::import(py, "weakref")
                .unwrap()
                .call_method1("ref", (&recorder,))
                .unwrap();
            let callback = Arc::new(Callback::new(py, recorder.clone().unbind()).unwrap());
            let resource = Arc::new(Managed::new(
                Arc::new(Client::new((), false)),
                Some(Arc::clone(&callback)),
                |()| (),
            ));
            resource.connect(Ok).unwrap();
            let active_callback = Arc::clone(&callback);
            let active = thread::spawn(move || active_callback.deliver(1, &[0xf8]));
            assert!(recorder
                .getattr("started")
                .unwrap()
                .call_method1("wait", (5,))
                .unwrap()
                .extract::<bool>()
                .unwrap());
            assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _state = callback.state.lock().unwrap();
                panic!("callback gate fault");
            }))
            .is_err());
            let delivering = Arc::clone(&callback);
            py.detach(move || {
                thread::spawn(move || delivering.deliver(2, &[0xfe]))
                    .join()
                    .unwrap()
            });
            let messages: Vec<(u64, Vec<u8>)> =
                recorder.getattr("messages").unwrap().extract().unwrap();
            assert_eq!(messages, vec![(1, vec![0xf8])]);
            resource.retire();
            let (done, completed) = mpsc::channel();
            let closing_resource = Arc::clone(&resource);
            let closing = thread::spawn(move || done.send(closing_resource.close()).unwrap());
            let (completed, waiting) = py.detach(move || {
                let waiting = completed.recv_timeout(Duration::from_millis(50));
                (completed, waiting)
            });
            recorder
                .getattr("release")
                .unwrap()
                .call_method0("set")
                .unwrap();
            let result = py.detach(move || {
                let result = completed.recv_timeout(DEADLINE).unwrap();
                closing.join().unwrap();
                active.join().unwrap();
                result
            });
            assert!(matches!(waiting, Err(mpsc::RecvTimeoutError::Timeout)));
            drop(recorder);
            assert!(reference.call0().unwrap().is_none());
            let error = result.unwrap_err();
            assert!(error.is_instance_of::<PyRuntimeError>(py));
            assert!(!resource.native.closed());
            assert!(resource.client.with_available(|_| Ok(())).is_err());
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
