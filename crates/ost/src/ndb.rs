//! Node database layer: header, page framing, the node and block BTrees, and
//! block payload assembly.
//!
//! Everything variant-specific lives in [`Geom`]. Entry records (BTENTRY,
//! NBTENTRY, BBTENTRY) are identical across both supported versions, so only the
//! page envelope is parameterised.

use crate::{Error, Result};
use memmap2::Mmap;
use std::collections::HashMap;
use std::fs::File;
use std::path::Path;

pub(crate) fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

pub(crate) fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

pub(crate) fn u64le(b: &[u8], o: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    u64::from_le_bytes(a)
}

/// BID bit 0 is reserved and must be ignored when comparing (MS-PST 2.2.2.2).
fn bid_key(bid: u64) -> u64 {
    bid & !1
}

/// BID bit 1 marks an internal block: an XBLOCK/XXBLOCK data tree or an
/// SLBLOCK/SIBLOCK subnode tree rather than leaf data.
fn bid_is_internal(bid: u64) -> bool {
    bid & 0x02 != 0
}

/// Page framing, the only thing that differs between the supported versions.
#[derive(Clone, Copy, Debug)]
pub struct Geom {
    /// Page size in bytes.
    pub page: usize,
    /// Offset of the BTPAGE header (`cEnt`) within a page.
    pub bt_hdr: usize,
    /// v36 widened `cEnt`/`cEntMax` from u8 to u16, which shifts the two fields
    /// that follow them.
    pub counts_u16: bool,
    /// First bit of `hidBlockIndex` in a HID. MS-PST puts it at 16, giving an
    /// 11-bit `hidIndex`; v36 moves it to 19, giving 14 bits. Determined
    /// empirically: on a v36 heap of 304 blocks, `hidUserRoot` 0x03484520 only
    /// resolves to a valid TCINFO under the bit-19 split.
    pub hid_block_shift: u32,
}

impl Geom {
    fn cb_ent_off(self) -> usize {
        if self.counts_u16 {
            4
        } else {
            2
        }
    }

    fn c_level_off(self) -> usize {
        if self.counts_u16 {
            5
        } else {
            3
        }
    }
}

/// One NBT leaf entry: a node and the blocks holding its data and subnodes.
#[derive(Clone, Copy, Debug)]
pub struct Node {
    pub nid: u32,
    pub bid_data: u64,
    pub bid_sub: u64,
    pub nid_parent: u32,
}

/// One BBT leaf entry: where a block lives and how many bytes are stored.
#[derive(Clone, Copy, Debug)]
pub struct BlockRef {
    pub ib: u64,
    /// Stored size. For a compressed block this is the *compressed* length.
    pub cb: u16,
    pub cref: u16,
}

pub struct Pff {
    map: Mmap,
    pub ver: u16,
    pub geom: Geom,
    pub nbt: HashMap<u32, Node>,
    pub bbt: HashMap<u64, BlockRef>,
}

impl Pff {
    pub fn open(path: impl AsRef<Path>) -> Result<Pff> {
        // Rust opens with share mode READ|WRITE|DELETE on Windows, so this
        // succeeds against a running Outlook; the mapping then bypasses the
        // byte-range lock Outlook holds on the header.
        let file = File::open(path.as_ref())?;
        let map = unsafe { Mmap::map(&file)? };
        Self::from_map(map)
    }

