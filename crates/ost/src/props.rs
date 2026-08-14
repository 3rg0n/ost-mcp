//! Property identifiers, property types, and value decoding helpers.

/// Property types (MS-OXCDATA 2.11.1).
pub const PT_I16: u16 = 0x0002;
pub const PT_I32: u16 = 0x0003;
pub const PT_F32: u16 = 0x0004;
pub const PT_F64: u16 = 0x0005;
pub const PT_BOOL: u16 = 0x000B;
pub const PT_OBJECT: u16 = 0x000D;
pub const PT_I64: u16 = 0x0014;
pub const PT_STRING8: u16 = 0x001E;
pub const PT_UNICODE: u16 = 0x001F;
pub const PT_SYSTIME: u16 = 0x0040;
pub const PT_GUID: u16 = 0x0048;
pub const PT_BINARY: u16 = 0x0102;

/// Property identifiers used by this reader. Everything here is below 0x8000, so
/// none of it needs the named-property map.
pub mod pid {
    pub const MESSAGE_CLASS: u16 = 0x001A;
    pub const IMPORTANCE: u16 = 0x0017;
    pub const CLIENT_SUBMIT_TIME: u16 = 0x0039;
    pub const SUBJECT: u16 = 0x0037;
    pub const CONVERSATION_TOPIC: u16 = 0x0070;
    /// `PidTagSentRepresentingName`. Contents tables carry this instead of
    /// [`SENDER_NAME`], which appears only on the message itself.
    pub const SENT_REPRESENTING_NAME: u16 = 0x0042;
    pub const SENDER_NAME: u16 = 0x0C1A;
    pub const SENDER_EMAIL: u16 = 0x0C1F;
    /// `PidTagSenderEntryId`. A contents table carries this for every row, and it
    /// embeds the sender's display name and address; see [`super::entryid_address`].
    pub const SENDER_ENTRY_ID: u16 = 0x0C19;
    /// `PidTagSentRepresentingEntryId`, the on-behalf-of counterpart.
    pub const SENT_REPRESENTING_ENTRY_ID: u16 = 0x0041;
    pub const RECIPIENT_TYPE: u16 = 0x0C15;
    pub const DISPLAY_BCC: u16 = 0x0E02;
    pub const DISPLAY_CC: u16 = 0x0E03;
    pub const DISPLAY_TO: u16 = 0x0E04;
    pub const MESSAGE_DELIVERY_TIME: u16 = 0x0E06;
    pub const MESSAGE_FLAGS: u16 = 0x0E07;
    pub const MESSAGE_SIZE: u16 = 0x0E08;
    pub const HAS_ATTACHMENTS: u16 = 0x0E1B;
    pub const ATTACH_SIZE: u16 = 0x0E20;
    pub const BODY: u16 = 0x1000;
    pub const RTF_COMPRESSED: u16 = 0x1009;
    pub const HTML: u16 = 0x1013;
    pub const INTERNET_MESSAGE_ID: u16 = 0x1035;
    pub const DISPLAY_NAME: u16 = 0x3001;
    pub const EMAIL_ADDRESS: u16 = 0x3003;
    pub const LAST_MODIFICATION_TIME: u16 = 0x3008;
    pub const CONTENT_COUNT: u16 = 0x3602;
    pub const CONTENT_UNREAD: u16 = 0x3603;
    pub const SUBFOLDERS: u16 = 0x360A;
    pub const ATTACH_DATA_BINARY: u16 = 0x3701;
    pub const ATTACH_FILENAME: u16 = 0x3704;
    pub const ATTACH_METHOD: u16 = 0x3705;
    pub const ATTACH_LONG_FILENAME: u16 = 0x3707;
    pub const ATTACH_MIME_TAG: u16 = 0x370E;
    pub const ATTACH_CONTENT_ID: u16 = 0x3712;
    pub const SMTP_ADDRESS: u16 = 0x39FE;
}

/// `PidTagMessageFlags` bit 0: the message has been read.
pub const MSGFLAG_READ: i32 = 0x01;
/// `PidTagMessageFlags` bit 4: the message has at least one attachment. This is
/// the only place a contents table records it — `PidTagHasAttachments` is not
/// one of its columns.
pub const MSGFLAG_HASATTACH: i32 = 0x10;

pub fn utf16le(b: &[u8]) -> String {
    let u: Vec<u16> = b
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&u)
}

/// Decode a string property according to its property type.
pub fn decode_string(ptype: u16, b: &[u8]) -> String {
    match ptype {
        PT_UNICODE => utf16le(b),
        _ => String::from_utf8_lossy(b).into_owned(),
    }
}

/// `muidOOP`, the provider UID of a One-Off EntryID (MS-OXCDATA 2.2.5.1). This
/// is the form an internet sender takes, and the only form that spells out an
/// address; an Exchange sender uses `muidEMSAB`, which carries an X500 DN with no
/// display name in it.
const MUID_OOP: [u8; 16] = [
    0x81, 0x2B, 0x1F, 0xA4, 0xBE, 0xA3, 0x10, 0x19, 0x9D, 0x6E, 0x00, 0xDD, 0x01, 0x0F, 0x54, 0x02,
];

