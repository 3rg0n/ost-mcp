//! The [`Mailbox`] trait: one interface, three backends.
//!
//! `ost-mcp`'s MCP surface (`server.rs`, `vtab.rs`) is written against this
//! trait rather than against any one backend, so `crates/ost` (OST/PST),
//! `crates/mac-outlook` (Mac Outlook's SQLite + `.olk15*` store) and a future
//! `.olm` reader can all sit behind it. See
//! `docs/adr/0001-mailbox-backend-trait.md` for why.
//!
//! Ids are `i64` everywhere at this boundary, even though the OST backend's
//! own node ids are `u32`: a SQLite rowid is `i64`, the public `nid` column
//! DuckDB exposes is already `BIGINT`, and one cast at the OST adapter is
//! cheaper than a second narrow-id path through every tool.

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    /// The store is the right kind of file but a structure did not match
    /// what this reader expects.
    Format(String),
    /// A store variant this reader does not implement.
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

#[derive(Clone, Debug)]
pub struct Folder {
    pub id: i64,
    pub parent_id: Option<i64>,
    /// `None` when the backend has no real name to report — including when
    /// the only value on disk is a known-inert placeholder (see
    /// `docs/mac-outlook-format.md` §3.1). A placeholder is not a name.
    pub name: Option<String>,
    /// Slash-delimited path from the store root.
    pub path: String,
    pub item_count: Option<i64>,
    pub unread_count: Option<i64>,
    pub has_subfolders: bool,
    /// A search folder (or equivalent virtual folder) owns no contents of its
    /// own and is skipped when a caller sweeps every folder's messages.
    pub is_search_folder: bool,
}

/// A message as seen from its folder's listing: cheap to list in bulk, with
/// no body and no attachment payloads.
#[derive(Clone, Debug)]
pub struct MessageRow {
    pub id: i64,
    pub folder_id: i64,
    pub subject: Option<String>,
    pub sender_name: Option<String>,
    pub sender_email: Option<String>,
    pub delivered_us: Option<i64>,
    pub submitted_us: Option<i64>,
    pub modified_us: Option<i64>,
    pub size: Option<i64>,
    pub unread: Option<bool>,
    pub has_attachments: Option<bool>,
    pub message_class: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Recipient {
    /// 1 = To, 2 = Cc, 3 = Bcc, matching `PidTagRecipientType`; other backends
    /// map onto the same three codes.
    pub kind: Option<i32>,
    pub name: Option<String>,
    pub email: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Attachment {
    pub id: i64,
    pub filename: Option<String>,
    pub mime: Option<String>,
    pub content_id: Option<String>,
    pub declared_size: Option<i64>,
    /// Payload length in bytes, or `None` when the backend cannot report one
    /// without reading the payload (or the attachment is not a byte stream).
    pub data_len: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct Message {
    pub id: i64,
    pub subject: Option<String>,
    pub sender_name: Option<String>,
    pub sender_email: Option<String>,
    pub display_to: Option<String>,
    pub display_cc: Option<String>,
    pub display_bcc: Option<String>,
    pub delivered_us: Option<i64>,
    pub submitted_us: Option<i64>,
    pub modified_us: Option<i64>,
    pub size: Option<i64>,
    pub unread: Option<bool>,
    pub message_class: Option<String>,
    pub internet_message_id: Option<String>,
    pub conversation_topic: Option<String>,
    /// Plain text, if the backend has it.
    pub body_plain: Option<String>,
    pub body_html: Option<String>,
    pub body_rtf: Option<String>,
    pub recipients: Vec<Recipient>,
    pub attachments: Vec<Attachment>,
}

/// One mounted mailbox, whatever is backing it.
pub trait Mailbox: Send + Sync {
    /// A short, stable tag identifying the backend, e.g. `"ost-v36"`,
    /// `"mac-olk15"`. Surfaced in `store_info`, not parsed by anything.
    fn kind(&self) -> &'static str;
    fn display_name(&self) -> Option<String>;
    /// Every folder reachable from the root, breadth-first, with paths.
    fn folders(&self) -> Result<Vec<Folder>>;
    /// Rows of one folder's contents. Any field can be `None` for reasons
    /// unrelated to the message — see each backend's own documentation.
    fn messages(&self, folder_id: i64) -> Result<Vec<MessageRow>>;
    /// A full message: properties, bodies, recipients, attachment metadata.
    fn message(&self, id: i64) -> Result<Message>;
    /// Attachment metadata for a message. Empty when it has none.
    fn attachments(&self, message_id: i64) -> Result<Vec<Attachment>>;
    /// Payload bytes of one attachment.
    fn attachment_bytes(&self, message_id: i64, attachment_id: i64) -> Result<Vec<u8>>;
}