    fn from_map(map: Mmap) -> Result<Pff> {
        if map.len() < 0x210 {
            return Err(Error::Format("file shorter than a PFF header".into()));
        }
        if &map[0..4] != b"!BDN" {
            return Err(Error::Format("missing !BDN magic".into()));
        }
        // 'SM' is a PST, 'SO' an OST. MS-PST documents only the former.
        let client = u16le(&map, 8);
        if client != 0x4D53 && client != 0x4F53 {
            return Err(Error::Format(format!(
                "unexpected wMagicClient 0x{client:04X}"
            )));
        }
        let ver = u16le(&map, 10);
        let geom = match ver {
            23 => Geom {
                page: 512,
                bt_hdr: 488,
                counts_u16: false,
                hid_block_shift: 16,
            },
            36 => Geom {
                page: 4096,
                bt_hdr: 4056,
                counts_u16: true,
                hid_block_shift: 19,
            },
            14 | 15 => {
                return Err(Error::Unsupported(
                    "32-bit ANSI PFF (wVer 14/15, Outlook 2002 and earlier)".into(),
                ))
            }
            v => return Err(Error::Unsupported(format!("unknown PFF wVer {v}"))),
        };
        // NDB_CRYPT_PERMUTE (1) and NDB_CRYPT_CYCLIC (2) need the 512-byte
        // substitution table from MS-PST 5.1, which is not implemented yet.
        // Rejecting here keeps every block read below unconditional.
        let crypt = map[0x201];
        if crypt != 0 {
            return Err(Error::Unsupported(format!(
                "bCryptMethod {crypt}; only NDB_CRYPT_NONE is implemented"
            )));
        }

        let nbt_ib = u64le(&map, 0xE0);
        let bbt_ib = u64le(&map, 0xF0);
        let mut pff = Pff {
            map,
            ver,
            geom,
            nbt: HashMap::new(),
            bbt: HashMap::new(),
        };

        let mut bbt = HashMap::new();
        pff.walk_leaves(bbt_ib, &mut |pg, c_ent, cb_ent| {
            for i in 0..c_ent {
                let o = i * cb_ent;
                bbt.insert(
                    bid_key(u64le(pg, o)),
                    BlockRef {
                        ib: u64le(pg, o + 8),
                        cb: u16le(pg, o + 16),
                        cref: u16le(pg, o + 18),
                    },
                );
            }
        })?;
        pff.bbt = bbt;

        let mut nbt = HashMap::new();
        pff.walk_leaves(nbt_ib, &mut |pg, c_ent, cb_ent| {
            for i in 0..c_ent {
                let o = i * cb_ent;
                // The NID is 4 bytes, widened to 8 in Unicode files to match
                // the width of BTENTRY.btkey.
                let nid = u64le(pg, o) as u32;
                nbt.insert(
                    nid,
                    Node {
                        nid,
                        bid_data: u64le(pg, o + 8),
                        bid_sub: u64le(pg, o + 16),
                        nid_parent: u32le(pg, o + 24),
                    },
                );
            }
        })?;
        pff.nbt = nbt;

        Ok(pff)
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    fn page(&self, ib: u64) -> Result<&[u8]> {
        let s = ib as usize;
        let e = s
            .checked_add(self.geom.page)
            .ok_or_else(|| Error::Format("page offset overflow".into()))?;
        self.map
            .get(s..e)
            .ok_or_else(|| Error::Format(format!("page at 0x{ib:X} is past EOF")))
    }

    /// Visit every leaf page of a BTree. The callback receives the page bytes,
    /// the live entry count, and the entry stride.
    fn walk_leaves(&self, root_ib: u64, f: &mut dyn FnMut(&[u8], usize, usize)) -> Result<()> {
        let g = self.geom;
        let mut stack = vec![(root_ib, 0usize)];
        while let Some((ib, depth)) = stack.pop() {
            if depth > 16 {
                return Err(Error::Format("BTree deeper than 16 levels".into()));
            }
            let pg = self.page(ib)?;
            let c_ent = if g.counts_u16 {
                u16le(pg, g.bt_hdr) as usize
            } else {
                pg[g.bt_hdr] as usize
            };
            let cb_ent = pg[g.bt_hdr + g.cb_ent_off()] as usize;
            let c_level = pg[g.bt_hdr + g.c_level_off()];
            if cb_ent == 0 || c_ent.saturating_mul(cb_ent) > g.bt_hdr {
                return Err(Error::Format(format!(
                    "BTree page at 0x{ib:X} claims {c_ent} entries of {cb_ent} bytes"
                )));
            }
            if c_level == 0 {
                f(pg, c_ent, cb_ent);
                continue;
            }
            // BTENTRY: btkey u64, BREF { bid u64, ib u64 }
            for i in 0..c_ent {
                stack.push((u64le(pg, i * cb_ent + 16), depth + 1));
            }
        }
        Ok(())
    }

    pub fn node(&self, nid: u32) -> Result<Node> {
        self.nbt
            .get(&nid)
            .copied()
            .ok_or_else(|| Error::NotFound(format!("nid 0x{nid:X} is not in the node BTree")))
    }

    /// One block's payload, inflated if the file stored it compressed.
    ///
    /// v36 stores most block payloads as raw zlib streams. There is no flag for
    /// it anywhere in the BBTENTRY (`dwPadding` was tested and ruled out across
    /// 200k blocks), so detection is a header sniff plus a successful inflate.
    fn block(&self, bid: u64) -> Result<Vec<u8>> {
        let e = *self
            .bbt
            .get(&bid_key(bid))
            .ok_or_else(|| Error::NotFound(format!("bid 0x{bid:X} is not in the block BTree")))?;
        let s = e.ib as usize;
        let end = s
            .checked_add(e.cb as usize)
            .ok_or_else(|| Error::Format("block offset overflow".into()))?;
        let raw = self
            .map
            .get(s..end)
            .ok_or_else(|| Error::Format(format!("block 0x{bid:X} is past EOF")))?;
        if looks_zlib(raw) {
            if let Some(v) = inflate(raw) {
                return Ok(v);
            }
        }
        Ok(raw.to_vec())
    }

    /// Assemble a node's data tree into its constituent blocks, in order. A leaf
    /// bid yields one block; an internal bid is an XBLOCK or XXBLOCK whose
    /// children are concatenated.
    pub fn data_blocks(&self, bid: u64) -> Result<Vec<Vec<u8>>> {
        self.data_blocks_at(bid, 0)
    }

    fn data_blocks_at(&self, bid: u64, depth: usize) -> Result<Vec<Vec<u8>>> {
        let payload = self.block(bid)?;
        if !bid_is_internal(bid) {
            return Ok(vec![payload]);
        }
        if depth > 4 {
            return Err(Error::Format("data tree deeper than 4 levels".into()));
        }
        if payload.len() < 8 {
            return Err(Error::Format(format!("XBLOCK 0x{bid:X} is truncated")));
        }
        if payload[0] != 0x01 {
            return Err(Error::Format(format!(
                "bid 0x{bid:X} is internal but btype is 0x{:02X}, expected 0x01",
                payload[0]
            )));
        }
        let c_ent = u16le(&payload, 2) as usize;
        if 8 + c_ent * 8 > payload.len() {
            return Err(Error::Format(format!(
                "XBLOCK 0x{bid:X} claims {c_ent} children but holds {} bytes",
                payload.len()
            )));
        }
        // An XXBLOCK's children are XBLOCK bids, which carry the internal flag
        // themselves, so one recursion handles both levels.
        let mut out = Vec::new();
        for i in 0..c_ent {
            out.extend(self.data_blocks_at(u64le(&payload, 8 + i * 8), depth + 1)?);
        }
        Ok(out)
    }

    pub fn data_flat(&self, bid: u64) -> Result<Vec<u8>> {
        Ok(self.data_blocks(bid)?.concat())
    }

    /// A node's subnode BTree, flattened to `nid -> (bid_data, bid_sub)`.
    pub fn subnodes(&self, bid_sub: u64) -> Result<HashMap<u32, (u64, u64)>> {
        let mut out = HashMap::new();
        if bid_sub != 0 {
            self.subnodes_into(bid_sub, &mut out, 0)?;
        }
        Ok(out)
    }

    fn subnodes_into(
        &self,
        bid: u64,
        out: &mut HashMap<u32, (u64, u64)>,
        depth: usize,
    ) -> Result<()> {
        if depth > 4 {
            return Err(Error::Format("subnode tree deeper than 4 levels".into()));
        }
        let payload = self.block(bid)?;
        if payload.len() < 8 {
            return Err(Error::Format(format!("SLBLOCK 0x{bid:X} is truncated")));
        }
        if payload[0] != 0x02 {
            return Err(Error::Format(format!(
                "subnode block 0x{bid:X} btype is 0x{:02X}, expected 0x02",
                payload[0]
            )));
        }
        let c_level = payload[1];
        let c_ent = u16le(&payload, 2) as usize;
        let stride = if c_level == 0 { 24 } else { 16 };
        if 8 + c_ent * stride > payload.len() {
            return Err(Error::Format(format!(
                "subnode block 0x{bid:X} claims {c_ent} entries but holds {} bytes",
                payload.len()
            )));
        }
        for i in 0..c_ent {
            let o = 8 + i * stride;
            if c_level == 0 {
                // SLENTRY: nid u64, bidData u64, bidSub u64
                out.insert(
                    u64le(&payload, o) as u32,
                    (u64le(&payload, o + 8), u64le(&payload, o + 16)),
                );
            } else {
                // SIENTRY: nid u64, bid u64
                self.subnodes_into(u64le(&payload, o + 8), out, depth + 1)?;
            }
        }
        Ok(())
    }
}

/// Sniff a zlib stream: low nibble of byte 0 is 8 (deflate) and the two header
/// bytes form a multiple of 31 (RFC 1950 check bits).
fn looks_zlib(b: &[u8]) -> bool {
    b.len() >= 2 && b[0] & 0x0F == 8 && (((b[0] as u16) << 8) | b[1] as u16).is_multiple_of(31)
}

fn inflate(b: &[u8]) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut out = Vec::new();
    let mut d = flate2::read::ZlibDecoder::new(b);
    match d.read_to_end(&mut out) {
        Ok(_) if !out.is_empty() => Some(out),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zlib_sniff_accepts_real_headers_and_rejects_text() {
        assert!(looks_zlib(&[0x78, 0x9C]));
        assert!(looks_zlib(&[0x78, 0x01]));
        assert!(looks_zlib(&[0x78, 0xDA]));
        // An HNHDR never opens with a valid zlib header pair.
        assert!(!looks_zlib(&[0x0C, 0xEC]));
        assert!(!looks_zlib(&[0x3C, 0x68]));
        assert!(!looks_zlib(&[0x78]));
    }

    #[test]
    fn bid_flags() {
        assert_eq!(bid_key(0x1D6E5DD), 0x1D6E5DC);
        assert!(bid_is_internal(0x02));
        assert!(!bid_is_internal(0x01));
    }
}
