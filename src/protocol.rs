use std::io;
use bytes::Bytes;

#[derive(Debug, PartialEq)]
pub enum Frame {
    Get { key: String },
    Put { key: String, timestamp: u64, value: Bytes },
    Delete { key: String, teimstamp: u64}
}

#[derive(Debug)]
pub enum ParseError {
    Incomplete,
    InvalidProtocol,
    Io(io::Error),
}
