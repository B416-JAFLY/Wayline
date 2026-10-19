//! Stable transfer failure categories for every user interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferFailureKind {
    Declined,
    Cancelled,
    Connection,
    Validation,
    Other,
}

impl TransferFailureKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Declined => "declined",
            Self::Cancelled => "cancelled",
            Self::Connection => "connection",
            Self::Validation => "validation",
            Self::Other => "other",
        }
    }

    pub(crate) fn from_io(error: &std::io::Error) -> Self {
        use std::io::ErrorKind::*;
        match error.kind() {
            ConnectionAborted | ConnectionReset | ConnectionRefused | NotConnected | BrokenPipe
            | UnexpectedEof | TimedOut | WouldBlock => Self::Connection,
            InvalidData => Self::Validation,
            _ => Self::Other,
        }
    }
}
