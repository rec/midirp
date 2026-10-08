use std::mem;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, TryLockError};

use pyo3::exceptions::PyRuntimeError;
use pyo3::types::{PyBytes, PyBytesMethods};
use pyo3::{Bound, PyResult, Python};

use crate::errors::{ConnectError, SendError};

/// Owns a client except while a connection owns its native resource.
pub struct Client<T> {
    state: Mutex<ClientState<T>>,
}

/// Serializes native teardown so every close caller waits for completion.
pub struct Connection<T> {
    native: Mutex<Option<T>>,
    closed: AtomicBool,
}

impl<T> Client<T> {
    pub fn new(native: T) -> Self {
        Self {
            state: Mutex::new(ClientState::Available(native)),
        }
    }

    pub fn with_available<R>(&self, operation: impl FnOnce(&mut T) -> PyResult<R>) -> PyResult<R> {
        let mut state = self.try_state()?;
        match &mut *state {
            ClientState::Available(native) => operation(native),
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

        // Native connect may block. Do not hold the client lock across it.
        match operation(native) {
            Ok(native) => {
                {
                    let mut handle = connection.native.lock().expect("connection lock poisoned");
                    *handle = Some(native);
                    connection.closed.store(false, Ordering::Release);
                }
                *self.state.lock().expect("client state lock poisoned") = ClientState::Connected;
                Ok(())
            }
            Err(error) => {
                let message = format!("connect: {error}");
                *self.state.lock().expect("client state lock poisoned") =
                    ClientState::Available(error.into_inner());
                Err(ConnectError::new_err(message))
            }
        }
    }

    fn try_state(&self) -> PyResult<MutexGuard<'_, ClientState<T>>> {
        match self.state.try_lock() {
            Ok(state) => Ok(state),
            Err(TryLockError::WouldBlock) => Err(PyRuntimeError::new_err(
                "MIDI client is busy with another operation",
            )),
            Err(TryLockError::Poisoned(_)) => panic!("client state lock poisoned"),
        }
    }
}

impl<T> Connection<T> {
    pub fn new() -> Self {
        Self {
            native: Mutex::new(None),
            closed: AtomicBool::new(true),
        }
    }

    pub fn closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    pub fn send(
        &self,
        py: Python<'_>,
        message: &Bound<'_, PyBytes>,
        operation: impl FnOnce(&mut T, &[u8]) -> Result<(), midir::SendError> + Send,
    ) -> PyResult<()>
    where
        T: Send,
    {
        // PyBytes owns immutable storage for the entire detached call. Extract
        // the slice while attached, then release the GIL before taking the lock.
        let message = message.as_bytes();
        py.detach(|| {
            let mut native = self.native.lock().expect("connection lock poisoned");
            let native = native
                .as_mut()
                .ok_or_else(|| PyRuntimeError::new_err("MIDI connection is closed"))?;
            operation(native, message).map_err(|error| SendError::new_err(format!("send: {error}")))
        })
    }

    pub fn close<C>(&self, client: &Client<C>, operation: impl FnOnce(T) -> C) {
        let mut native = self.native.lock().expect("connection lock poisoned");
        if let Some(connection) = native.take() {
            *client.state.lock().expect("client state lock poisoned") = ClientState::Closing;
            let restored = operation(connection);
            *client.state.lock().expect("client state lock poisoned") =
                ClientState::Available(restored);
            // Publish completion only after the original client is available again.
            self.closed.store(true, Ordering::Release);
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
    use std::sync::{mpsc, Arc};
    use std::thread;
    use std::time::Duration;

    use super::*;

    #[test]
    fn failed_connect_restores_the_same_client() {
        let client = Client::new(Box::new(37));
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
        let client = Client::new(Box::new(37));
        let connection = Connection::new();
        client.connect(&connection, Ok).unwrap();
        assert!(!connection.closed());
        assert!(client.with_available(|_| Ok(())).is_err());
        assert!(client.connect(&Connection::new(), Ok).is_err());

        connection.close(&client, |native| {
            assert!(client.with_available(|_| Ok(())).is_err());
            native
        });
        connection.close(&client, |_| panic!("close must be idempotent"));
        assert!(connection.closed());
        assert_eq!(client.with_available(|native| Ok(**native)).unwrap(), 37);

        let reopened = Connection::new();
        client.connect(&reopened, Ok).unwrap();
        assert!(!reopened.closed());
        assert!(connection.closed());
        reopened.close(&client, |native| native);
    }

    #[test]
    fn concurrent_close_waits_for_native_teardown() {
        let client = Arc::new(Client::new(37));
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
                closing_connection.close(&closing_client, |native| {
                    started_tx.send(()).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                    native
                })
            });
            started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(client.with_available(|_| Ok(())).is_err());
            scope.spawn(|| {
                second_started_tx.send(()).unwrap();
                connection.close(&client, |_| panic!("native close must run once"));
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
        let client = Client::new(37);
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
        let client = Client::new(37);
        let connection = Connection::new();
        client.connect(&connection, Ok).unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        thread::scope(|scope| {
            let client = &client;
            let connection = &connection;
            scope.spawn(move || {
                connection.close(client, |native| {
                    started_tx.send(()).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                    native
                });
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
        let client = Client::new(midir::Ignore::None);
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
        connection.close(&client, |native| native);
        assert_eq!(client.with_available(|native| Ok(*native)).unwrap(), flags);
    }

    #[test]
    fn send_preserves_bytes_and_native_errors_without_closing_the_connection() {
        Python::initialize();
        Python::attach(|py| {
            let client = Client::new(Vec::<Vec<u8>>::new());
            let connection = Connection::new();
            client.connect(&connection, Ok).unwrap();
            for bytes in [vec![], vec![0], vec![0x90, 60, 127], vec![0xf0, 0, 1, 0xf7]] {
                connection
                    .send(py, &PyBytes::new(py, &bytes), |native, message| {
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
                    .send(py, &PyBytes::new(py, &[0xf8]), |_, _| Err(native_error))
                    .unwrap_err();
                assert!(error.is_instance_of::<SendError>(py));
                assert_eq!(error.value(py).to_string(), format!("send: {native_error}"));
                assert!(!connection.closed());
            }
            connection
                .send(py, &PyBytes::new(py, &[0xf8]), |native, message| {
                    native.push(message.to_vec());
                    Ok(())
                })
                .unwrap();
            connection.close(&client, |native| native);
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
                .send(py, &PyBytes::new(py, &[0xf8]), |_, _| {
                    panic!("closed connection must not send")
                })
                .unwrap_err();
            assert!(error.is_instance_of::<PyRuntimeError>(py));
        });
    }

    #[test]
    fn close_waits_for_a_send_in_progress_while_python_is_released() {
        Python::initialize();
        Python::attach(|py| {
            let client = Client::new(Vec::<u8>::new());
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
                        connection.close(client, |native| native);
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
