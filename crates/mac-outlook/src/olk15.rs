//! Parsers for the three `.olk15*` file types that carry message content.
//!
//! Layouts are as measured and credited in `docs/mac-outlook-format.md` §3.2,
//! derived independently from reading (not porting) two external projects:
//! `xianhammer/format`'s `olk15` Go package and `thomasmaerz/olk15-export`'s
//! Python pipeline. None of this was re-verified against a real file from
//! this project's own measurement machine — that account's classic engine
//! has no message files at all (§3.2) — so treat every offset here as
//! "measured elsewhere, not yet by us" until re-confirmed against a real
//! `.olk15Message`/`.olk15MsgSource`/`.olk15MsgAttachment`.
//!
//! Portable on purpose: no `cfg(target_os = ...)` here. Discovery is the only
//! macOS-specific part (see [`crate::discover`]); a copied profile can be
//! parsed on any OS, which is what makes the fixtures below possible without
//! a Mac.

/// Body content type a `.olk15Message` blob turned out to hold.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum BodyKind {
    Html,
    Rtf,
    Calendar,
    /// No known marker was found; the raw remainder past the header, decoded
    /// best-effort, is returned as plain text.
    Plain,
}

/// One decoded `.olk15Message` body.
#[derive(Debug, Clone)]
pub struct MessageBody {
    pub kind: BodyKind,
    pub text: String,
}

const MESSAGE_MAGIC: [u8; 4] = [0x0D, 0x00, 0x00, 0x01];
const MESSAGE_HEADER_LEN: usize = 20;

/// Decode a `.olk15Message` binary cache into its body text.
///
/// The 20-byte header (4-byte magic, 16-byte UUID) is not validated strictly:
/// a file that does not start with [`MESSAGE_MAGIC`] is still scanned for a
/// body marker, since the magic byte layout is one of the least-confirmed
/// parts of the format.
pub fn parse_message(data: &[u8]) -> Option<MessageBody> {
    let body = data.get(MESSAGE_HEADER_LEN..).unwrap_or(&[]);
    let (pos, kind, utf16) = find_body_start(body)?;
    let text = decode_best_effort(&body[pos..], utf16);
    let text = truncate_trailing_metadata(&text, kind);
    Some(MessageBody { kind, text })
}

/// Whether `data` begins with the `.olk15Message` magic, for diagnostics —
/// not required for [`parse_message`] to work, since the magic is unverified
/// against a real file.
pub fn looks_like_message(data: &[u8]) -> bool {
    data.starts_with(&MESSAGE_MAGIC)
}

fn find_body_start(data: &[u8]) -> Option<(usize, BodyKind, bool)> {
    let candidates: [(&[u8], BodyKind, bool); 6] = [
        (b"<html", BodyKind::Html, false),
        (b"<HTML", BodyKind::Html, false),
        (&utf16le(b"<html"), BodyKind::Html, true),
        (b"{\\rtf", BodyKind::Rtf, false),
        (b"BEGIN:VCALENDAR", BodyKind::Calendar, false),
        (&utf16le(b"BEGIN:VCALENDAR"), BodyKind::Calendar, true),
    ];
    candidates
        .into_iter()
        .filter_map(|(marker, kind, utf16)| find(data, marker).map(|p| (p, kind, utf16)))
        .min_by_key(|(p, _, _)| *p)
}

/// `s` re-encoded as UTF-16LE bytes, for building a byte-string marker.
fn utf16le(s: &[u8]) -> Vec<u8> {
    std::str::from_utf8(s)
        .unwrap()
        .encode_utf16()
        .flat_map(|u| u.to_le_bytes())
        .collect()
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len().max(1))
        .position(|w| w == needle)
}

