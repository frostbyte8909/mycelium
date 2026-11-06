
use std::io;
use tokio::sync::{mpsc, oneshot};

use crate::storage::Bitcask;

pub enum DbCommand {
    Get {
        key: Vec<u8>,
        responder: oneshot::Sender<io::Result<Option<Vec<u8>>>>,
    },
    Put {
        key: Vec<u8>,
        value: Vec<u8>,
        timestamp: u64,
        responder: oneshot::Sender<io::Result<()>>,
    },
    Delete {
        key: Vec<u8>,
        timestamp: u64,
        responder: oneshot::Sender<io::Result<()>>,
    },
}

pub struct DbActor {
    receiver: mpsc::Receiver<DbCommand>,
    store: Bitcask,
}

impl DbActor {
    pub fn new(
        receiver: mpsc::Receiver<DbCommand>,
        store: Bitcask,
    ) -> Self {
        Self { receiver, store }
    }

    pub fn run(mut self) {
        while let Some(cmd) = self.receiver.blocking_recv() {
            match cmd {
                DbCommand::Get { key, responder } => {
                    let result = self.store.get(&key);
                    let _ = responder.send(result);
                }

                DbCommand::Put {
                    key,
                    value,
                    timestamp,
                    responder,
                } => {
                    let result = self.store.put(&key, &value, timestamp);
                    let _ = responder.send(result);
                }

                DbCommand::Delete {
                    key,
                    timestamp,
                    responder,
                } => {
                    let result = self.store.delete(&key, timestamp);
                    let _ = responder.send(result);
                }
            }
        }
    }
}

#[derive(Clone)]
pub struct DbHandle {
    sender: mpsc::Sender<DbCommand>,
}

impl DbHandle {
    pub fn new(sender: mpsc::Sender<DbCommand>) -> Self {
        Self { sender }
    }

    pub async fn get(&self, key: Vec<u8>) -> io::Result<Option<Vec<u8>>> {
        let (responder, response) = oneshot::channel();

        self.sender
            .send(DbCommand::Get { key, responder })
            .await
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "database actor has stopped",
                )
            })?;

        response.await.map_err(|_| {
            io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "database actor dropped the response",
            )
        })?
    }

    pub async fn put(
        &self,
        key: Vec<u8>,
        value: Vec<u8>,
        timestamp: u64,
    ) -> io::Result<()> {
        let (responder, response) = oneshot::channel();

        self.sender
            .send(DbCommand::Put {
                key,
                value,
                timestamp,
                responder,
            })
            .await
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "database actor has stopped",
                )
            })?;

        response.await.map_err(|_| {
            io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "database actor dropped the response",
            )
        })?
    }

    pub async fn delete(
        &self,
        key: Vec<u8>,
        timestamp: u64,
    ) -> io::Result<()> {
        let (responder, response) = oneshot::channel();

        self.sender
            .send(DbCommand::Delete {
                key,
                timestamp,
                responder,
            })
            .await
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "database actor has stopped",
                )
            })?;

        response.await.map_err(|_| {
            io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "database actor dropped the response",
            )
        })?
    }
}
