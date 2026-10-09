use std::mem;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, TryLockError};

use pyo3::exceptions::PyRuntimeError;
use pyo3::types::{PyBytes, PyBytesMethods};
use pyo3::{Bound, PyResult, Python};

use crate::callback::check_blocking_thread;
use crate::errors::{failed_error, native_call, report_failure, ConnectError, SendError};

/// Owns a client except while a connection owns its native resource.
pub struct Client<T> {
    state: Mutex<ClientState<T>>,
    failed: AtomicBool,
    discard_failed_opens: bool,
}

/// Serializes native teardown and separately publishes completed closure.
pub struct Connection<T> {
    native: Mutex<Option<T>>,
    closed: AtomicBool,
    failed: AtomicBool,
}

impl<T> Client<T> {
    pub fn new(native: T, discard_failed_opens: bool) -> Self {
        Self {
            state: Mutex::new(ClientState::Available(native)),
            failed: AtomicBool::new(false),
            discard_failed_opens,
        }
    }

    pub fn with_available<R>(&self, operation: impl FnOnce(&mut T) -> PyResult<R>) -> PyResult<R> {
        check_blocking_thread()?;
        let mut state = self.try_state()?;
        match &mut *state {
            ClientState::Available(native) => {
                match native_call("client operation", || operation(native)) {
                    Ok(result) => result,
                    Err(error) => {
                        self.failed.store(true, Ordering::Release);
                        Err(error)
                    }
                }
            }
            _ => Err(PyRuntimeError::new_err(
                "MIDI client is unavailable while connecting, connected, or closing",
            )),
        }
    }

    pub fn connect<R>(
        &self,
        connection: &Connection<R>,
        operation: impl FnOnce(T) -> Result<R, midir::ConnectError<T>>,
    ) -> PyResult<()> {
        check_blocking_thread()?;
        let native = {
            let mut state = self.try_state()?;
            if !matches!(*state, ClientState::Available(_)) {
                return Err(PyRuntimeError::new_err(
                    "MIDI client is unavailable while connecting, connected, or closing",
                ));
            }
            let ClientState::Available(native) = mem::replace(&mut *state, ClientState::Connecting)
            else {
                unreachable!();
            };
            native
        };
        match native_call("connect", || operation(native)) {
            Ok(Ok(native)) => {
                {
                    let mut handle = connection.lock_native(self)?;
                    *handle = Some(native);
                    connection.closed.store(false, Ordering::Release);
                }
                *self.lock_state()? = ClientState::Connected;
                Ok(())
            }
            Ok(Err(error)) => {
                let message = format!("connect: {error}");
                let mut state = self.lock_state()?;
                // ALSA can retain partial allocations on returned errors.
                // The thread-start error also returns a client without a
                // sequencer, including when exercised by device-free tests.
                if self.discard_failed_opens
                    || matches!(
                        error.kind(),
                        midir::ConnectErrorKind::Other("could not start ALSA input handler thread")
                    )
                {
                    self.failed.store(true, Ordering::Release);
                    *state = ClientState::Closing;
                    drop(state);
                    if native_call("failed-open client destruction", || {
                        drop(error.into_inner())
                    })
                    .is_err()
                    {
                        report_failure("native failed-open client destruction panicked");
                    }
                } else {
                    *state = ClientState::Available(error.into_inner());
                }
                Err(ConnectError::new_err(message))
            }
            Err(error) => {
                connection.fail(self);
                Err(error)
            }
        }
    }

