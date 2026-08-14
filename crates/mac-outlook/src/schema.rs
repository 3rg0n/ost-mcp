//! `Outlook.sqlite` queries: folders, mail rows, and the `Blocks` /
//! `Mail_OwnedBlocks` join that links a message to its source and
//! attachment files.
//!
//! Schema and every row value here is as measured in
//! `docs/mac-outlook-format.md` §3.1 — column names, the `PathToDataFile`
//! mechanism, and the `Placeholder_*` sentinel are all confirmed against a
//! real (if content-empty) profile. What is *not* measured against a real
//! `Mail` row — because none exists on the machine this was written against
//! — is flagged inline; see §3.3 and the module doc on [`crate::olk15`].

use rusqlite::{Connection, OpenFlags};
use std::collections::HashMap;

use mailbox::{Error, Result};

/// Open `Outlook.sqlite` read-only. Never creates the file, never opens
/// read-write — measured safe to do this while Outlook holds the file open
/// (`docs/mac-outlook-format.md` U1: both `mode=ro` and `mode=ro&immutable=1`
/// succeeded against the live database).
pub fn open_readonly(path: &std::path::Path) -> Result<Connection> {
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))
}

fn sql_err(e: rusqlite::Error) -> Error {
    Error::Format(format!("sqlite: {e}"))
}

/// `Account_Name` of whichever account row exists first — mail, then
/// Exchange. Neither table had a row on the measurement machine, so this
/// path is unverified against a real value.
pub fn display_name(conn: &Connection) -> Option<String> {
    for table in ["AccountsMail", "AccountsExchange"] {
        let sql = format!("SELECT Account_Name FROM {table} WHERE Account_Name IS NOT NULL LIMIT 1");
        if let Ok(name) = conn.query_row(&sql, [], |r| r.get::<_, String>(0)) {
            if !name.is_empty() {
                return Some(name);
            }
        }
    }
    None
}

/// The literal placeholder text Outlook itself writes into `Folder_Name`
/// when a folder is scaffolding rather than a synced folder — measured
/// verbatim (`docs/mac-outlook-format.md` §3.1): every one of 14 rows on the
/// measurement account read `Placeholder_<Something>_Placeholder`.
fn is_placeholder_name(name: &str) -> bool {
    name.starts_with("Placeholder_") && name.ends_with("_Placeholder")
}

/// `Folder_SpecialFolderType` → canonical name. The full measured
/// correspondence — each code cross-checked against that row's own fake but
/// consistently coded placeholder label — is published in
/// `docs/mac-outlook-format.md` §3.1; this is not the standard MAPI
/// `OlDefaultFolders` enum applied from memory. Code `0` covered two
/// different placeholder labels in the sample (`Saved Messages` and `Auto
/// Saved Messages`) and is deliberately left unmapped rather than guessed.
fn special_folder_name(code: i64) -> Option<&'static str> {
    match code {
        1 => Some("Inbox"),
        2 => Some("Outbox"),
        3 => Some("Address Book"),
        4 => Some("Calendar"),
        5 => Some("Notes"),
        6 => Some("Tasks"),
        8 => Some("Sent Items"),
        9 => Some("Deleted Items"),
        10 => Some("Drafts"),
        12 => Some("Junk Email"),
        99 => Some("On My Computer"),
        103 => Some("Temporary Items"),
        _ => None,
    }
}

struct RawFolder {
    id: i64,
    parent_id: Option<i64>,
    name: Option<String>,
}

