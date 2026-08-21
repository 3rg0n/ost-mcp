//! `impl Mailbox for ost::Store` — the thin adapter `docs/adr/0001-mailbox-backend-trait.md`
//! calls for. `ost::Store` itself is untouched; only ids get widened here, at
//! the boundary, from the OST reader's native `u32` NID to the trait's `i64`.

use mailbox::{Attachment, Error, Folder, Mailbox, Message, MessageRow, Recipient, Result};

pub struct OstMailbox(pub ost::Store);

fn conv_err(e: ost::Error) -> Error {
    match e {
        ost::Error::Io(io) => Error::Io(io),
        ost::Error::Format(m) => Error::Format(m),
        ost::Error::Unsupported(m) => Error::Unsupported(m),
        ost::Error::NotFound(m) => Error::NotFound(m),
    }
}

fn conv_attachment(a: ost::Attachment) -> Attachment {
    Attachment {
        id: a.nid as i64,
        filename: a.filename,
        mime: a.mime,
        content_id: a.content_id,
        declared_size: a.declared_size.map(i64::from),
        data_len: a.data_len,
    }
}

impl Mailbox for OstMailbox {
    fn kind(&self) -> &'static str {
        match self.0.pff.ver {
            36 => "ost-v36",
            23 => "ost-v23",
            _ => "ost-other",
        }
    }

    fn display_name(&self) -> Option<String> {
        self.0.display_name()
    }

    fn folders(&self) -> Result<Vec<Folder>> {
        Ok(self
            .0
            .folders()
            .map_err(conv_err)?
            .into_iter()
            .map(|f| Folder {
                id: f.nid as i64,
                parent_id: (f.parent_nid != 0).then_some(f.parent_nid as i64),
                name: Some(f.name),
                path: f.path,
                item_count: f.item_count.map(i64::from),
                unread_count: f.unread_count.map(i64::from),
                has_subfolders: f.has_subfolders,
                is_search_folder: ost::store::nid_type(f.nid) != ost::store::NID_TYPE_NORMAL_FOLDER,
            })
            .collect())
    }

    fn messages(&self, folder_id: i64) -> Result<Vec<MessageRow>> {
        Ok(self
            .0
            .messages(folder_id as u32)
            .map_err(conv_err)?
            .into_iter()
            .map(|m| MessageRow {
                id: m.nid as i64,
                folder_id: m.folder_nid as i64,
                subject: m.subject,
                sender_name: m.sender_name,
                sender_email: m.sender_email,
                delivered_us: m.delivered_us,
                submitted_us: m.submitted_us,
                modified_us: m.modified_us,
                size: m.size.map(i64::from),
                unread: m.unread,
                has_attachments: m.has_attachments,
                message_class: m.message_class,
            })
            .collect())
    }

    fn message(&self, id: i64) -> Result<Message> {
        let m = self.0.message(id as u32).map_err(conv_err)?;
        Ok(Message {
            id: m.nid as i64,
            subject: m.subject,
            sender_name: m.sender_name,
            sender_email: m.sender_email,
            display_to: m.display_to,
            display_cc: m.display_cc,
            display_bcc: m.display_bcc,
            delivered_us: m.delivered_us,
            submitted_us: m.submitted_us,
            modified_us: m.modified_us,
            size: m.size.map(i64::from),
            unread: m.unread,
            message_class: m.message_class,
            internet_message_id: m.internet_message_id,
            conversation_topic: m.conversation_topic,
            body_plain: m.body_plain,
            body_html: m.body_html,
            body_rtf: m.body_rtf,
            recipients: m
                .recipients
                .into_iter()
                .map(|r| Recipient {
                    kind: r.kind,
                    name: r.name,
                    email: r.email,
                })
                .collect(),
            attachments: m.attachments.into_iter().map(conv_attachment).collect(),
        })
    }

    fn attachments(&self, message_id: i64) -> Result<Vec<Attachment>> {
        Ok(self
            .0
            .attachments(message_id as u32)
            .map_err(conv_err)?
            .into_iter()
            .map(conv_attachment)
            .collect())
    }

    fn attachment_bytes(&self, message_id: i64, attachment_id: i64) -> Result<Vec<u8>> {
        self.0
            .attachment_bytes(message_id as u32, attachment_id as u32)
            .map_err(conv_err)
    }
}