    fn try_state(&self) -> PyResult<MutexGuard<'_, ClientState<T>>> {
        self.check_failed()?;
        let state = match self.state.try_lock() {
            Ok(state) => state,
            Err(TryLockError::WouldBlock) => {
                return Err(PyRuntimeError::new_err(
                    "MIDI client is busy with another operation",
                ))
            }
            Err(TryLockError::Poisoned(_)) => {
                self.failed.store(true, Ordering::Release);
                return Err(failed_error());
            }
        };
        self.check_failed()?;
        Ok(state)
    }

    fn lock_state(&self) -> PyResult<MutexGuard<'_, ClientState<T>>> {
        self.state.lock().map_err(|_| {
            self.failed.store(true, Ordering::Release);
            failed_error()
        })
    }

    fn check_failed(&self) -> PyResult<()> {
        if self.failed.load(Ordering::Acquire) {
            Err(failed_error())
        } else {
            Ok(())
        }
    }
}

impl<T> Connection<T> {
    pub fn new() -> Self {
        Self {
            native: Mutex::new(None),
            closed: AtomicBool::new(true),
            failed: AtomicBool::new(false),
        }
    }

    pub fn closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    pub fn send<C: Send>(
        &self,
        client: &Client<C>,
        py: Python<'_>,
        message: &Bound<'_, PyBytes>,
        operation: impl FnOnce(&mut T, &[u8]) -> Result<(), midir::SendError> + Send,
    ) -> PyResult<()>
    where
        T: Send,
    {
        check_blocking_thread()?;
        let message = message.as_bytes();
        py.detach(|| {
            self.check_failed()?;
            client.check_failed()?;
            let mut native = self.lock_native(client)?;
            self.check_failed()?;
            let native = native
                .as_mut()
                .ok_or_else(|| PyRuntimeError::new_err("MIDI connection is closed"))?;
            match native_call("send", || operation(native, message)) {
                Ok(result) => result.map_err(|error| SendError::new_err(format!("send: {error}"))),
                Err(error) => {
                    self.fail(client);
                    Err(error)
                }
            }
        })
    }

    pub fn close<C>(&self, client: &Client<C>, operation: impl FnOnce(T) -> C) -> PyResult<()> {
        let mut native = self.lock_native(client)?;
        if native.is_some() {
            *client.lock_state().inspect_err(|_| {
                self.fail(client);
            })? = ClientState::Closing;
            let connection = native.take().expect("owned connection missing");
            match native_call("close", || operation(connection)) {
                Ok(restored) => *client.lock_state()? = ClientState::Available(restored),
                Err(error) => {
                    self.fail(client);
                    self.closed.store(true, Ordering::Release);
                    return Err(error);
                }
            }
            self.closed.store(true, Ordering::Release);
        }
        self.check_failed()
    }

    pub fn fail<C>(&self, client: &Client<C>) {
        self.failed.store(true, Ordering::Release);
        client.failed.store(true, Ordering::Release);
    }

    pub fn check_failed(&self) -> PyResult<()> {
        if self.failed.load(Ordering::Acquire) {
            Err(failed_error())
        } else {
            Ok(())
        }
    }

    fn lock_native<C>(&self, client: &Client<C>) -> PyResult<MutexGuard<'_, Option<T>>> {
        self.native.lock().map_err(|_| {
            self.fail(client);
            failed_error()
        })
    }
}

impl<T> Drop for Client<T> {
    fn drop(&mut self) {
        let state = self
            .state
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let native = mem::replace(state, ClientState::Closing);
        if native_call("client destruction", || drop(native)).is_err() {
            report_failure("native client destruction panicked");
        }
    }
}

enum ClientState<T> {
    Available(T),
    Connecting,
    Connected,
    Closing,
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::sync::{mpsc, Arc};
    use std::thread;
    use std::time::Duration;

    use super::*;

    #[test]
    fn panicking_connect_disables_the_client_and_a_fresh_client_still_works() {
        Python::initialize();
        Python::attach(|py| {
            let client = Client::new(37, false);
            let connection = Connection::<i32>::new();
            let error = client
                .connect(&connection, |_| panic!("connect fault"))
                .unwrap_err();
            assert!(error.is_instance_of::<PyRuntimeError>(py));
            assert!(error.value(py).to_string().contains("connect fault"));
            assert!(connection.closed());
            assert!(client.with_available(|_| Ok(())).is_err());
            assert!(client
                .connect(&Connection::<i32>::new(), |_| panic!(
                    "failed client must not reopen"
                ))
                .is_err());
            assert!(connection
                .close(&client, |_| panic!("no native handle survived"))
                .is_err());
            let fresh = Client::new(19, false);
            let reopened = Connection::new();
            fresh.connect(&reopened, Ok).unwrap();
            reopened.close(&fresh, |value| value).unwrap();
            assert_eq!(fresh.with_available(|value| Ok(*value)).unwrap(), 19);
        });
    }

