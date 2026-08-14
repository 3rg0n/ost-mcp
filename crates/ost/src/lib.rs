//! Read-only reader for Outlook Personal Folder File (PFF) stores.
//!
//! Handles two 64-bit Unicode variants:
//!
//! - `wVer` 23 — the PST/OST layout documented by MS-PST (512-byte pages).
//! - `wVer` 36 — the undocumented 4 KB-page OST written by Outlook 2013+, whose
//!   deltas from MS-PST are recorded in `docs/ost-v36-format.md`.
//!
//! I/O is memory-mapped, which is not an optimisation: Outlook holds a byte-range
//! lock on bytes 0..1023 of a live OST, and a mapped view bypasses it. This is the
//! only way to read the store of a running Outlook.

pub mod ltp;
pub mod ndb;
pub mod props;
pub mod store;

pub use ndb::Pff;
pub use store::{Attachment, Folder, Message, MessageRow, Recipient, Store};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    /// The file is a PFF but a structure did not match the spec.
    Format(String),
    /// A valid PFF variant this reader does not implement.
    Unsupported(String),
    NotFound(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "io: {e}"),
            Error::Format(m) => write!(f, "malformed: {m}"),
            Error::Unsupported(m) => write!(f, "unsupported: {m}"),
            Error::NotFound(m) => write!(f, "not found: {m}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}