pub fn folders(conn: &Connection) -> Result<Vec<mailbox::Folder>> {
    let mut stmt = conn
        .prepare(
            "SELECT Record_RecordID, Folder_ParentID, Folder_Name, Folder_SpecialFolderType \
             FROM Folders",
        )
        .map_err(sql_err)?;
    let rows = stmt
        .query_map([], |r| {
            let id: i64 = r.get(0)?;
            let parent_raw: i64 = r.get(1)?;
            let raw_name: String = r.get(2)?;
            let special: i64 = r.get(3)?;
            let name = special_folder_name(special).map(str::to_string).or_else(|| {
                if is_placeholder_name(&raw_name) {
                    None
                } else {
                    Some(raw_name)
                }
            });
            Ok(RawFolder {
                id,
                parent_id: (parent_raw > 0).then_some(parent_raw),
                name,
            })
        })
        .map_err(sql_err)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sql_err)?;

    let counts = message_counts(conn)?;
    let by_id: HashMap<i64, &RawFolder> = rows.iter().map(|f| (f.id, f)).collect();

    let path_of = |mut id: i64| -> String {
        let mut segments = Vec::new();
        let mut guard = 0;
        loop {
            guard += 1;
            if guard > 64 {
                break; // cycle guard; a real profile should never loop this deep
            }
            let Some(f) = by_id.get(&id) else { break };
            segments.push(f.name.clone().unwrap_or_else(|| format!("folder-{}", f.id)));
            match f.parent_id {
                Some(p) => id = p,
                None => break,
            }
        }
        segments.reverse();
        format!("/{}", segments.join("/"))
    };

    Ok(rows
        .iter()
        .map(|f| {
            let (item_count, unread_count) = counts.get(&f.id).copied().unwrap_or((0, 0));
            mailbox::Folder {
                id: f.id,
                parent_id: f.parent_id,
                name: f.name.clone(),
                path: path_of(f.id),
                item_count: Some(item_count),
                unread_count: Some(unread_count),
                has_subfolders: rows.iter().any(|c| c.parent_id == Some(f.id)),
                // Not measured: no account on the measurement machine has a
                // search-folder equivalent to confirm against.
                is_search_folder: false,
            }
        })
        .collect())
}

/// `(item_count, unread_count)` per folder, from a real live count against
/// `Mail` — honest even when it comes back all zero, unlike `Folder_Name`.
fn message_counts(conn: &Connection) -> Result<HashMap<i64, (i64, i64)>> {
    let mut stmt = conn
        .prepare(
            "SELECT Record_FolderID, count(*), \
                    sum(CASE WHEN Message_ReadFlag = 0 THEN 1 ELSE 0 END) \
             FROM Mail GROUP BY Record_FolderID",
        )
        .map_err(sql_err)?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Option<i64>>(2)?.unwrap_or(0),
            ))
        })
        .map_err(sql_err)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sql_err)?;
    Ok(rows.into_iter().map(|(f, total, unread)| (f, (total, unread))).collect())
}

/// A `Unix seconds` column read defensively: SQLite's dynamic typing means a
/// `DATETIME`-affinity column can come back as an integer, a float, or text
/// depending on what wrote it. Only the epoch itself (seconds since 1970) is
/// measured (`docs/mac-outlook-format.md` §3.3, against `Record_ModDate`);
/// applying it to `Message_Time*` is an inference pending a real row.
fn read_epoch_seconds(row: &rusqlite::Row, idx: usize) -> Option<i64> {
    match row.get_ref(idx).ok()? {
        rusqlite::types::ValueRef::Integer(i) => Some(i),
        rusqlite::types::ValueRef::Real(f) => Some(f as i64),
        rusqlite::types::ValueRef::Text(t) => std::str::from_utf8(t).ok()?.parse::<f64>().ok().map(|f| f as i64),
        _ => None,
    }
}

struct RawMail {
    row: mailbox::MessageRow,
    path_to_data_file: Option<String>,
    conversation_topic: Option<String>,
}

const MAIL_COLUMNS: &str = "Record_RecordID, Record_FolderID, Message_NormalizedSubject, \
     Message_SenderList, Message_SenderAddressList, Message_TimeReceived, Message_TimeSent, \
     Record_ModDate, Message_Size, Message_ReadFlag, Message_HasAttachment, PathToDataFile, \
     Message_ThreadTopic";

fn row_to_mail(r: &rusqlite::Row) -> rusqlite::Result<RawMail> {
    let sender_name: Option<String> = r.get(3)?;
    let sender_email: Option<String> = r.get(4)?;
    let conversation_topic: Option<String> = r.get(12)?;
    Ok(RawMail {
        row: mailbox::MessageRow {
            id: r.get(0)?,
            folder_id: r.get(1)?,
            subject: r.get(2)?,
            sender_name: sender_name.filter(|s| !s.is_empty()),
            sender_email: sender_email.filter(|s| !s.is_empty()),
            delivered_us: read_epoch_seconds(r, 5).map(|s| s * 1_000_000),
            submitted_us: read_epoch_seconds(r, 6).map(|s| s * 1_000_000),
            modified_us: read_epoch_seconds(r, 7).map(|s| s * 1_000_000),
            size: r.get(8)?,
            unread: r.get::<_, Option<i64>>(9)?.map(|f| f == 0),
            has_attachments: r.get::<_, Option<i64>>(10)?.map(|f| f != 0),
            // No column in this schema maps to a MAPI-style message class;
            // `Message_type` is an integer whose code table is unmeasured.
            message_class: None,
        },
        path_to_data_file: r.get(11)?,
        conversation_topic: conversation_topic.filter(|s| !s.is_empty()),
    })
}