    #[test]
    fn metadata_panic_disables_reuse_and_client_destruction_contains_native_panics() {
        struct Native;
        impl Drop for Native {
            fn drop(&mut self) {
                panic!("destruction fault");
            }
        }
        Python::initialize();
        Python::attach(|py| {
            let client = Client::new(Native, false);
            let result: PyResult<()> = client.with_available(|_| panic!("metadata fault"));
            assert!(result.unwrap_err().is_instance_of::<PyRuntimeError>(py));
            assert!(client
                .with_available::<()>(|_| panic!("failed client must not enumerate"))
                .is_err());
            drop(client);
        });
    }

    #[test]
    fn send_panic_allows_teardown_but_permanently_disables_connection_and_client() {
        Python::initialize();
        Python::attach(|py| {
            let client = Client::new(37, false);
            let connection = Connection::new();
            client.connect(&connection, Ok).unwrap();
            let error = connection
                .send(&client, py, &PyBytes::new(py, &[0xf8]), |_, _| {
                    panic!("send fault")
                })
                .unwrap_err();
            assert!(error.is_instance_of::<PyRuntimeError>(py));
            assert!(!connection.closed());
            assert!(connection
                .send(&client, py, &PyBytes::new(py, &[0xf8]), |_, _| panic!(
                    "failed connection must not send"
                ))
                .is_err());
            let mut restored = false;
            assert!(connection
                .close(&client, |native| {
                    restored = true;
                    native
                })
                .is_err());
            assert!(restored);
            assert!(connection.closed());
            assert!(client.with_available(|_| Ok(())).is_err());
            assert!(connection
                .close(&client, |_| panic!("native teardown must not repeat"))
                .is_err());
        });
    }

    #[test]
    fn teardown_panic_consumes_ownership_once_and_reports_failed_state() {
        Python::initialize();
        Python::attach(|py| {
            let client = Client::new(37, false);
            let connection = Connection::new();
            client.connect(&connection, Ok).unwrap();
            let error = connection
                .close(&client, |_| panic!("teardown fault"))
                .unwrap_err();
            assert!(error.is_instance_of::<PyRuntimeError>(py));
            assert!(connection.closed());
            assert!(client.with_available(|_| Ok(())).is_err());
            assert!(connection
                .close(&client, |_| panic!("native teardown must not repeat"))
                .is_err());
        });
    }

    #[test]
    fn alsa_thread_start_failure_disables_client_before_any_native_reuse() {
        Python::initialize();
        Python::attach(|py| {
            let client = Client::new(Some(Box::new(37)), false);
            let connection: Connection<Option<Box<i32>>> = Connection::new();
            let error = client
                .connect(&connection, |mut native| {
                    // ALSA moves the sequencer into the spawn closure. Failed
                    // spawn drops that closure and returns a client with None.
                    drop(native.take().unwrap());
                    Err(midir::ConnectError::other(
                        "could not start ALSA input handler thread",
                        native,
                    ))
                })
                .unwrap_err();
            assert!(error.is_instance_of::<ConnectError>(py));
            assert_eq!(
                error.value(py).to_string(),
                "connect: could not start ALSA input handler thread"
            );
            assert!(connection.closed());

            let mut reused = false;
            let result = client.with_available(|_| {
                reused = true;
                Ok(())
            });
            assert!(!reused, "damaged client reached a native operation");
            let error = result.unwrap_err();
            assert!(error.is_instance_of::<PyRuntimeError>(py));
            assert_eq!(
                error.value(py).to_string(),
                "MIDI resource failed; create a new client"
            );
            let result = client.connect(&Connection::new(), |native| {
                reused = true;
                Ok(native)
            });
            assert!(!reused, "damaged client reached native opening");
            assert!(result.unwrap_err().is_instance_of::<PyRuntimeError>(py));
            connection
                .close(&client, |_| panic!("failed open has no native connection"))
                .unwrap();

            let fresh = Client::new(Some(Box::new(19)), false);
            let reopened = Connection::new();
            fresh.connect(&reopened, Ok).unwrap();
            reopened.close(&fresh, |native| native).unwrap();
            assert_eq!(
                fresh
                    .with_available(|native| Ok(**native.as_ref().unwrap()))
                    .unwrap(),
                19
            );
        });
    }

