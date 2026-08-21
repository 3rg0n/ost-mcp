//! `HxStore.hxd` file and block container.
//!
//! This is New Outlook's undocumented local cache, distinct from the classic
//! `Outlook.sqlite` + `.olk15*` engine in [`crate::schema`]/[`crate::olk15`].
//! Every structural claim here — the `Nostromo` magic, the 40-byte block
//! header with its two CRC-32s, the LZ4 payload framing — is credited to
//! [`securized/hxstore-reverse-engineering`](https://github.com/securized/hxstore-reverse-engineering)
//! (MIT), whose write-up describes reading it directly out of `HxCore.framework`
//! disassembly plus the Osa protocol logs (see `docs/mac-outlook-format.md`
//! for the section on this). This module is an independent implementation
//! informed by that research and re-verified against a real store in this
//! project, not a port of that project's code.
//!
//! ```text
//! file header (48+ bytes): magic "Nostromo", version byte, page size
//! block (40-byte header + LZ4 payload), repeated, found by magic scan:
//!   +0x00  u32  crc32(block[0x04..0x20])            header checksum
//!   +0x04  u32  crc32(block[0x08..0x28+payload_len]) payload checksum
//!   +0x08  u64  magic 0x5d0245643b706a05
//!   +0x10  u32  kind          (observed 8 and 16; semantics unconfirmed)
//!   +0x14  u32  payload_len   compressed bytes, starting at +0x28
//!   +0x18  u32  inflated_len  exact decompressed size
//!   +0x1c  u32  4             constant in every block observed so far
//!   +0x28  ..   LZ4-compressed payload
//! ```
//!
//! There is no usable block directory (a `KeyDirectory` walk exists in the
//! binary, but no valid chain was found in `.hxd` files either upstream or in
//! this project's own testing — it opens the smaller `.ctr` sidecar stores
//! instead). Blocks are found by scanning for the magic and validating both
//! checksums plus the declared inflated length; a false positive cannot
//! survive all three, which is what makes a magic scan safe here.

use crate::hxlz4;

pub const FILE_MAGIC: &[u8; 8] = b"Nostromo";
/// The macOS build under test writes `'i'`; Windows Mail literature describes
/// `'h'` for the same block container. `'h'` is untested by this project.
pub const KNOWN_VERSIONS: [u8; 2] = *b"ih";

const BLOCK_MAGIC: u64 = 0x5d02_4564_3b70_6a05;
const BLOCK_HEADER_LEN: usize = 0x28;
/// A block larger than this is treated as corrupt rather than allocated —
/// guards against a crafted or garbage `inflated_len` causing a huge
/// allocation before the checksum that would have rejected it is even
/// reached.
const MAX_INFLATED_LEN: usize = 32 << 20;

pub struct FileHeader {
    pub version: u8,
    pub known_version: bool,
    pub page_size: u64,
}

/// Validate the file header. `Err` only when this is not an HxStore at all;
/// an unrecognised version still parses, since the per-block checksums (not
/// the version byte) are what actually guards correctness.
pub fn check_header(data: &[u8]) -> Result<FileHeader, mailbox::Error> {
    if data.len() < 0x40 {
        return Err(mailbox::Error::Format("HxStore.hxd: file too small for a header".into()));
    }
    if &data[..8] != FILE_MAGIC {
        return Err(mailbox::Error::Format("HxStore.hxd: missing Nostromo magic".into()));
    }
    let version = data[8];
    Ok(FileHeader {
        version,
        known_version: KNOWN_VERSIONS.contains(&version),
        page_size: u64::from_le_bytes(data[0x38..0x40].try_into().unwrap()),
    })
}

pub struct Block {
    pub kind: u32,
    pub data: Vec<u8>,
}

fn u32_at(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
}

/// Parse and fully verify the block whose magic was found at `off + 8`.
/// Returns `None` unless the header checksum, the payload checksum and the
/// declared inflated length all agree.
fn parse_block(data: &[u8], off: usize) -> Option<Block> {
    let header = data.get(off..off + BLOCK_HEADER_LEN)?;
    let crc_header = u32_at(header, 0x00);
    let crc_body = u32_at(header, 0x04);
    let kind = u32_at(header, 0x10);
    let payload_len = u32_at(header, 0x14) as usize;
    let inflated_len = u32_at(header, 0x18) as usize;

    if inflated_len == 0 || inflated_len > MAX_INFLATED_LEN {
        return None;
    }
    let end = off.checked_add(BLOCK_HEADER_LEN)?.checked_add(payload_len)?;
    if end > data.len() {
        return None;
    }

    if crc32(&data[off + 4..off + 0x20]) != crc_header {
        return None;
    }
    if crc32(&data[off + 8..end]) != crc_body {
        return None;
    }

    let decoded = hxlz4::decode_exact(&data[off + BLOCK_HEADER_LEN..end], inflated_len)?;
    Some(Block { kind, data: decoded })
}