pub fn messages(conn: &Connection, folder_id: i64) -> Result<Vec<mailbox::MessageRow>> {
    let sql = format!("SELECT {MAIL_COLUMNS} FROM Mail WHERE Record_FolderID = ?");
    let mut stmt = conn.prepare(&sql).map_err(sql_err)?;
    let rows = stmt
        .query_map([folder_id], |r| row_to_mail(r).map(|m| m.row))
        .map_err(sql_err)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sql_err)?;
    Ok(rows)
}

/// `(row, olk15Message path, conversation topic)` for one mail record.
pub fn mail_row(conn: &Connection, id: i64) -> Result<(mailbox::MessageRow, Option<String>, Option<String>)> {
    let sql = format!("SELECT {MAIL_COLUMNS} FROM Mail WHERE Record_RecordID = ?");
    conn.query_row(&sql, [id], row_to_mail)
        .map(|m| (m.row, m.path_to_data_file, m.conversation_topic))
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Error::NotFound(format!("mail record {id}")),
            other => sql_err(other),
        })
}

/// FNV-1a 64-bit over a `Blocks.BlockID` blob, folded into a positive `i64`.
/// `BlockID` is a `BLOB` primary key, not an integer, so this gives it a
/// stable surrogate id for the `Mailbox` trait without needing a second
/// lookup table.
fn block_id_hash(bytes: &[u8]) -> i64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    (hash & 0x7fff_ffff_ffff_ffff) as i64
}

/// One block linked to a mail record via `Mail_OwnedBlocks`, with a derived
/// `i64` id standing in for its `BLOB` primary key.
pub struct LinkedBlock {
    pub id: i64,
    pub path_to_data_file: String,
}

