//! The MCP surface: seven tools over one mounted store.
//!
//! Three of them (`list_folders`, `search`, `sql`) go through DuckDB, which reads
//! the OST through the table functions in [`crate::vtab`]. The other four go
//! straight to the reader, because a body or an attachment payload is a
//! single-node read that SQL would only get in the way of.
//!
//! Tool bodies are synchronous. They run on the multi-threaded runtime rather
//! than under `spawn_blocking`: a stdio server has one client, so a slow sweep
//! has nothing to compete with, and stdin stays readable on another worker.

use std::sync::{Arc, Mutex};

use base64::Engine;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::ErrorData;
use rmcp::{tool, tool_handler, tool_router, Json, ServerHandler};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use ost::props::format_time_us;
use ost::Store;

use crate::sql;

/// Most tools cap their output; a model does not benefit from 40,000 rows.
const DEFAULT_LIMIT: usize = 200;
const MAX_LIMIT: usize = 5_000;
/// Bodies are truncated at this many characters unless the caller asks for more.
const DEFAULT_BODY_CHARS: usize = 20_000;
/// Attachment payloads are truncated at this many bytes unless asked otherwise.
const DEFAULT_ATTACH_BYTES: usize = 1 << 20;

#[derive(Clone)]
pub struct OstServer {
    store: Arc<Store>,
    /// `Connection` is `Send` but not `Sync`, and the handler is shared, so
    /// every query serialises through this lock. Queries are single-threaded
    /// anyway — the table functions set `max_threads` to 1.
    conn: Arc<Mutex<duckdb::Connection>>,
    path: String,
    /// Built once at startup rather than per request.
    tool_router: ToolRouter<Self>,
}

fn internal(e: impl std::fmt::Display) -> ErrorData {
    ErrorData::internal_error(e.to_string(), None)
}

fn bad_request(e: impl std::fmt::Display) -> ErrorData {
    ErrorData::invalid_params(e.to_string(), None)
}