fn decode_best_effort(bytes: &[u8], utf16: bool) -> String {
    if utf16 {
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        char::decode_utf16(units)
            .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect()
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

/// Outlook appends its own trailing metadata after the real content — a
/// serialized message card, an embedded XML fragment, or raw control bytes
/// once the text run ends. Cut at the earliest of: the format's own
/// closing tag, or one of those markers, whichever comes first.
fn truncate_trailing_metadata(text: &str, kind: BodyKind) -> String {
    let mut end = text.len();

    if kind == BodyKind::Html {
        if let Some(close) = rfind_ascii_ci(text, "</html>") {
            end = end.min(close + "</html>".len());
        } else if let Some(close) = rfind_ascii_ci(text, "</body>") {
            end = end.min(close + "</body>".len());
        }
    } else if kind == BodyKind::Calendar {
        if let Some(close) = text.rfind("END:VCALENDAR") {
            end = end.min(close + "END:VCALENDAR".len());
        }
    }

    for marker in [
        "{\"MessageCardSerialized\":",
        "<?xml version=\"1.0\" encoding=\"utf-16\"?>",
    ] {
        if let Some(pos) = text.find(marker) {
            end = end.min(pos);
        }
    }
    if let Some(pos) = find_control_run(text) {
        end = end.min(pos);
    }

    text[..end].trim_end().to_string()
}

/// Case-insensitive `rfind` for an ASCII `needle`, operating on raw bytes so
/// the match position is always a valid boundary in the original `haystack`.
///
/// `str::to_lowercase` is not safe to use for this: some characters change
/// byte length when case-folded (e.g. U+212A KELVIN SIGN, 3 bytes, lowercases
/// to `k`, 1 byte), so an offset found in a lowercased copy is not
/// necessarily a valid index into the original string — slicing on it can
/// panic. Restricting the fold to ASCII avoids the problem entirely: an ASCII
/// byte can never be a continuation byte of a multi-byte UTF-8 sequence, so a
/// match against ASCII-only bytes is always boundary-safe.
fn rfind_ascii_ci(haystack: &str, needle: &str) -> Option<usize> {
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    if n.is_empty() || h.len() < n.len() {
        return None;
    }
    (0..=h.len() - n.len())
        .rev()
        .find(|&i| h[i..i + n.len()].eq_ignore_ascii_case(n))
}

/// The byte offset of the first run of 3 or more control characters, which
/// marks the start of binary remnants past the real text.
fn find_control_run(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let is_control = |b: u8| matches!(b, 0x00..=0x08 | 0x0B | 0x0C | 0x0E..=0x1F);
    let mut run_start = None;
    let mut run_len = 0;
    for (i, &b) in bytes.iter().enumerate() {
        if is_control(b) {
            if run_len == 0 {
                run_start = Some(i);
            }
            run_len += 1;
            if run_len >= 3 {
                return run_start;
            }
        } else {
            run_len = 0;
        }
    }
    None
}

/// Strip the binary prefix ahead of a `.olk15MsgSource` file's actual RFC822
/// bytes. Returns `None` when no MIME header marker is found at all.
pub fn parse_source(data: &[u8]) -> Option<&[u8]> {
    const MARKERS: [&[u8]; 7] = [
        b"Received:",
        b"From:",
        b"Return-Path:",
        b"MIME-Version:",
        b"Date:",
        b"Subject:",
        b"Message-ID:",
    ];
    MARKERS
        .iter()
        .filter_map(|m| find(data, m))
        .min()
        .map(|pos| &data[pos..])
}

const ATTACHMENT_MAGIC: [u8; 4] = [0xd0, 0x0d, 0x00, 0x00];

#[derive(Debug, Clone)]
pub struct ParsedAttachment {
    pub filename: Option<String>,
    pub content_type: Option<String>,
    pub bytes: Vec<u8>,
}

struct AttachmentHeader {
    text: String,
    payload_start: usize,
}

/// Validate the magic and find the `\r\r` header/payload boundary — the one
/// step [`parse_attachment`] and [`attachment_metadata`] share, so listing
/// metadata does not require decoding the base64 payload.
fn locate_attachment_header(data: &[u8]) -> mailbox::Result<AttachmentHeader> {
    if !data.starts_with(&ATTACHMENT_MAGIC) {
        return Err(mailbox::Error::Format(
            "olk15MsgAttachment: missing d00d magic".into(),
        ));
    }
    let marker = find(data, b"\r\r").ok_or_else(|| {
        mailbox::Error::Format("olk15MsgAttachment: no \\r\\r header terminator".into())
    })?;
    Ok(AttachmentHeader {
        text: String::from_utf8_lossy(&data[..marker]).into_owned(),
        payload_start: marker + 2,
    })
}

/// Filename and content type only, without decoding the (possibly large)
/// base64 payload — for listing attachments cheaply.
pub fn attachment_metadata(data: &[u8]) -> mailbox::Result<(Option<String>, Option<String>)> {
    let h = locate_attachment_header(data)?;
    Ok((
        extract_header_value(&h.text, "name="),
        extract_header_value(&h.text, "Content-type:"),
    ))
}

/// Decode a `.olk15MsgAttachment` file: MIME-style headers (terminated by
/// `\r\r`, not the standard `\r\n\r\n`) followed by a base64 payload.
pub fn parse_attachment(data: &[u8]) -> mailbox::Result<ParsedAttachment> {
    let h = locate_attachment_header(data)?;
    let payload_start = h.payload_start;

    let is_b64 = |c: u8| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'/' | b'=' | b'\r' | b'\n');
    let payload_end = data[payload_start..]
        .iter()
        .position(|&c| !is_b64(c))
        .map(|p| payload_start + p)
        .unwrap_or(data.len());
    let b64: String = data[payload_start..payload_end]
        .iter()
        .filter(|&&c| c != b'\r' && c != b'\n')
        .map(|&c| c as char)
        .collect();

    let bytes = decode_base64(&b64)
        .ok_or_else(|| mailbox::Error::Format("olk15MsgAttachment: invalid base64".into()))?;

    Ok(ParsedAttachment {
        filename: extract_header_value(&h.text, "name="),
        content_type: extract_header_value(&h.text, "Content-type:"),
        bytes,
    })
}

/// Case-insensitive `find` for an ASCII `needle` — see [`rfind_ascii_ci`] for
/// why this cannot use `str::to_lowercase` on attacker-controlled `headers`.
fn find_ascii_ci(haystack: &str, needle: &str) -> Option<usize> {
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    if n.is_empty() || h.len() < n.len() {
        return None;
    }
    (0..=h.len() - n.len()).find(|&i| h[i..i + n.len()].eq_ignore_ascii_case(n))
}

fn extract_header_value(headers: &str, key: &str) -> Option<String> {
    let start = find_ascii_ci(headers, key)? + key.len();
    let rest = &headers[start..];
    let rest = rest.trim_start();
    if let Some(inner) = rest.strip_prefix('"') {
        let end = inner.find('"')?;
        Some(inner[..end].to_string())
    } else {
        let end = rest
            .find([';', '\r', '\n'])
            .unwrap_or(rest.len());
        let v = rest[..end].trim();
        if v.is_empty() {
            None
        } else {
            Some(v.to_string())
        }
    }
}

/// Padding is neither required nor rejected: [`locate_attachment_header`]'s
/// caller has already trimmed the payload at the first non-base64 byte, so
/// `=` may or may not be present depending on where that cut landed.
fn decode_base64(s: &str) -> Option<Vec<u8>> {
    use base64::engine::{general_purpose::GeneralPurpose, DecodePaddingMode, GeneralPurposeConfig};
    use base64::alphabet::STANDARD;
    use base64::Engine;

    let clean: String = s.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    let engine = GeneralPurpose::new(
        &STANDARD,
        GeneralPurposeConfig::new()
            .with_decode_padding_mode(DecodePaddingMode::Indifferent)
            .with_encode_padding(false),
    );
    engine.decode(clean).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message_fixture(body: &[u8]) -> Vec<u8> {
        let mut f = Vec::new();
        f.extend_from_slice(&MESSAGE_MAGIC);
        f.extend_from_slice(&[0u8; 16]); // UUID, contents irrelevant here
        f.extend_from_slice(body);
        f
    }

    #[test]
    fn extracts_ascii_html_body() {
        let f = message_fixture(b"garbage<html><body>Hello from example.com</body></html>trailing junk");
        let m = parse_message(&f).expect("body found");
        assert_eq!(m.kind, BodyKind::Html);
        assert_eq!(m.text, "<html><body>Hello from example.com</body></html>");
    }

    #[test]
    fn extracts_utf16_html_body() {
        let html = "<html><body>Ol\u{e1} from example.com</body></html>";
        let utf16_bytes: Vec<u8> = html.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        let f = message_fixture(&utf16_bytes);
        let m = parse_message(&f).expect("body found");
        assert_eq!(m.kind, BodyKind::Html);
        assert_eq!(m.text, html);
    }

    #[test]
    fn extracts_calendar_body() {
        let f = message_fixture(b"BEGIN:VCALENDAR\r\nSUMMARY:Sync\r\nEND:VCALENDAR\r\ntrailing");
        let m = parse_message(&f).expect("body found");
        assert_eq!(m.kind, BodyKind::Calendar);
        assert!(m.text.starts_with("BEGIN:VCALENDAR"));
        assert!(m.text.ends_with("END:VCALENDAR"));
    }

    #[test]
    fn truncates_message_card_metadata() {
        let f = message_fixture(
            b"<html>hi</html>{\"MessageCardSerialized\":{\"junk\":1}}",
        );
        let m = parse_message(&f).expect("body found");
        // </html> is found first by the closing-tag rule, so the metadata
        // marker never needs to bite here; this asserts it doesn't leak in.
        assert!(!m.text.contains("MessageCardSerialized"));
    }

    #[test]
    fn html_close_tag_search_does_not_panic_on_byte_length_changing_case_fold() {
        // U+212A KELVIN SIGN is 3 bytes in UTF-8 and folds to ASCII 'k' (1
        // byte) under `str::to_lowercase` — searching a lowercased copy of
        // text containing this character and slicing the *original* string
        // at the resulting offset used to panic with a non-char-boundary
        // index. `rfind_ascii_ci` must not reproduce that.
        let body = "<html>K\u{212A}elvin sign</html>".to_string();
        let f = message_fixture(body.as_bytes());
        let m = parse_message(&f).expect("body found");
        assert_eq!(m.kind, BodyKind::Html);
        assert!(m.text.ends_with("</html>"));
    }

    #[test]
    fn no_marker_returns_none() {
        let f = message_fixture(b"no markers in here at all");
        assert!(parse_message(&f).is_none());
    }

    #[test]
    fn strips_source_binary_prefix() {
        let raw = b"\x01\x02\x03junkFrom: sender@example.com\r\nSubject: Hi\r\n\r\nBody";
        let mime = parse_source(raw).expect("marker found");
        assert!(mime.starts_with(b"From: sender@example.com"));
    }

    #[test]
    fn source_with_no_marker_is_none() {
        assert!(parse_source(b"\x01\x02\x03 nothing mime-shaped here").is_none());
    }

    fn attachment_fixture(headers: &str, payload_b64: &str) -> Vec<u8> {
        let mut f = Vec::new();
        f.extend_from_slice(&ATTACHMENT_MAGIC);
        f.extend_from_slice(&[0u8; 12]);
        f.extend_from_slice(&[0u8; 16]); // GUID
        f.extend_from_slice(headers.as_bytes());
        f.extend_from_slice(b"\r\r");
        f.extend_from_slice(payload_b64.as_bytes());
        f
    }

    /// Minimal base64 encoder for the fixture only; the payload length is
    /// kept a multiple of 3 so no padding logic is needed.
    fn encode_base64(payload: &[u8]) -> String {
        const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        assert_eq!(payload.len() % 3, 0);
        let mut s = String::new();
        for chunk in payload.chunks(3) {
            let n = ((chunk[0] as u32) << 16) | ((chunk[1] as u32) << 8) | chunk[2] as u32;
            for shift in [18, 12, 6, 0] {
                s.push(A[((n >> shift) & 0x3f) as usize] as char);
            }
        }
        s
    }

    #[test]
    fn decodes_attachment_payload_and_filename() {
        let payload = b"hello world!"; // 12 bytes, multiple of 3
        let b64 = encode_base64(payload);
        let headers = "Content-type: application/pdf; name=\"invoice.pdf\";\r\nContent-Transfer-Encoding: base64";
        let f = attachment_fixture(headers, &b64);
        let a = parse_attachment(&f).expect("parses");
        assert_eq!(a.filename.as_deref(), Some("invoice.pdf"));
        assert_eq!(a.content_type.as_deref(), Some("application/pdf"));
        assert_eq!(a.bytes, payload);
    }

    #[test]
    fn rejects_wrong_magic() {
        let f = b"NOPE0000".to_vec();
        assert!(parse_attachment(&f).is_err());
    }

    #[test]
    fn header_value_search_does_not_panic_on_byte_length_changing_case_fold() {
        // Same U+212A hazard as the message-body test, this time in the
        // attachment header text `extract_header_value` scans.
        let payload = b"hello world!";
        let headers = "X-Junk: \u{212A}\r\nContent-type: text/plain; name=\"a.txt\"".to_string();
        let f = attachment_fixture(&headers, &encode_base64(payload));
        let a = parse_attachment(&f).expect("parses");
        assert_eq!(a.filename.as_deref(), Some("a.txt"));
        assert_eq!(a.content_type.as_deref(), Some("text/plain"));
    }
}
