
use std::{error::Error, fmt, io};
use bytes::Bytes;

#[derive(Debug, PartialEq)]
pub enum Frame {
    Get {
        key: Bytes,
    },
    Put {
        key: Bytes,
        timestamp: u64,
        value: Bytes,
    },
    Delete {
        key: Bytes,
        timestamp: u64,
    },
}

#[derive(Debug)]
pub enum ParseError {
    Incomplete,
    InvalidProtocol,
    Io(io::Error),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Incomplete => write!(f, "incomplete frame"),
            Self::InvalidProtocol => write!(f, "invalid protocol frame"),
            Self::Io(err) => write!(f, "I/O error: {err}"),
        }
    }
}

impl Error for ParseError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for ParseError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}
