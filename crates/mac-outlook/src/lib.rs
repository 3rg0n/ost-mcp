//! Read-only reader for Mac Outlook's local store: `Outlook.sqlite` plus
//! `.olk15*` companion files.
//!
//! ```text
//! model  <-->  ost-mcp (MCP stdio + DuckDB)  <-->  Outlook.sqlite + .olk15*
//! ```
//!
//! Every claim about the on-disk format is measurement-backed in
//! `docs/mac-outlook-format.md`; read that first. The short version: this
//! reads real data when an account's classic engine is populated (confirmed
//! by independent third-party tools against other profiles — see the
//! `olk15` module docs for credit), and returns empty results rather than a
//! guess when it is not, which is the normal state for an Exchange/M365
//! account under "New Outlook" (§2/§3.1 of that doc).
//!
//! **Known limitation:** `message()` only resolves a body from
//! `.olk15Message` (the 100%-of-viewed-messages cache). `.olk15MsgSource`
//! files (full RFC822 MIME, higher fidelity but present for a minority of
//! messages) are located but not parsed here — that needs a real MIME
//! parser and a populated profile to validate against, neither of which
//! this change has. See `CHANGELOG.md`.

pub mod discover;
pub mod olk15;
mod schema;

use std::path::PathBuf;
use std::sync::Mutex;

use mailbox::{Error, Mailbox, Result};
use rusqlite::Connection;

pub struct Profile {
    data_dir: PathBuf,
    conn: Mutex<Connection>,
}