    #[test]
    fn failed_alsa_opens_release_partial_allocations_without_client_reuse() {
        struct Sequencer {
            allocated: usize,
            live: Arc<AtomicUsize>,
            drops: Arc<AtomicUsize>,
        }
        impl Drop for Sequencer {
            fn drop(&mut self) {
                self.live.fetch_sub(self.allocated, Ordering::SeqCst);
                self.drops.fetch_add(1, Ordering::SeqCst);
            }
        }
        Python::initialize();
        Python::attach(|py| {
            for (kind, allocated) in [
                (midir::ConnectErrorKind::InvalidPort, 1),
                (
                    midir::ConnectErrorKind::Other("port_name must not contain null bytes"),
                    1,
                ),
                (
                    midir::ConnectErrorKind::Other("could not create ALSA input port"),
                    1,
                ),
                (
                    midir::ConnectErrorKind::Other("could not create ALSA input subscription"),
                    2,
                ),
                (
                    midir::ConnectErrorKind::Other("could not create ALSA output subscription"),
                    1,
                ),
            ] {
                let live = Arc::new(AtomicUsize::new(0));
                let drops = Arc::new(AtomicUsize::new(0));
                let client = Client::new(
                    Sequencer {
                        allocated: 0,
                        live: Arc::clone(&live),
                        drops: Arc::clone(&drops),
                    },
                    true,
                );
                let connection: Connection<()> = Connection::new();
                let error = client
                    .connect(&connection, |mut native| {
                        native.allocated = allocated;
                        live.fetch_add(allocated, Ordering::SeqCst);
                        Err(midir::ConnectError::new(kind, native))
                    })
                    .unwrap_err();
                assert!(error.is_instance_of::<ConnectError>(py));
                assert_eq!(error.value(py).to_string(), format!("connect: {kind}"));
                assert_eq!(
                    live.load(Ordering::SeqCst),
                    0,
                    "partial allocations survived failed open"
                );
                assert_eq!(drops.load(Ordering::SeqCst), 1);
                let mut reused = false;
                let result = client.connect(&Connection::<()>::new(), |_| {
                    reused = true;
                    Ok(())
                });
                assert!(!reused);
                assert!(result.unwrap_err().is_instance_of::<PyRuntimeError>(py));
                assert!(client.with_available(|_| Ok(())).is_err());
                assert!(connection.closed());
                connection
                    .close(&client, |_| panic!("failed open never connected"))
                    .unwrap();
                drop(client);
                assert_eq!(drops.load(Ordering::SeqCst), 1);
            }
        });
    }

    #[test]
    fn failed_open_cleanup_panic_preserves_connect_error_and_blocks_client_reuse() {
        struct Sequencer;
        impl Drop for Sequencer {
            fn drop(&mut self) {
                panic!("sequencer close fault");
            }
        }
        Python::initialize();
        Python::attach(|py| {
            let client = Client::new(Sequencer, true);
            let connection: Connection<()> = Connection::new();
            let error = client
                .connect(&connection, |native| {
                    Err(midir::ConnectError::new(
                        midir::ConnectErrorKind::InvalidPort,
                        native,
                    ))
                })
                .unwrap_err();
            assert!(error.is_instance_of::<ConnectError>(py));
            assert_eq!(error.value(py).to_string(), "connect: invalid port");
            assert!(client.with_available(|_| Ok(())).is_err());
            assert!(connection.closed());
            drop(client); // Must not retry destruction of the consumed handle.
        });
    }