/// Display name and SMTP address out of an EntryID such as
/// `PidTagSenderEntryId`.
///
/// This exists because a contents table has no usable sender-name column: the
/// `PidTagSentRepresentingName` cells of a v36 contents table do not hold
/// resolvable HNIDs, but every row does carry a sender EntryID, so the name and
/// address are recoverable in bulk without opening each message.
///
/// Returns `None` for anything that is not a One-Off EntryID, and an address only
/// when the EntryID declares its type as SMTP.
pub fn entryid_address(b: &[u8]) -> Option<(Option<String>, Option<String>)> {
    if b.len() < 24 || b[4..20] != MUID_OOP {
        return None;
    }
    // The low bit of the flags word says whether the three strings that follow
    // are UTF-16 or 8-bit; MAPI_UNICODE is 0x8000.
    let unicode = u16::from_le_bytes([b[22], b[23]]) & 0x8000 != 0;
    let mut at = 24;
    let mut next = || {
        let s = string_z(b, at, unicode)?;
        at = s.1;
        Some(s.0)
    };
    let (name, kind, addr) = (next()?, next()?, next()?);
    let email = (kind.eq_ignore_ascii_case("SMTP") && addr.contains('@')).then_some(addr);
    Some((Some(name).filter(|s| !s.is_empty()), email))
}

/// Read a null-terminated string starting at `at`, returning it with the offset
/// just past its terminator.
fn string_z(b: &[u8], at: usize, unicode: bool) -> Option<(String, usize)> {
    if unicode {
        let units: Vec<u16> = b[at..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|&u| u != 0)
            .collect();
        let end = at + units.len() * 2 + 2;
        (end <= b.len()).then(|| (String::from_utf16_lossy(&units), end))
    } else {
        let len = b[at..].iter().position(|&c| c == 0)?;
        Some((
            String::from_utf8_lossy(&b[at..at + len]).into_owned(),
            at + len + 1,
        ))
    }
}

/// `PidTagSubject` may carry a prefix escape: U+0001, then a byte giving the
/// length of the prefix ("RE: ", "FW: ") that follows it (MS-OXCMSG 2.2.1.5).
pub fn clean_subject(s: &str) -> String {
    let mut it = s.chars();
    if it.next() == Some('\u{1}') {
        it.next();
        it.collect()
    } else {
        s.to_string()
    }
}

/// Windows FILETIME (100 ns ticks since 1601-01-01) to microseconds since the
/// Unix epoch. Zero means "unset", not 1601.
pub fn filetime_to_unix_us(ft: u64) -> Option<i64> {
    if ft == 0 {
        return None;
    }
    Some((ft / 10) as i64 - 11_644_473_600_000_000)
}

/// `YYYY-MM-DD HH:MM:SS` for microseconds since the Unix epoch, via Howard
/// Hinnant's `civil_from_days`.
pub fn format_time_us(us: i64) -> String {
    let secs = us.div_euclid(1_000_000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        y,
        m,
        d,
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filetime_epoch_boundary() {
        // 1970-01-01T00:00:00Z expressed as FILETIME.
        assert_eq!(filetime_to_unix_us(116_444_736_000_000_000), Some(0));
        assert_eq!(filetime_to_unix_us(0), None);
    }

    #[test]
    fn formats_a_known_instant() {
        assert_eq!(format_time_us(0), "1970-01-01 00:00:00");
        assert_eq!(format_time_us(1_700_000_000_000_000), "2023-11-14 22:13:20");
    }

    /// A One-Off EntryID as it appears in a contents table row.
    fn one_off(name: &str, kind: &str, addr: &str) -> Vec<u8> {
        let mut b = vec![0u8; 4];
        b.extend_from_slice(&MUID_OOP);
        b.extend_from_slice(&0u16.to_le_bytes());
        b.extend_from_slice(&0x8000u16.to_le_bytes());
        for s in [name, kind, addr] {
            b.extend(s.encode_utf16().flat_map(|u| u.to_le_bytes()));
            b.extend_from_slice(&[0, 0]);
        }
        b
    }

    #[test]
    fn reads_a_one_off_entryid() {
        assert_eq!(
            entryid_address(&one_off("Example Sender", "SMTP", "sender@example.com")),
            Some((
                Some("Example Sender".to_string()),
                Some("sender@example.com".to_string())
            ))
        );
        // A non-SMTP address type yields the name only, never a bogus address.
        assert_eq!(
            entryid_address(&one_off("Fax Gateway", "FAX", "+1 555 0100")),
            Some((Some("Fax Gateway".to_string()), None))
        );
        // An Exchange EntryID has a different provider UID and no name in it.
        let mut ab = one_off("x", "SMTP", "x@y.z");
        ab[4] = 0xDC;
        assert_eq!(entryid_address(&ab), None);
        assert_eq!(entryid_address(&[0; 8]), None);
    }

    #[test]
    fn strips_subject_prefix_escape() {
        assert_eq!(clean_subject("\u{1}\u{4}RE: hello"), "RE: hello");
        assert_eq!(clean_subject("plain"), "plain");
    }
}