impl Profile {
    pub fn open(data_dir: PathBuf) -> Result<Profile> {
        let conn = schema::open_readonly(&data_dir.join("Outlook.sqlite"))?;
        Ok(Profile {
            data_dir,
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Read one `.olk15*` file relative to `Data/`, decoding the
    /// URL-encoded path segments a `Blocks.PathToDataFile` value carries
    /// (`docs/mac-outlook-format.md` §3.2).
    ///
    /// `relative` comes straight out of `Outlook.sqlite` — a `PathToDataFile`
    /// column, not something this reader controls. A `..` component or an
    /// absolute path is rejected rather than joined: `PathBuf::join` does not
    /// sanitize either (an absolute argument replaces the base outright), so
    /// without this check a crafted or corrupted database row would let any
    /// tool that surfaces a message body or attachment read an arbitrary file
    /// the process has access to, not just the mailbox.
    fn read_data_file(&self, relative: &str) -> Result<Vec<u8>> {
        let decoded = percent_decode(relative);
        let rel_path = std::path::Path::new(&decoded);
        let escapes = rel_path.is_absolute()
            || rel_path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir));
        if escapes {
            return Err(Error::Format(format!(
                "refusing to read outside the profile data directory: {relative:?}"
            )));
        }
        let path = self.data_dir.join(rel_path);
        std::fs::read(&path).map_err(Error::Io)
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3]) {
                if let Ok(v) = u8::from_str_radix(hex, 16) {
                    out.push(v);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl Mailbox for Profile {
    fn kind(&self) -> &'static str {
        "mac-olk15"
    }

    fn display_name(&self) -> Option<String> {
        schema::display_name(&self.lock())
    }

    fn folders(&self) -> Result<Vec<mailbox::Folder>> {
        schema::folders(&self.lock())
    }

    fn messages(&self, folder_id: i64) -> Result<Vec<mailbox::MessageRow>> {
        schema::messages(&self.lock(), folder_id)
    }

    fn message(&self, id: i64) -> Result<mailbox::Message> {
        let (row, path_to_data_file, conversation_topic) = schema::mail_row(&self.lock(), id)?;

        let mut body_plain = None;
        let mut body_html = None;
        let mut body_rtf = None;
        if let Some(rel) = path_to_data_file {
            if let Ok(data) = self.read_data_file(&rel) {
                if let Some(body) = olk15::parse_message(&data) {
                    match body.kind {
                        olk15::BodyKind::Html => body_html = Some(body.text),
                        olk15::BodyKind::Rtf => body_rtf = Some(body.text),
                        // A calendar body and a plain-text fallback both
                        // read fine as plain text; there is no richer slot
                        // for either in `mailbox::Message`.
                        olk15::BodyKind::Calendar | olk15::BodyKind::Plain => {
                            body_plain = Some(body.text)
                        }
                    }
                }
            }
        }

        Ok(mailbox::Message {
            id: row.id,
            subject: row.subject,
            sender_name: row.sender_name,
            sender_email: row.sender_email,
            // Not populated: `Mail` carries flat recipient-address-list
            // strings, not a structured recipient table, and the list
            // delimiter/encoding has not been measured against a real row
            // (docs/mac-outlook-format.md §3.3). Guessing one would be
            // exactly the fabricated-but-plausible value CONTRIBUTING.md
            // rules out.
            display_to: None,
            display_cc: None,
            display_bcc: None,
            delivered_us: row.delivered_us,
            submitted_us: row.submitted_us,
            modified_us: row.modified_us,
            size: row.size,
            unread: row.unread,
            message_class: row.message_class,
            internet_message_id: None,
            conversation_topic,
            body_plain,
            body_html,
            body_rtf,
            recipients: Vec::new(),
            attachments: self.attachments(id)?,
        })
    }

    fn attachments(&self, message_id: i64) -> Result<Vec<mailbox::Attachment>> {
        let blocks = schema::linked_blocks(&self.lock(), message_id, "Message Attachments")?;
        Ok(blocks
            .into_iter()
            .map(|b| {
                let (filename, mime) = self
                    .read_data_file(&b.path_to_data_file)
                    .ok()
                    .and_then(|data| olk15::attachment_metadata(&data).ok())
                    .unwrap_or((None, None));
                mailbox::Attachment {
                    id: b.id,
                    filename,
                    mime,
                    content_id: None,
                    declared_size: None,
                    data_len: None,
                }
            })
            .collect())
    }

    fn attachment_bytes(&self, message_id: i64, attachment_id: i64) -> Result<Vec<u8>> {
        let blocks = schema::linked_blocks(&self.lock(), message_id, "Message Attachments")?;
        let block = blocks
            .into_iter()
            .find(|b| b.id == attachment_id)
            .ok_or_else(|| {
                Error::NotFound(format!(
                    "attachment {attachment_id} is not linked to message {message_id}"
                ))
            })?;
        let data = self.read_data_file(&block.path_to_data_file)?;
        Ok(olk15::parse_attachment(&data)?.bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decode_handles_spaces_and_plain_text() {
        assert_eq!(
            percent_decode("Message%20Attachments/35/x.olk15MsgAttachment"),
            "Message Attachments/35/x.olk15MsgAttachment"
        );
        assert_eq!(percent_decode("Categories/1/a.olk15Category"), "Categories/1/a.olk15Category");
    }

    /// An empty but valid SQLite file, so `Profile::open`'s read-only open
    /// succeeds without needing the real schema — these tests exercise
    /// `read_data_file` directly, not anything SQL-shaped.
    fn empty_profile(dir: &std::path::Path) -> Profile {
        rusqlite::Connection::open(dir.join("Outlook.sqlite")).unwrap();
        Profile::open(dir.to_path_buf()).unwrap()
    }

    #[test]
    fn read_data_file_rejects_parent_dir_traversal() {
        let dir = std::env::temp_dir().join(format!("mac-outlook-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let secret = dir.parent().unwrap().join("mac-outlook-test-secret.txt");
        std::fs::write(&secret, b"do not read me").unwrap();

        let profile = empty_profile(&dir);
        let err = profile
            .read_data_file("../mac-outlook-test-secret.txt")
            .unwrap_err();
        assert!(matches!(err, Error::Format(_)));

        std::fs::remove_file(&secret).ok();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_data_file_rejects_absolute_path() {
        let dir = std::env::temp_dir().join(format!("mac-outlook-test-abs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let profile = empty_profile(&dir);
        let err = profile.read_data_file("/etc/passwd").unwrap_err();
        assert!(matches!(err, Error::Format(_)));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_data_file_allows_a_normal_relative_path() {
        let dir = std::env::temp_dir().join(format!("mac-outlook-test-ok-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Categories/1")).unwrap();
        std::fs::write(dir.join("Categories/1/a.olk15Category"), b"hi").unwrap();

        let profile = empty_profile(&dir);
        let data = profile.read_data_file("Categories/1/a.olk15Category").unwrap();
        assert_eq!(data, b"hi");

        std::fs::remove_dir_all(&dir).ok();
    }
}