    #[test]
    fn failed_connect_restores_the_same_client() {
        let client = Client::new(Box::new(37), false);
        let identity = client
            .with_available(|native| Ok(&**native as *const i32))
            .unwrap();
        let connection: Connection<Box<i32>> = Connection::new();

        let error = client
            .connect(&connection, |native| {
                assert!(client.with_available(|_| Ok(())).is_err());
                Err(midir::ConnectError::new(
                    midir::ConnectErrorKind::InvalidPort,
                    native,
                ))
            })
            .unwrap_err();

        pyo3::Python::initialize();
        pyo3::Python::attach(|py| {
            assert!(error.is_instance_of::<ConnectError>(py));
            assert_eq!(error.value(py).to_string(), "connect: invalid port");
        });
        assert_eq!(
            client
                .with_available(|native| Ok(&**native as *const i32))
                .unwrap(),
            identity
        );
        assert!(connection.closed());
    }

    #[test]
    fn closing_restores_the_client_and_allows_a_new_connection() {
        let client = Client::new(Box::new(37), true);
        let connection = Connection::new();
        client.connect(&connection, Ok).unwrap();
        assert!(!connection.closed());
        assert!(client.with_available(|_| Ok(())).is_err());
        assert!(client.connect(&Connection::new(), Ok).is_err());

        connection
            .close(&client, |native| {
                assert!(client.with_available(|_| Ok(())).is_err());
                native
            })
            .unwrap();
        connection
            .close(&client, |_| panic!("close must be idempotent"))
            .unwrap();
        assert!(connection.closed());
        assert_eq!(client.with_available(|native| Ok(**native)).unwrap(), 37);

        let reopened = Connection::new();
        client.connect(&reopened, Ok).unwrap();
        assert!(!reopened.closed());
        assert!(connection.closed());
        reopened.close(&client, |native| native).unwrap();
    }

