//! LZ4 block decoder for `HxStore.hxd` payloads.
//!
//! Independently written from the public LZ4 block-format specification, not
//! from any one project's code — sequences are `[token][literal varint]
//! [literals][2-byte little-endian distance][match-length varint]`, with the
//! high nibble of `token` giving the literal count and the low nibble the
//! match length minus 4, either extended by a 255-continuation varint when it
//! hits 15. That this container's payload actually decodes as this format
//! (rather than something merely LZ4-shaped) is `docs/mac-outlook-format.md`
//! §2's claim, credited there to `securized/hxstore-reverse-engineering` and
//! independently re-verified in this project against a real store (see the
//! same section) — this module is the compression codec, which is public and
//! unrelated to whose research located it inside this particular file format.

/// Decode one block payload, requiring it to inflate to exactly `expected`
/// bytes.
///
/// The block container states the inflated size (see [`crate::hxstore`]), so a
/// correct decode lands on it precisely. LZ4 has no checksum of its own, so a
/// wrong start still "decodes" into something — anything short of an exact
/// length match, or a back-reference pointing outside the window, means this
/// was not really an LZ4 stream at this offset, and returning `None` beats
/// returning plausible garbage.
pub fn decode_exact(src: &[u8], expected: usize) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(expected);
    let mut i = 0usize;

    while i < src.len() && out.len() < expected {
        let token = *src.get(i)?;
        i += 1;

        let mut lit = (token >> 4) as usize;
        if lit == 15 {
            lit = read_varint(src, &mut i, lit)?;
        }
        let end = i.checked_add(lit)?;
        out.extend_from_slice(src.get(i..end)?);
        i = end;

        // A block's final sequence is literals only — there is nothing left
        // to read a match from.
        if out.len() >= expected || i >= src.len() {
            break;
        }

        let dist = u16::from_le_bytes([*src.get(i)?, *src.get(i + 1)?]) as usize;
        i += 2;
        if dist == 0 || dist > out.len() {
            return None;
        }

        let mut len = (token & 0x0F) as usize + 4;
        if (token & 0x0F) == 15 {
            len = read_varint(src, &mut i, len)?;
        }

        // A distance smaller than the match length means the copy overlaps
        // its own output (run-length expansion) and must proceed one byte at
        // a time rather than as a slice copy.
        let start = out.len() - dist;
        for k in 0..len {
            if out.len() >= expected {
                break;
            }
            out.push(out[start + k]);
        }
    }

    (out.len() == expected).then_some(out)
}

fn read_varint(src: &[u8], i: &mut usize, base: usize) -> Option<usize> {
    let mut n = base;
    loop {
        let b = *src.get(*i)?;
        *i += 1;
        n += b as usize;
        if b != 0xFF {
            return Some(n);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_only() {
        // token 0x50: literal count 5, match length nibble 0 (unused, no match follows).
        assert_eq!(decode_exact(&[0x50, b'h', b'e', b'l', b'l', b'o'], 5).unwrap(), b"hello");
    }

    #[test]
    fn back_reference_repeats() {
        // 4 literals "abcd", then distance 4 (LE u16), match length 4 -> "abcdabcd".
        let src = [0x40, b'a', b'b', b'c', b'd', 0x04, 0x00];
        assert_eq!(decode_exact(&src, 8).unwrap(), b"abcdabcd");
    }

    #[test]
    fn overlapping_copy_expands_a_run() {
        // 1 literal "x", distance 1, match length 4 -> "xxxxx".
        let src = [0x10, b'x', 0x01, 0x00];
        assert_eq!(decode_exact(&src, 5).unwrap(), b"xxxxx");
    }

    #[test]
    fn extended_literal_length_varint() {
        // token high nibble 15 -> read a 255-continuation varint (here: 0x0A = 10,
        // so 15 + 10 = 25 literals), no match follows.
        let mut src = vec![0xF0, 0x0A];
        src.extend_from_slice(&[b'z'; 25]);
        assert_eq!(decode_exact(&src, 25).unwrap(), vec![b'z'; 25]);
    }

    #[test]
    fn wrong_length_is_rejected() {
        assert!(decode_exact(&[0x50, b'h', b'e', b'l', b'l', b'o'], 6).is_none());
    }

    #[test]
    fn distance_outside_window_is_rejected() {
        assert!(decode_exact(&[0x10, b'x', 0x99, 0x00], 5).is_none());
    }

    #[test]
    fn truncated_input_is_rejected_not_panicking() {
        assert!(decode_exact(&[0xF0], 100).is_none());
        // An empty input decoding to zero expected bytes is trivially correct,
        // not truncated — this asserts it does not panic, and gets the right
        // answer either way.
        assert_eq!(decode_exact(&[], 0), Some(Vec::new()));
        assert!(decode_exact(&[0x40, b'a', b'b'], 10).is_none());
    }
}
