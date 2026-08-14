//! Messaging layer: the folder tree, message rows, messages, and attachments.
//!
//! Nothing here is materialised up front. Folder and message listings read table
//! contexts, and a message body or attachment payload is only touched when asked
//! for by NID.

use crate::ltp::{Pc, Tc};
use crate::props::{
    clean_subject, entryid_address, filetime_to_unix_us, pid, MSGFLAG_HASATTACH, MSGFLAG_READ,
    PT_BINARY,
};
use crate::{Error, Pff, Result};
use std::collections::{HashSet, VecDeque};
use std::path::Path;

pub const NID_MESSAGE_STORE: u32 = 0x21;
pub const NID_ROOT_FOLDER: u32 = 0x122;
/// Subnode holding a message's attachment table.
pub const NID_ATTACHMENT_TABLE: u32 = 0x671;
/// Subnode holding a message's recipient table.
pub const NID_RECIPIENT_TABLE: u32 = 0x692;

pub const NID_TYPE_NORMAL_FOLDER: u32 = 0x02;
pub const NID_TYPE_HIERARCHY_TABLE: u32 = 0x0D;
pub const NID_TYPE_CONTENTS_TABLE: u32 = 0x0E;

pub fn nid_type(nid: u32) -> u32 {
    nid & 0x1F
}

pub fn nid_index(nid: u32) -> u32 {
    nid >> 5
}

/// A folder and its tables share a node index and differ only in node type.
pub fn make_nid(index: u32, ntype: u32) -> u32 {
    (index << 5) | ntype
}

#[derive(Clone, Debug)]
pub struct Folder {
    pub nid: u32,
    pub parent_nid: u32,
    pub name: String,
    /// Slash-delimited path from the store root, e.g. `/Root - Mailbox/Inbox`.
    pub path: String,
    pub item_count: Option<i32>,
    pub unread_count: Option<i32>,
    pub has_subfolders: bool,
}

