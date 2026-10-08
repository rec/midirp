use std::mem;
use std::sync::Mutex;

use pyo3::exceptions::PyRuntimeError;
use pyo3::PyResult;

use crate::errors::ConnectError;

/// Owns a client except while a connection owns its native resource.
pub struct Client<T> {
    state: Mutex<ClientState<T>>,
}

/// Serializes native teardown so every close caller waits for completion.
pub struct Connection<T> {
    native: Mutex<Option<T>>,
}

impl<T> Client<T> {
    pub fn new(native: T) -> Self {
        Self {
            state: Mutex::new(ClientState::Available(native)),
        }
    }

    pub fn with_available<R>(&self, operation: impl FnOnce(&T) -> PyResult<R>) -> PyResult<R> {
        let state = self.state.lock().expect("client state lock poisoned");
        match &*state {
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
            let mut state = self.state.lock().expect("client state lock poisoned");
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
                *connection.native.lock().expect("connection lock poisoned") = Some(native);
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
}

impl<T> Connection<T> {
    pub fn new() -> Self {
        Self {
            native: Mutex::new(None),
        }
    }

    pub fn closed(&self) -> bool {
        self.native
            .lock()
            .expect("connection lock poisoned")
            .is_none()
    }

    pub fn close<C>(&self, client: &Client<C>, operation: impl FnOnce(T) -> C) {
        let mut native = self.native.lock().expect("connection lock poisoned");
        if let Some(connection) = native.take() {
            *client.state.lock().expect("client state lock poisoned") = ClientState::Closing;
            let restored = operation(connection);
            *client.state.lock().expect("client state lock poisoned") =
                ClientState::Available(restored);
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
}