fn clamp_limit(limit: Option<usize>) -> usize {
    limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

// ------------------------------------------------------------------- requests

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SqlRequest {
    /// A single read-only SQL statement. The tables are `folders`, `messages`
    /// and the table function `ost_attachments(message_nid => <nid>)`.
    pub query: String,
    /// Maximum rows to return (default 200, maximum 5000).
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchRequest {
    /// Case-insensitive substring matched against subject, sender name and
    /// sender address. Omit to filter only by the other fields.
    pub text: Option<String>,
    /// Case-insensitive substring matched against the folder path, e.g.
    /// `Inbox`.
    pub folder: Option<String>,
    /// Only messages delivered on or after this `YYYY-MM-DD` date.
    pub since: Option<String>,
    /// Only messages delivered before this `YYYY-MM-DD` date.
    pub until: Option<String>,
    /// Only messages that carry at least one attachment.
    pub with_attachments: Option<bool>,
    /// Maximum rows to return (default 200, maximum 5000).
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FoldersRequest {
    /// Case-insensitive substring matched against the folder path. Omit for the
    /// whole tree.
    pub contains: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MessageRequest {
    /// Node id, as returned in the `nid` column of `messages`.
    pub nid: u32,
    /// Characters of body text to return (default 20000). The reply says
    /// whether it was cut.
    pub max_body_chars: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct NidRequest {
    /// Node id, as returned in the `nid` column of `messages`.
    pub nid: u32,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AttachmentRequest {
    /// Node id of the message that owns the attachment.
    pub message_nid: u32,
    /// Node id of the attachment, from `list_attachments`.
    pub attachment_nid: u32,
    /// Bytes to return (default 1048576).
    pub max_bytes: Option<usize>,
}

// -------------------------------------------------------------------- replies

#[derive(Debug, Serialize, JsonSchema)]
pub struct StoreInfo {
    pub path: String,
    pub display_name: Option<String>,
    /// PFF format version: 23 for a documented PST/OST, 36 for the 4 KB-page
    /// OST written by Outlook 2013 and later.
    pub version: u16,
    pub bytes: u64,
    pub folders: usize,
    pub tables: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RowsReply {
    pub columns: Vec<String>,
    pub row_count: usize,
    /// True when the limit cut the result short.
    pub truncated: bool,
    pub rows: Vec<serde_json::Value>,
}

impl From<sql::Rows> for RowsReply {
    fn from(r: sql::Rows) -> Self {
        RowsReply {
            columns: r.columns,
            row_count: r.rows.len(),
            truncated: r.truncated,
            rows: r.rows,
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RecipientReply {
    /// `to`, `cc`, `bcc`, or the raw code when it is none of those.
    pub kind: String,
    pub name: Option<String>,
    pub email: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct AttachmentReply {
    pub nid: u32,
    pub filename: Option<String>,
    pub mime: Option<String>,
    pub content_id: Option<String>,
    pub declared_size: Option<i32>,
    /// Payload length in bytes, or null when the attachment is an embedded
    /// message rather than a byte stream.
    pub data_len: Option<usize>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct MessageReply {
    pub nid: u32,
    pub subject: Option<String>,
    pub sender_name: Option<String>,
    pub sender_email: Option<String>,
    pub to: Option<String>,
    pub cc: Option<String>,
    pub delivered: Option<String>,
    pub submitted: Option<String>,
    pub modified: Option<String>,
    pub size: Option<i32>,
    pub unread: Option<bool>,
    pub message_class: Option<String>,
    pub internet_message_id: Option<String>,
    pub conversation_topic: Option<String>,
    /// Plain text if the message has it, otherwise HTML, otherwise decompressed
    /// RTF. `body_format` says which.
    pub body: Option<String>,
    pub body_format: Option<String>,
    pub body_truncated: bool,
    /// Which body variants the message actually carries.
    pub bodies_available: Vec<String>,
    pub recipients: Vec<RecipientReply>,
    pub attachments: Vec<AttachmentReply>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct AttachmentDataReply {
    pub message_nid: u32,
    pub attachment_nid: u32,
    pub filename: Option<String>,
    pub mime: Option<String>,
    pub total_bytes: usize,
    pub returned_bytes: usize,
    pub truncated: bool,
    /// Set when the payload is valid UTF-8 text.
    pub text: Option<String>,
    /// Set when it is not: standard base64 of the returned bytes.
    pub base64: Option<String>,
}

// -------------------------------------------------------------------- reports
//
// These are plain functions rather than methods on the server so that the CLI in
// `main` produces byte-for-byte the same JSON as the MCP tool of the same name.
// One shape, two transports — a reply that only one of them can emit is a reply
// that drifts.

pub fn store_info(store: &Store, path: &str) -> Result<StoreInfo, ost::Error> {
    Ok(StoreInfo {
        path: path.to_string(),
        display_name: store.display_name(),
        version: store.pff.ver,
        bytes: store.pff.len() as u64,
        folders: store.folders()?.len(),
        tables: vec![
            "folders(nid, parent_nid, name, path, item_count, unread_count, has_subfolders, is_search_folder)".into(),
            "messages(nid, folder_nid, folder_path, subject, sender_name, sender_email, delivered, submitted, modified, size, unread, has_attachments, message_class)".into(),
            "ost_attachments(message_nid => <nid>) -> (message_nid, nid, filename, mime, content_id, declared_size, data_len)".into(),
        ],
    })
}

pub fn message_report(
    store: &Store,
    nid: u32,
    max_body_chars: Option<usize>,
) -> Result<MessageReply, ost::Error> {
    let m = store.message(nid)?;

    let mut available = Vec::new();
    if m.body_plain.is_some() {
        available.push("plain".to_string());
    }
    if m.body_html.is_some() {
        available.push("html".to_string());
    }
    if m.body_rtf.is_some() {
        available.push("rtf".to_string());
    }
    // Prefer plain text: it is what a model can read without markup, and an
    // HTML body is usually the same message.
    let (body_format, full_body) = match (m.body_plain, m.body_html, m.body_rtf) {
        (Some(p), _, _) => (Some("plain"), Some(p)),
        (None, Some(h), _) => (Some("html"), Some(h)),
        (None, None, Some(r)) => (Some("rtf"), Some(r)),
        _ => (None, None),
    };
    let cap = max_body_chars.unwrap_or(DEFAULT_BODY_CHARS);
    let mut truncated = false;
    let body = full_body.map(|b| {
        if b.chars().count() > cap {
            truncated = true;
            b.chars().take(cap).collect()
        } else {
            b
        }
    });

    Ok(MessageReply {
        nid: m.nid,
        subject: m.subject,
        sender_name: m.sender_name,
        sender_email: m.sender_email,
        to: m.display_to,
        cc: m.display_cc,
        delivered: m.delivered_us.map(format_time_us),
        submitted: m.submitted_us.map(format_time_us),
        modified: m.modified_us.map(format_time_us),
        size: m.size,
        unread: m.unread,
        message_class: m.message_class,
        internet_message_id: m.internet_message_id,
        conversation_topic: m.conversation_topic,
        body,
        body_format: body_format.map(str::to_string),
        body_truncated: truncated,
        bodies_available: available,
        recipients: m
            .recipients
            .into_iter()
            .map(|r| RecipientReply {
                kind: match r.kind {
                    Some(1) => "to".to_string(),
                    Some(2) => "cc".to_string(),
                    Some(3) => "bcc".to_string(),
                    other => format!("{other:?}"),
                },
                name: r.name,
                email: r.email,
            })
            .collect(),
        attachments: m.attachments.into_iter().map(attachment_reply).collect(),
    })
}

pub fn attachment_list(store: &Store, nid: u32) -> Result<Vec<AttachmentReply>, ost::Error> {
    Ok(store
        .attachments(nid)?
        .into_iter()
        .map(attachment_reply)
        .collect())
}

pub fn attachment_data(
    store: &Store,
    message_nid: u32,
    attachment_nid: u32,
    max_bytes: Option<usize>,
) -> Result<AttachmentDataReply, ost::Error> {
    let meta = store
        .attachments(message_nid)?
        .into_iter()
        .find(|a| a.nid == attachment_nid);
    let bytes = store.attachment_bytes(message_nid, attachment_nid)?;

    let cap = max_bytes.unwrap_or(DEFAULT_ATTACH_BYTES);
    let total = bytes.len();
    let truncated = total > cap;
    let slice = &bytes[..total.min(cap)];
    // Truncation can split a multi-byte character, so a cut payload is only
    // reported as text when the cut lands on a boundary.
    let (text, b64) = match std::str::from_utf8(slice) {
        Ok(s) if !s.contains('\0') => (Some(s.to_string()), None),
        _ => (
            None,
            Some(base64::engine::general_purpose::STANDARD.encode(slice)),
        ),
    };
    Ok(AttachmentDataReply {
        message_nid,
        attachment_nid,
        filename: meta.as_ref().and_then(|m| m.filename.clone()),
        mime: meta.as_ref().and_then(|m| m.mime.clone()),
        total_bytes: total,
        returned_bytes: slice.len(),
        truncated,
        text,
        base64: b64,
    })
}

// --------------------------------------------------------------------- server

/// A missing node is the caller's mistake, not the server's.
fn from_ost(e: ost::Error) -> ErrorData {
    match e {
        ost::Error::NotFound(msg) => bad_request(msg),
        other => internal(other),
    }
}

#[tool_router]
impl OstServer {
    pub fn new(store: Arc<Store>, conn: duckdb::Connection, path: String) -> Self {
        OstServer {
            store,
            conn: Arc::new(Mutex::new(conn)),
            path,
            tool_router: Self::tool_router(),
        }
    }

    fn run(&self, sql_text: &str, params: Vec<duckdb::types::Value>, limit: usize) -> Result<RowsReply, ErrorData> {
        let conn = self.conn.lock().map_err(internal)?;
        sql::query(&conn, sql_text, duckdb::params_from_iter(params), limit)
            .map(RowsReply::from)
            .map_err(internal)
    }

    /// What is mounted: the file, its format version, and the queryable tables.
    #[tool(description = "Describe the mounted Outlook store: path, format version, size, folder count and the tables available to `sql`.")]
    async fn store_info(&self) -> Result<Json<StoreInfo>, ErrorData> {
        Ok(Json(store_info(&self.store, &self.path).map_err(internal)?))
    }

    /// The folder tree. Cheap enough to return whole.
    #[tool(description = "List mail folders with their paths and item counts.")]
    async fn list_folders(
        &self,
        Parameters(req): Parameters<FoldersRequest>,
    ) -> Result<Json<RowsReply>, ErrorData> {
        let (clause, params) = match req.contains {
            Some(c) => (
                "WHERE path ILIKE ?",
                vec![duckdb::types::Value::Text(format!("%{c}%"))],
            ),
            None => ("", Vec::new()),
        };
        let text = format!(
            "SELECT nid, path, name, item_count, unread_count, is_search_folder \
             FROM folders {clause} ORDER BY path"
        );
        Ok(Json(self.run(&text, params, MAX_LIMIT)?))
    }

    /// Structured search, so the common case needs no SQL.
    #[tool(description = "Find messages by text (subject or sender), folder, date range or attachment presence. Returns newest first.")]
    async fn search(
        &self,
        Parameters(req): Parameters<SearchRequest>,
    ) -> Result<Json<RowsReply>, ErrorData> {
        // Only the filters the caller supplied are built into the statement, so
        // no parameter is ever bound as an untyped NULL.
        let mut clauses: Vec<String> = Vec::new();
        let mut params: Vec<duckdb::types::Value> = Vec::new();
        if let Some(t) = req.text.filter(|t| !t.is_empty()) {
            clauses.push(
                "(subject ILIKE ? OR sender_name ILIKE ? OR sender_email ILIKE ?)".to_string(),
            );
            let pat = duckdb::types::Value::Text(format!("%{t}%"));
            params.extend([pat.clone(), pat.clone(), pat]);
        }
        if let Some(f) = req.folder.filter(|f| !f.is_empty()) {
            clauses.push("folder_path ILIKE ?".to_string());
            params.push(duckdb::types::Value::Text(format!("%{f}%")));
        }
        if let Some(s) = req.since {
            clauses.push("delivered >= ?::TIMESTAMP".to_string());
            params.push(duckdb::types::Value::Text(s));
        }
        if let Some(u) = req.until {
            clauses.push("delivered < ?::TIMESTAMP".to_string());
            params.push(duckdb::types::Value::Text(u));
        }
        if req.with_attachments == Some(true) {
            clauses.push("has_attachments".to_string());
        }
        let where_clause = if clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", clauses.join(" AND "))
        };
        let text = format!(
            "SELECT nid, folder_path, subject, sender_name, sender_email, delivered, size, \
                    unread, has_attachments \
             FROM messages {where_clause} ORDER BY delivered DESC NULLS LAST"
        );
        Ok(Json(self.run(&text, params, clamp_limit(req.limit))?))
    }

    /// The escape hatch: anything the structured tools cannot express.
    #[tool(description = "Run one read-only SQL statement against the mounted store. Use `store_info` for the schema. Aggregates, joins and GROUP BY all work; the file is read as the query runs.")]
    async fn sql(&self, Parameters(req): Parameters<SqlRequest>) -> Result<Json<RowsReply>, ErrorData> {
        sql::check_read_only(&req.query).map_err(bad_request)?;
        Ok(Json(self.run(&req.query, Vec::new(), clamp_limit(req.limit))?))
    }

    /// One message in full, including its body.
    #[tool(description = "Read one message by node id: headers, recipients, body text and attachment metadata.")]
    async fn get_message(
        &self,
        Parameters(req): Parameters<MessageRequest>,
    ) -> Result<Json<MessageReply>, ErrorData> {
        Ok(Json(
            message_report(&self.store, req.nid, req.max_body_chars).map_err(from_ost)?,
        ))
    }

    /// Attachment metadata for one message, without reading any payload.
    #[tool(description = "List the attachments of one message: filenames, MIME types and sizes.")]
    async fn list_attachments(
        &self,
        Parameters(req): Parameters<NidRequest>,
    ) -> Result<Json<Vec<AttachmentReply>>, ErrorData> {
        Ok(Json(
            attachment_list(&self.store, req.nid).map_err(from_ost)?,
        ))
    }

    /// One attachment's bytes, as text when they are text.
    #[tool(description = "Read one attachment's payload. Text payloads come back as text; anything else as base64.")]
    async fn read_attachment(
        &self,
        Parameters(req): Parameters<AttachmentRequest>,
    ) -> Result<Json<AttachmentDataReply>, ErrorData> {
        Ok(Json(
            attachment_data(
                &self.store,
                req.message_nid,
                req.attachment_nid,
                req.max_bytes,
            )
            .map_err(from_ost)?,
        ))
    }
}

fn attachment_reply(a: ost::Attachment) -> AttachmentReply {
    AttachmentReply {
        nid: a.nid,
        filename: a.filename,
        mime: a.mime,
        content_id: a.content_id,
        declared_size: a.declared_size,
        data_len: a.data_len,
    }
}

#[tool_handler(
    router = self.tool_router,
    name = "ost-mcp",
    instructions = "Queries a mounted Outlook OST/PST mailbox file in place. Start with `store_info` for the schema, then `search` or `sql` to find messages and `get_message` to read one. Node ids (`nid`) are the handle for everything: a message's nid comes from a query, an attachment's from `list_attachments`. Nothing is written to the mailbox and nothing is exported to disk."
)]
impl ServerHandler for OstServer {}