/// A message as seen from its folder's contents table: cheap to list in bulk,
/// with no body and no attachment payloads.
#[derive(Clone, Debug)]
pub struct MessageRow {
    pub nid: u32,
    pub folder_nid: u32,
    pub subject: Option<String>,
    pub sender_name: Option<String>,
    pub sender_email: Option<String>,
    pub delivered_us: Option<i64>,
    pub submitted_us: Option<i64>,
    pub modified_us: Option<i64>,
    pub size: Option<i32>,
    pub unread: Option<bool>,
    pub has_attachments: Option<bool>,
    pub message_class: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Recipient {
    /// `PidTagRecipientType`: 1 = To, 2 = Cc, 3 = Bcc.
    pub kind: Option<i32>,
    pub name: Option<String>,
    pub email: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Attachment {
    pub nid: u32,
    pub filename: Option<String>,
    pub mime: Option<String>,
    pub content_id: Option<String>,
    /// `PidTagAttachSize`, which counts the whole attachment record rather than
    /// just the payload.
    pub declared_size: Option<i32>,
    /// Length of `PidTagAttachDataBinary`, or `None` when the attachment is an
    /// embedded message rather than bytes.
    pub data_len: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct Message {
    pub nid: u32,
    pub subject: Option<String>,
    pub sender_name: Option<String>,
    pub sender_email: Option<String>,
    pub display_to: Option<String>,
    pub display_cc: Option<String>,
    pub display_bcc: Option<String>,
    pub delivered_us: Option<i64>,
    pub submitted_us: Option<i64>,
    pub modified_us: Option<i64>,
    pub size: Option<i32>,
    pub unread: Option<bool>,
    pub message_class: Option<String>,
    pub internet_message_id: Option<String>,
    pub conversation_topic: Option<String>,
    pub body_plain: Option<String>,
    pub body_html: Option<String>,
    pub body_rtf: Option<String>,
    pub recipients: Vec<Recipient>,
    pub attachments: Vec<Attachment>,
}

pub struct Store {
    pub pff: Pff,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Store> {
        Ok(Store {
            pff: Pff::open(path)?,
        })
    }

    /// `PidTagDisplayName` of the message store node.
    pub fn display_name(&self) -> Option<String> {
        Pc::open(&self.pff, NID_MESSAGE_STORE)
            .ok()?
            .string(pid::DISPLAY_NAME)
    }

    /// Every folder reachable from the root, breadth-first, with paths.
    ///
    /// Names and counts come straight out of each parent's hierarchy table, so
    /// this costs one table read per folder rather than a property-context open
    /// per folder.
    pub fn folders(&self) -> Result<Vec<Folder>> {
        let root_name = Pc::open(&self.pff, NID_ROOT_FOLDER)
            .ok()
            .and_then(|pc| pc.string(pid::DISPLAY_NAME))
            .unwrap_or_else(|| "Root".to_string());

        let mut out = vec![Folder {
            nid: NID_ROOT_FOLDER,
            parent_nid: 0,
            name: root_name,
            path: String::new(),
            item_count: None,
            unread_count: None,
            has_subfolders: true,
        }];

        let mut seen = HashSet::from([NID_ROOT_FOLDER]);
        let mut queue = VecDeque::from([(NID_ROOT_FOLDER, String::new())]);

        while let Some((parent, prefix)) = queue.pop_front() {
            let hier = make_nid(nid_index(parent), NID_TYPE_HIERARCHY_TABLE);
            // Search folders and other non-folder nodes own no hierarchy table;
            // a missing one is normal, not an error.
            let Ok(tc) = Tc::open(&self.pff, hier) else {
                continue;
            };
            for row in &tc.rows {
                let nid = Tc::row_id(row);
                if nid == 0 || !seen.insert(nid) {
                    continue;
                }
                let name = tc.string(row, pid::DISPLAY_NAME).unwrap_or_default();
                let path = format!("{prefix}/{name}");
                out.push(Folder {
                    nid,
                    parent_nid: parent,
                    name,
                    path: path.clone(),
                    item_count: tc.i32(row, pid::CONTENT_COUNT),
                    unread_count: tc.i32(row, pid::CONTENT_UNREAD),
                    has_subfolders: tc.bool(row, pid::SUBFOLDERS).unwrap_or(false),
                });
                if nid_type(nid) == NID_TYPE_NORMAL_FOLDER {
                    queue.push_back((nid, path));
                }
            }
        }
        Ok(out)
    }

    /// Rows of a folder's contents table.
    ///
    /// Contents tables do not all carry the same columns, so any field here can
    /// be `None` for reasons that have nothing to do with the message.
    ///
    /// Sender identity is the awkward one. A v36 contents table has no
    /// `PidTagSenderName` column, and its `PidTagSentRepresentingName` cells do
    /// not resolve — measured against the message nodes themselves, not one cell
    /// in a 68-row sample agreed. What every row does carry is a sender EntryID,
    /// so that is what this reads; see [`entryid_address`]. An Exchange sender's
    /// EntryID holds only an X500 DN, so those rows come back with no name, and
    /// [`Store::message`] is the way to get one.
    pub fn messages(&self, folder_nid: u32) -> Result<Vec<MessageRow>> {
        let contents = make_nid(nid_index(folder_nid), NID_TYPE_CONTENTS_TABLE);
        let tc = Tc::open(&self.pff, contents)?;
        Ok(tc
            .rows
            .iter()
            .map(|row| {
                let (sender_name, sender_email) = tc
                    .bytes(row, pid::SENDER_ENTRY_ID)
                    .or_else(|| tc.bytes(row, pid::SENT_REPRESENTING_ENTRY_ID))
                    .and_then(|b| entryid_address(&b))
                    .unwrap_or((None, None));
                MessageRow {
                nid: Tc::row_id(row),
                folder_nid,
                subject: tc.string(row, pid::SUBJECT).map(|s| clean_subject(&s)),
                sender_name: tc.string(row, pid::SENDER_NAME).or(sender_name),
                sender_email: tc.string(row, pid::SENDER_EMAIL).or(sender_email),
                delivered_us: tc
                    .u64(row, pid::MESSAGE_DELIVERY_TIME)
                    .and_then(filetime_to_unix_us),
                submitted_us: tc
                    .u64(row, pid::CLIENT_SUBMIT_TIME)
                    .and_then(filetime_to_unix_us),
                modified_us: tc
                    .u64(row, pid::LAST_MODIFICATION_TIME)
                    .and_then(filetime_to_unix_us),
                size: tc.i32(row, pid::MESSAGE_SIZE),
                unread: tc
                    .i32(row, pid::MESSAGE_FLAGS)
                    .map(|f| f & MSGFLAG_READ == 0),
                has_attachments: tc.bool(row, pid::HAS_ATTACHMENTS).or_else(|| {
                    tc.i32(row, pid::MESSAGE_FLAGS)
                        .map(|f| f & MSGFLAG_HASATTACH != 0)
                }),
                message_class: tc.string(row, pid::MESSAGE_CLASS),
                }
            })
            .collect())
    }

    /// A full message: properties, bodies, recipients, and attachment metadata.
    pub fn message(&self, nid: u32) -> Result<Message> {
        let node = self.pff.node(nid)?;
        let pc = Pc::open_at(&self.pff, node.bid_data, node.bid_sub)?;
        let subs = self.pff.subnodes(node.bid_sub)?;

        let body_rtf = pc.bytes(pid::RTF_COMPRESSED).and_then(|b| {
            // Compressed RTF is its own [MS-OXRTFCP] format, unrelated to the
            // zlib framing v36 applies to whole blocks.
            compressed_rtf::decompress_rtf(&b).ok()
        });

        let mut recipients = Vec::new();
        if let Some(&(bid, bid_sub)) = subs.get(&NID_RECIPIENT_TABLE) {
            if let Ok(tc) = Tc::open_at(&self.pff, bid, bid_sub) {
                recipients = tc
                    .rows
                    .iter()
                    .map(|row| Recipient {
                        kind: tc.i32(row, pid::RECIPIENT_TYPE),
                        name: tc.string(row, pid::DISPLAY_NAME),
                        email: tc
                            .string(row, pid::SMTP_ADDRESS)
                            .or_else(|| tc.string(row, pid::EMAIL_ADDRESS)),
                    })
                    .collect();
            }
        }

        // The message node normally spells both of these out; the EntryID is the
        // fallback for the rare node that carries only that.
        let (entry_name, entry_email) = pc
            .bytes(pid::SENDER_ENTRY_ID)
            .or_else(|| pc.bytes(pid::SENT_REPRESENTING_ENTRY_ID))
            .and_then(|b| entryid_address(&b))
            .unwrap_or((None, None));

        Ok(Message {
            nid,
            subject: pc.string(pid::SUBJECT).map(|s| clean_subject(&s)),
            sender_name: pc
                .string(pid::SENDER_NAME)
                .or_else(|| pc.string(pid::SENT_REPRESENTING_NAME))
                .or(entry_name),
            sender_email: pc.string(pid::SENDER_EMAIL).or(entry_email),
            display_to: pc.string(pid::DISPLAY_TO),
            display_cc: pc.string(pid::DISPLAY_CC),
            display_bcc: pc.string(pid::DISPLAY_BCC),
            delivered_us: pc
                .u64(pid::MESSAGE_DELIVERY_TIME)
                .and_then(filetime_to_unix_us),
            submitted_us: pc
                .u64(pid::CLIENT_SUBMIT_TIME)
                .and_then(filetime_to_unix_us),
            modified_us: pc
                .u64(pid::LAST_MODIFICATION_TIME)
                .and_then(filetime_to_unix_us),
            size: pc.i32(pid::MESSAGE_SIZE),
            unread: pc.i32(pid::MESSAGE_FLAGS).map(|f| f & MSGFLAG_READ == 0),
            message_class: pc.string(pid::MESSAGE_CLASS),
            internet_message_id: pc.string(pid::INTERNET_MESSAGE_ID),
            conversation_topic: pc.string(pid::CONVERSATION_TOPIC),
            body_plain: pc.text(pid::BODY),
            body_html: pc.text(pid::HTML),
            body_rtf,
            recipients,
            attachments: self.attachments(nid)?,
        })
    }

    /// Attachment metadata for a message. Empty when the message has none.
    pub fn attachments(&self, msg_nid: u32) -> Result<Vec<Attachment>> {
        let node = self.pff.node(msg_nid)?;
        let subs = self.pff.subnodes(node.bid_sub)?;
        let Some(&(bid, bid_sub)) = subs.get(&NID_ATTACHMENT_TABLE) else {
            return Ok(Vec::new());
        };
        let tc = Tc::open_at(&self.pff, bid, bid_sub)?;
        let mut out = Vec::new();
        for row in &tc.rows {
            let nid = Tc::row_id(row);
            // The row carries only a summary; the attachment's own node holds the
            // filename, MIME type and payload.
            let detail = subs
                .get(&nid)
                .and_then(|&(b, s)| Pc::open_at(&self.pff, b, s).ok());
            let (filename, mime, content_id, data_len) = match &detail {
                Some(pc) => (
                    pc.string(pid::ATTACH_LONG_FILENAME)
                        .or_else(|| pc.string(pid::ATTACH_FILENAME)),
                    pc.string(pid::ATTACH_MIME_TAG),
                    pc.string(pid::ATTACH_CONTENT_ID),
                    // An embedded message stores PT_OBJECT here, not bytes.
                    pc.prop(pid::ATTACH_DATA_BINARY)
                        .filter(|p| p.ptype == PT_BINARY)
                        .and_then(|_| pc.bytes(pid::ATTACH_DATA_BINARY))
                        .map(|b| b.len()),
                ),
                None => (None, None, None, None),
            };
            out.push(Attachment {
                nid,
                filename: filename.or_else(|| tc.string(row, pid::ATTACH_LONG_FILENAME)),
                mime,
                content_id,
                declared_size: tc.i32(row, pid::ATTACH_SIZE),
                data_len,
            });
        }
        Ok(out)
    }

    /// Payload bytes of one attachment.
    pub fn attachment_bytes(&self, msg_nid: u32, att_nid: u32) -> Result<Vec<u8>> {
        let node = self.pff.node(msg_nid)?;
        let subs = self.pff.subnodes(node.bid_sub)?;
        let &(bid, bid_sub) = subs.get(&att_nid).ok_or_else(|| {
            Error::NotFound(format!(
                "attachment 0x{att_nid:X} is not a subnode of message 0x{msg_nid:X}"
            ))
        })?;
        let pc = Pc::open_at(&self.pff, bid, bid_sub)?;
        match pc.prop(pid::ATTACH_DATA_BINARY) {
            Some(p) if p.ptype == PT_BINARY => pc
                .bytes(pid::ATTACH_DATA_BINARY)
                .ok_or_else(|| Error::Format("attachment payload did not resolve".into())),
            Some(_) => Err(Error::Unsupported(
                "attachment is an embedded message, not a byte payload".into(),
            )),
            None => Err(Error::NotFound(
                "attachment has no PidTagAttachDataBinary".into(),
            )),
        }
    }
}