    #[test]
    fn concurrent_close_waits_for_native_teardown() {
        let client = Arc::new(Client::new(37, false));
        let connection = Arc::new(Connection::new());
        client.connect(&connection, Ok).unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (second_started_tx, second_started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();

        thread::scope(|scope| {
            let closing_connection = Arc::clone(&connection);
            let closing_client = Arc::clone(&client);
            scope.spawn(move || {
                closing_connection
                    .close(&closing_client, |native| {
                        started_tx.send(()).unwrap();
                        release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                        native
                    })
                    .unwrap()
            });
            started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(client.with_available(|_| Ok(())).is_err());
            scope.spawn(|| {
                second_started_tx.send(()).unwrap();
                connection
                    .close(&client, |_| panic!("native close must run once"))
                    .unwrap();
                done_tx.send(()).unwrap();
            });
            second_started_rx
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
            assert_eq!(
                done_rx.recv_timeout(Duration::from_millis(50)),
                Err(mpsc::RecvTimeoutError::Timeout),
            );
            release_tx.send(()).unwrap();
            done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        });

        assert!(connection.closed());
        assert_eq!(client.with_available(|native| Ok(*native)).unwrap(), 37);
    }

    #[test]
    fn concurrent_client_operations_reject_use_without_waiting_for_the_backend() {
        let client = Client::new(37, false);
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        thread::scope(|scope| {
            let client = &client;
            scope.spawn(move || {
                client
                    .with_available(|_| {
                        started_tx.send(()).unwrap();
                        release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                        Ok(())
                    })
                    .unwrap();
            });
            started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            scope.spawn(move || {
                let metadata: PyResult<()> =
                    client.with_available(|_| panic!("busy client must reject use"));
                let connect = client.connect(&Connection::<()>::new(), |_| {
                    panic!("busy client must reject connect")
                });
                result_tx.send((metadata, connect)).unwrap();
            });
            let result = result_rx.recv_timeout(Duration::from_secs(1));
            release_tx.send(()).unwrap();
            let (metadata, connect): (PyResult<()>, PyResult<()>) = result.unwrap();
            Python::initialize();
            Python::attach(|py| {
                for error in [metadata.unwrap_err(), connect.unwrap_err()] {
                    assert!(error.is_instance_of::<PyRuntimeError>(py));
                    assert_eq!(
                        error.value(py).to_string(),
                        "MIDI client is busy with another operation"
                    );
                }
            });
        });
        assert_eq!(client.with_available(|native| Ok(*native)).unwrap(), 37);
    }

    #[test]
    fn closed_reports_without_waiting_and_remains_false_until_teardown_completes() {
        let client = Client::new(37, false);
        let connection = Connection::new();
        client.connect(&connection, Ok).unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        thread::scope(|scope| {
            let client = &client;
            let connection = &connection;
            scope.spawn(move || {
                connection
                    .close(client, |native| {
                        started_tx.send(()).unwrap();
                        release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                        native
                    })
                    .unwrap();
            });
            started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            scope.spawn(move || result_tx.send(connection.closed()).unwrap());
            let result = result_rx.recv_timeout(Duration::from_secs(1));
            release_tx.send(()).unwrap();
            assert!(!result.unwrap());
        });
        assert!(connection.closed());
        assert_eq!(client.with_available(|native| Ok(*native)).unwrap(), 37);
    }

    #[test]
    fn input_filter_configuration_survives_failed_connect_and_close() {
        let client = Client::new(midir::Ignore::None, false);
        let connection = Connection::new();
        assert_eq!(
            client.with_available(|native| Ok(*native)).unwrap(),
            midir::Ignore::None
        );
        let flags = midir::Ignore::Sysex | midir::Ignore::Time;
        client
            .with_available(|native| {
                *native = flags;
                Ok(())
            })
            .unwrap();
        let error = client
            .connect(&connection, |native| {
                assert!(client
                    .with_available(|native| {
                        *native = midir::Ignore::All;
                        Ok(())
                    })
                    .is_err());
                Err(midir::ConnectError::other(
                    "virtual creation failed",
                    native,
                ))
            })
            .unwrap_err();
        Python::initialize();
        Python::attach(|py| {
            assert!(error.is_instance_of::<ConnectError>(py));
            assert_eq!(
                error.value(py).to_string(),
                "connect: virtual creation failed"
            );
        });
        assert_eq!(client.with_available(|native| Ok(*native)).unwrap(), flags);
        client.connect(&connection, Ok).unwrap();
        assert!(client
            .with_available(|native| {
                *native = midir::Ignore::None;
                Ok(())
            })
            .is_err());
        connection.close(&client, |native| native).unwrap();
        assert_eq!(client.with_available(|native| Ok(*native)).unwrap(), flags);
    }

    #[test]
    fn send_preserves_bytes_and_native_errors_without_closing_the_connection() {
        Python::initialize();
        Python::attach(|py| {
            let client = Client::new(Vec::<Vec<u8>>::new(), false);
            let connection = Connection::new();
            client.connect(&connection, Ok).unwrap();
            for bytes in [vec![], vec![0], vec![0x90, 60, 127], vec![0xf0, 0, 1, 0xf7]] {
                connection
                    .send(&client, py, &PyBytes::new(py, &bytes), |native, message| {
                        native.push(message.to_vec());
                        Ok(())
                    })
                    .unwrap();
            }
            for native_error in [
                midir::SendError::InvalidData("invalid data"),
                midir::SendError::Other("driver error"),
            ] {
                let error = connection
                    .send(&client, py, &PyBytes::new(py, &[0xf8]), |_, _| {
                        Err(native_error)
                    })
                    .unwrap_err();
                assert!(error.is_instance_of::<SendError>(py));
                assert_eq!(error.value(py).to_string(), format!("send: {native_error}"));
                assert!(!connection.closed());
            }
            connection
                .send(
                    &client,
                    py,
                    &PyBytes::new(py, &[0xf8]),
                    |native, message| {
                        native.push(message.to_vec());
                        Ok(())
                    },
                )
                .unwrap();
            connection.close(&client, |native| native).unwrap();
            assert_eq!(
                client.with_available(|native| Ok(native.clone())).unwrap(),
                vec![
                    vec![],
                    vec![0],
                    vec![0x90, 60, 127],
                    vec![0xf0, 0, 1, 0xf7],
                    vec![0xf8]
                ]
            );
            let error = connection
                .send(&client, py, &PyBytes::new(py, &[0xf8]), |_, _| {
                    panic!("closed connection must not send")
                })
                .unwrap_err();
            assert!(error.is_instance_of::<PyRuntimeError>(py));
        });
    }

    #[test]
    fn an_open_snapshot_does_not_reserve_the_connection_against_another_closer() {
        Python::initialize();
        Python::attach(|py| {
            let client = Client::new(37, false);
            let connection = Connection::new();
            client.connect(&connection, Ok).unwrap();
            assert!(!connection.closed());
            py.detach(|| {
                thread::scope(|scope| {
                    scope
                        .spawn(|| connection.close(&client, |native| native))
                        .join()
                        .unwrap()
                })
            })
            .unwrap();
            let error = connection
                .send(&client, py, &PyBytes::new(py, &[0xf8]), |_, _| {
                    panic!("a stale snapshot must not allow native send after close")
                })
                .unwrap_err();
            assert!(error.is_instance_of::<PyRuntimeError>(py));
            assert_eq!(error.value(py).to_string(), "MIDI connection is closed");
            assert!(connection.closed());
            assert_eq!(client.with_available(|native| Ok(*native)).unwrap(), 37);
        });
    }

    #[test]
    fn close_waits_for_a_send_in_progress_while_python_is_released() {
        Python::initialize();
        Python::attach(|py| {
            let client = Client::new(Vec::<u8>::new(), false);
            let connection = Connection::new();
            client.connect(&connection, Ok).unwrap();
            let (started, sending) = mpsc::channel();
            let (release, released) = mpsc::channel();
            let (closing, close_started) = mpsc::channel();
            let (done, completed) = mpsc::channel();
            let client = &client;
            let connection = &connection;
            py.detach(move || {
                thread::scope(|scope| {
                    scope.spawn(|| {
                        Python::attach(|py| {
                            connection
                                .send(
                                    client,
                                    py,
                                    &PyBytes::new(py, &[0x90, 60, 127]),
                                    move |native, bytes| {
                                        started.send(()).unwrap();
                                        released.recv_timeout(Duration::from_secs(5)).unwrap();
                                        native.extend_from_slice(bytes);
                                        Ok(())
                                    },
                                )
                                .unwrap();
                        })
                    });
                    sending.recv_timeout(Duration::from_secs(5)).unwrap();
                    assert!(!connection.closed());
                    scope.spawn(|| {
                        closing.send(()).unwrap();
                        connection.close(client, |native| native).unwrap();
                        done.send(()).unwrap();
                    });
                    close_started.recv_timeout(Duration::from_secs(5)).unwrap();
                    assert_eq!(
                        completed.recv_timeout(Duration::from_millis(50)),
                        Err(mpsc::RecvTimeoutError::Timeout)
                    );
                    release.send(()).unwrap();
                    completed.recv_timeout(Duration::from_secs(5)).unwrap();
                })
            });
            assert!(connection.closed());
            assert_eq!(
                client.with_available(|native| Ok(native.clone())).unwrap(),
                vec![0x90, 60, 127]
            );
        });
    }
}