/// Every verified block in the file, found by scanning for the block magic
/// rather than by walking a directory (see the module doc for why).
pub fn scan_blocks(data: &[u8]) -> Vec<Block> {
    let magic = BLOCK_MAGIC.to_le_bytes();
    memchr_find_iter(data, &magic)
        .filter_map(|m| m.checked_sub(8))
        .filter_map(|off| parse_block(data, off))
        .collect()
}

/// A small `memmem`-style search so this crate does not need to add `memchr`
/// solely for an 8-byte needle scanned once per file.
fn memchr_find_iter<'a>(haystack: &'a [u8], needle: &'a [u8]) -> impl Iterator<Item = usize> + 'a {
    let mut start = 0;
    std::iter::from_fn(move || {
        if start > haystack.len().saturating_sub(needle.len()) {
            return None;
        }
        let rest = &haystack[start..];
        let pos = rest.windows(needle.len()).position(|w| w == needle)?;
        let found = start + pos;
        start = found + 1;
        Some(found)
    })
}

/// CRC-32 (IEEE / zlib polynomial `0xEDB88320`), the same algorithm gzip and
/// zlib use. Independently verifiable against the standard test vectors
/// (`crc32(b"") == 0`, `crc32(b"a") == 0xE8B7BE43`).
fn crc32(data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, entry) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            *entry = c;
        }
        t
    });

    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc = table[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_known_test_vectors() {
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"a"), 0xE8B7BE43);
        assert_eq!(crc32(b"123456789"), 0xCBF43926);
    }

    fn header(version: u8) -> Vec<u8> {
        let mut v = vec![0u8; 0x40];
        v[..8].copy_from_slice(FILE_MAGIC);
        v[8] = version;
        v[0x38..0x40].copy_from_slice(&4096u64.to_le_bytes());
        v
    }

    #[test]
    fn accepts_known_versions() {
        assert!(check_header(&header(b'i')).unwrap().known_version);
        assert!(check_header(&header(b'h')).unwrap().known_version);
    }

    #[test]
    fn flags_unknown_version_without_failing() {
        let h = check_header(&header(b'z')).unwrap();
        assert!(!h.known_version);
        assert_eq!(h.version, b'z');
    }

    #[test]
    fn rejects_missing_magic() {
        assert!(check_header(&[0u8; 0x40]).is_err());
    }

    #[test]
    fn rejects_too_small_file() {
        assert!(check_header(&[0u8; 8]).is_err());
    }

    fn synthetic_block(kind: u32, payload_plain: &[u8]) -> Vec<u8> {
        // A single literals-only LZ4 sequence for a payload under 15 bytes,
        // which is all these tests need.
        assert!(payload_plain.len() < 15);
        let mut compressed = vec![(payload_plain.len() as u8) << 4];
        compressed.extend_from_slice(payload_plain);

        let mut block = vec![0u8; BLOCK_HEADER_LEN];
        block[0x08..0x10].copy_from_slice(&BLOCK_MAGIC.to_le_bytes());
        block[0x10..0x14].copy_from_slice(&kind.to_le_bytes());
        block[0x14..0x18].copy_from_slice(&(compressed.len() as u32).to_le_bytes());
        block[0x18..0x1c].copy_from_slice(&(payload_plain.len() as u32).to_le_bytes());
        block.extend_from_slice(&compressed);

        let crc_body = crc32(&block[8..]);
        block[0x04..0x08].copy_from_slice(&crc_body.to_le_bytes());
        let crc_header = crc32(&block[4..0x20]);
        block[0x00..0x04].copy_from_slice(&crc_header.to_le_bytes());
        block
    }

    #[test]
    fn scans_and_decodes_a_well_formed_block() {
        let block = synthetic_block(8, b"hello hx");
        let blocks = scan_blocks(&block);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].kind, 8);
        assert_eq!(blocks[0].data, b"hello hx");
    }

    #[test]
    fn a_bit_flip_fails_checksum_and_is_skipped() {
        let mut block = synthetic_block(8, b"hello hx");
        let last = block.len() - 1;
        block[last] ^= 0x01;
        assert!(scan_blocks(&block).is_empty());
    }

    #[test]
    fn magic_bytes_inside_garbage_do_not_produce_a_false_positive() {
        let mut data = vec![0xAAu8; 64];
        data.extend_from_slice(&BLOCK_MAGIC.to_le_bytes());
        data.extend_from_slice(&[0xBBu8; 64]);
        assert!(scan_blocks(&data).is_empty());
    }

    #[test]
    fn truncated_file_does_not_panic() {
        let mut block = synthetic_block(8, b"hello hx");
        block.truncate(block.len() - 3);
        assert!(scan_blocks(&block).is_empty());
    }
}