/// `Message Sources`, `Message Attachments` or anything else `Blocks` names
/// for this record, distinguished by substring on `PathToDataFile` — the
/// only `BlockTag` value found in any measured or credited source is the
/// attachment one, and it was not independently re-verified, so this does
/// not rely on it.
pub fn linked_blocks(conn: &Connection, mail_id: i64, path_contains: &str) -> Result<Vec<LinkedBlock>> {
    let mut stmt = conn
        .prepare(
            "SELECT b.BlockID, b.PathToDataFile FROM Blocks b \
             JOIN Mail_OwnedBlocks m ON m.BlockID = b.BlockID \
             WHERE m.Record_RecordID = ?",
        )
        .map_err(sql_err)?;
    let rows = stmt
        .query_map([mail_id], |r| {
            Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(sql_err)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sql_err)?;
    Ok(rows
        .into_iter()
        .filter(|(_, path)| path.contains(path_contains))
        .map(|(blockid, path)| LinkedBlock {
            id: block_id_hash(&blockid),
            path_to_data_file: path,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal schema covering only the columns these queries touch,
    /// with invented data — no real mailbox content, per `CONTRIBUTING.md`.
    fn fixture_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE Folders (
                Record_RecordID INTEGER PRIMARY KEY,
                Folder_ParentID INTEGER,
                Folder_Name TEXT,
                Folder_SpecialFolderType INTEGER
             );
             CREATE TABLE Mail (
                Record_RecordID INTEGER PRIMARY KEY,
                Record_FolderID INTEGER,
                Message_NormalizedSubject TEXT,
                Message_SenderList TEXT,
                Message_SenderAddressList TEXT,
                Message_TimeReceived INTEGER,
                Message_TimeSent INTEGER,
                Record_ModDate INTEGER,
                Message_Size INTEGER,
                Message_ReadFlag INTEGER,
                Message_HasAttachment INTEGER,
                PathToDataFile TEXT,
                Message_ThreadTopic TEXT
             );
             CREATE TABLE Blocks (BlockID BLOB PRIMARY KEY, PathToDataFile TEXT);
             CREATE TABLE Mail_OwnedBlocks (Record_RecordID INTEGER, BlockID BLOB);
             CREATE TABLE AccountsMail (Account_Name TEXT);
             CREATE TABLE AccountsExchange (Account_Name TEXT);

             INSERT INTO Folders VALUES (1, -2, 'Placeholder_Inbox_Placeholder', 1);
             INSERT INTO Folders VALUES (2, -2, 'Team Project', 0);
             INSERT INTO Folders VALUES (3, 2, 'Subfolder', 0);

             INSERT INTO Mail VALUES (10, 2, 'Re: example', 'Sample Sender', 'sender@example.com',
                1700000000, 1700000000, 1700000000, 1024, 0, 1, 'Messages/1/example.olk15Message',
                'Example thread');
             ",
        )
        .unwrap();
        conn
    }

    #[test]
    fn placeholder_folder_name_is_nulled_but_special_type_gets_a_real_name() {
        let conn = fixture_db();
        let folders = folders(&conn).unwrap();
        let inbox = folders.iter().find(|f| f.id == 1).unwrap();
        assert_eq!(inbox.name, Some("Inbox".to_string()));

        let custom = folders.iter().find(|f| f.id == 2).unwrap();
        assert_eq!(custom.name, Some("Team Project".to_string()));
        assert!(custom.has_subfolders);
        assert_eq!(custom.path, "/Team Project");

        let sub = folders.iter().find(|f| f.id == 3).unwrap();
        assert_eq!(sub.path, "/Team Project/Subfolder");
        assert_eq!(sub.parent_id, Some(2));
    }

    #[test]
    fn message_counts_are_real_not_placeholder() {
        let conn = fixture_db();
        let folders = folders(&conn).unwrap();
        let custom = folders.iter().find(|f| f.id == 2).unwrap();
        assert_eq!(custom.item_count, Some(1));
        assert_eq!(custom.unread_count, Some(1));
        let empty = folders.iter().find(|f| f.id == 1).unwrap();
        assert_eq!(empty.item_count, Some(0));
    }

    #[test]
    fn mail_row_reads_back_by_id() {
        let conn = fixture_db();
        let (row, path, topic) = mail_row(&conn, 10).unwrap();
        assert_eq!(row.subject.as_deref(), Some("Re: example"));
        assert_eq!(row.sender_email.as_deref(), Some("sender@example.com"));
        assert_eq!(row.unread, Some(true));
        assert_eq!(row.has_attachments, Some(true));
        assert_eq!(path.as_deref(), Some("Messages/1/example.olk15Message"));
        assert_eq!(topic.as_deref(), Some("Example thread"));
    }

    #[test]
    fn missing_mail_row_is_not_found() {
        let conn = fixture_db();
        assert!(matches!(mail_row(&conn, 999), Err(Error::NotFound(_))));
    }

    #[test]
    fn linked_blocks_filters_by_path_substring() {
        let conn = fixture_db();
        conn.execute(
            "INSERT INTO Blocks VALUES (X'AABBCC', 'Message Attachments/9/x.olk15MsgAttachment')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO Blocks VALUES (X'DDEEFF', 'Message Sources/9/y.olk15MsgSource')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO Mail_OwnedBlocks VALUES (10, X'AABBCC'), (10, X'DDEEFF')",
            [],
        )
        .unwrap();

        let attachments = linked_blocks(&conn, 10, "Message Attachments").unwrap();
        assert_eq!(attachments.len(), 1);
        assert!(attachments[0].path_to_data_file.contains("Attachments"));

        let sources = linked_blocks(&conn, 10, "Message Sources").unwrap();
        assert_eq!(sources.len(), 1);
        assert!(sources[0].path_to_data_file.contains("Sources"));
    }

    #[test]
    fn display_name_reads_first_populated_account_table() {
        let conn = fixture_db();
        assert_eq!(display_name(&conn), None);
        conn.execute("INSERT INTO AccountsExchange VALUES ('Example Account')", [])
            .unwrap();
        assert_eq!(display_name(&conn), Some("Example Account".to_string()));
    }
}
