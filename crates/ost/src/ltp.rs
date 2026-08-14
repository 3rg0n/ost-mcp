//! Lists, tables and properties layer: heap-on-node, the heap BTree, and the
//! property and table contexts built on them.
//!
//! Every structure here is byte-for-byte identical between MS-PST and the v36
//! OST, verified against a live v36 file. The one difference is where a HID
//! divides into its index and block index; see [`hid_parts`].

use crate::ndb::{u16le, u32le, u64le, Pff};
use crate::props::{decode_string, PT_BINARY, PT_STRING8, PT_UNICODE};
use crate::{Error, Result};
use std::collections::HashMap;

/// Split a HID into `(hidIndex, hidBlockIndex)`. The boundary between them moved
/// in v36, so it comes from [`crate::ndb::Geom`] rather than being a constant.
fn hid_parts(hid: u32, block_shift: u32) -> (usize, usize) {
    let index_mask = (1u32 << (block_shift - 5)) - 1;
    (
        ((hid >> 5) & index_mask) as usize,
        (hid >> block_shift) as usize,
    )
}

/// Heap-on-node: a byte heap spread over a node's data blocks.
pub struct Hn {
    blocks: Vec<Vec<u8>>,
    /// Per block, the HNPAGEMAP allocation table (`rgibAlloc`).
    maps: Vec<Vec<u16>>,
    hid_block_shift: u32,
    pub client_sig: u8,
    pub hid_user_root: u32,
}

impl Hn {
    pub fn parse(blocks: Vec<Vec<u8>>, hid_block_shift: u32) -> Result<Hn> {
        let first = blocks
            .first()
            .ok_or_else(|| Error::Format("heap node has no blocks".into()))?;
        if first.len() < 12 {
            return Err(Error::Format("first heap block is truncated".into()));
        }
        if first[2] != 0xEC {
            return Err(Error::Format(format!(
                "HNHDR bSig is 0x{:02X}, expected 0xEC",
                first[2]
            )));
        }
        let client_sig = first[3];
        let hid_user_root = u32le(first, 4);

        let mut maps = Vec::with_capacity(blocks.len());
        for blk in &blocks {
            let mut rgib = Vec::new();
            if blk.len() >= 2 {
                let ib_map = u16le(blk, 0) as usize;
                if ib_map + 4 <= blk.len() {
                    let c_alloc = u16le(blk, ib_map) as usize;
                    for k in 0..=c_alloc {
                        let o = ib_map + 4 + k * 2;
                        if o + 2 > blk.len() {
                            break;
                        }
                        rgib.push(u16le(blk, o));
                    }
                }
            }
            maps.push(rgib);
        }

        Ok(Hn {
            blocks,
            maps,
            hid_block_shift,
            client_sig,
            hid_user_root,
        })
    }

    /// Resolve a HID to its heap allocation.
    ///
    /// HID layout: bits 0-4 are the type (0 for a HID), then the 1-based index
    /// into the block's allocation map, then the block index. Where the latter
    /// two divide is version-specific; see [`hid_parts`].
    pub fn item(&self, hid: u32) -> Result<&[u8]> {
        if hid & 0x1F != 0 {
            return Err(Error::Format(format!("0x{hid:08X} is not a HID")));
        }
        let (idx, blk_i) = hid_parts(hid, self.hid_block_shift);
        if idx == 0 {
            return Err(Error::Format("hidIndex 0 is reserved".into()));
        }
        let blk = self
            .blocks
            .get(blk_i)
            .ok_or_else(|| Error::Format(format!("HID block index {blk_i} is out of range")))?;
        let map = &self.maps[blk_i];
        if idx >= map.len() {
            return Err(Error::Format(format!(
                "hidIndex {idx} is beyond a {}-entry allocation map",
                map.len()
            )));
        }
        let (s, e) = (map[idx - 1] as usize, map[idx] as usize);
        if s > e || e > blk.len() {
            return Err(Error::Format("heap item runs outside its block".into()));
        }
        Ok(&blk[s..e])
    }
}

/// Walk a heap BTree and return every leaf record as `(key, data)`.
pub fn bth_records(hn: &Hn, hid_root: u32) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
    let hdr = hn.item(hid_root)?;
    if hdr.len() < 8 {
        return Err(Error::Format("BTHHEADER is truncated".into()));
    }
    if hdr[0] != 0xB5 {
        return Err(Error::Format(format!(
            "BTHHEADER bType is 0x{:02X}, expected 0xB5",
            hdr[0]
        )));
    }
    let (cb_key, cb_ent, levels, root) = (
        hdr[1] as usize,
        hdr[2] as usize,
        hdr[3],
        u32le(hdr, 4),
    );
    let mut out = Vec::new();
    if root != 0 && cb_key != 0 && cb_ent != 0 {
        bth_walk(hn, root, cb_key, cb_ent, levels, &mut out)?;
    }
    Ok(out)
}

fn bth_walk(
    hn: &Hn,
    hid: u32,
    cb_key: usize,
    cb_ent: usize,
    level: u8,
    out: &mut Vec<(Vec<u8>, Vec<u8>)>,
) -> Result<()> {
    let buf = hn.item(hid)?;
    if level == 0 {
        let rec = cb_key + cb_ent;
        for i in 0..buf.len() / rec {
            let o = i * rec;
            out.push((
                buf[o..o + cb_key].to_vec(),
                buf[o + cb_key..o + rec].to_vec(),
            ));
        }
    } else {
        let rec = cb_key + 4;
        for i in 0..buf.len() / rec {
            let o = i * rec;
            bth_walk(hn, u32le(buf, o + cb_key), cb_key, cb_ent, level - 1, out)?;
        }
    }
    Ok(())
}

/// Resolve an HNID: either a HID into the local heap, or a NID into the node's
/// subnode BTree for values too large to keep on the heap.
fn resolve_hnid(
    pff: &Pff,
    hn: &Hn,
    subs: &HashMap<u32, (u64, u64)>,
    hnid: u32,
) -> Option<Vec<u8>> {
    if hnid == 0 {
        return None;
    }
    if hnid & 0x1F == 0 {
        hn.item(hnid).ok().map(|s| s.to_vec())
    } else {
        let (bid_data, _) = subs.get(&hnid)?;
        pff.data_flat(*bid_data).ok()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Prop {
    pub id: u16,
    pub ptype: u16,
    /// Either an inline value (types of 4 bytes or fewer) or an HNID.
    pub raw: u32,
}

/// A property context: a node whose heap holds a BTH of property values.
pub struct Pc<'a> {
    pff: &'a Pff,
    hn: Hn,
    subs: HashMap<u32, (u64, u64)>,
    pub props: Vec<Prop>,
}

impl<'a> Pc<'a> {
    pub fn open(pff: &'a Pff, nid: u32) -> Result<Pc<'a>> {
        let n = pff.node(nid)?;
        Pc::open_at(pff, n.bid_data, n.bid_sub)
    }

    pub fn open_at(pff: &'a Pff, bid_data: u64, bid_sub: u64) -> Result<Pc<'a>> {
        let hn = Hn::parse(pff.data_blocks(bid_data)?, pff.geom.hid_block_shift)?;
        if hn.client_sig != 0xBC {
            return Err(Error::Format(format!(
                "bClientSig is 0x{:02X}, not a property context (0xBC)",
                hn.client_sig
            )));
        }
        let subs = pff.subnodes(bid_sub)?;
        let mut props: Vec<Prop> = bth_records(&hn, hn.hid_user_root)?
            .into_iter()
            .filter(|(k, v)| k.len() >= 2 && v.len() >= 6)
            .map(|(k, v)| Prop {
                id: u16le(&k, 0),
                ptype: u16le(&v, 0),
                raw: u32le(&v, 2),
            })
            .collect();
        props.sort_by_key(|p| p.id);
        Ok(Pc {
            pff,
            hn,
            subs,
            props,
        })
    }

    pub fn prop(&self, id: u16) -> Option<Prop> {
        self.props.iter().find(|p| p.id == id).copied()
    }

    /// Raw bytes of a variable-length property.
    pub fn bytes(&self, id: u16) -> Option<Vec<u8>> {
        let p = self.prop(id)?;
        resolve_hnid(self.pff, &self.hn, &self.subs, p.raw)
    }

    pub fn string(&self, id: u16) -> Option<String> {
        let p = self.prop(id)?;
        if p.ptype != PT_UNICODE && p.ptype != PT_STRING8 {
            return None;
        }
        let b = resolve_hnid(self.pff, &self.hn, &self.subs, p.raw)?;
        Some(decode_string(p.ptype, &b))
    }

    /// A string property, or a binary one decoded as UTF-8. `PidTagHtml` is
    /// declared `PT_BINARY` in practice even though it holds text.
    pub fn text(&self, id: u16) -> Option<String> {
        let p = self.prop(id)?;
        let b = resolve_hnid(self.pff, &self.hn, &self.subs, p.raw)?;
        match p.ptype {
            PT_UNICODE | PT_STRING8 => Some(decode_string(p.ptype, &b)),
            PT_BINARY => Some(String::from_utf8_lossy(&b).into_owned()),
            _ => None,
        }
    }

    pub fn i32(&self, id: u16) -> Option<i32> {
        self.prop(id).map(|p| p.raw as i32)
    }

    pub fn bool(&self, id: u16) -> Option<bool> {
        self.prop(id).map(|p| p.raw != 0)
    }

    /// 8-byte values live off-heap in a property context, unlike in a table row.
    pub fn u64(&self, id: u16) -> Option<u64> {
        let b = self.bytes(id)?;
        (b.len() >= 8).then(|| u64le(&b, 0))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct TcCol {
    pub tag: u32,
    pub ib: usize,
    pub cb: usize,
    /// This column's bit in each row's cell existence bitmap.
    pub i_bit: u8,
}

impl TcCol {
    pub fn id(&self) -> u16 {
        (self.tag >> 16) as u16
    }

    pub fn ptype(&self) -> u16 {
        (self.tag & 0xFFFF) as u16
    }
}

/// A table context: a row matrix plus its column descriptors.
pub struct Tc<'a> {
    pff: &'a Pff,
    hn: Hn,
    subs: HashMap<u32, (u64, u64)>,
    pub cols: Vec<TcCol>,
    pub row_size: usize,
    /// Offset within a row where the cell existence bitmap begins; it runs to
    /// `row_size`.
    pub ceb_start: usize,
    pub rows: Vec<Vec<u8>>,
}

impl<'a> Tc<'a> {
    pub fn open(pff: &'a Pff, nid: u32) -> Result<Tc<'a>> {
        let n = pff.node(nid)?;
        Tc::open_at(pff, n.bid_data, n.bid_sub)
    }

    pub fn open_at(pff: &'a Pff, bid_data: u64, bid_sub: u64) -> Result<Tc<'a>> {
        let hn = Hn::parse(pff.data_blocks(bid_data)?, pff.geom.hid_block_shift)?;
        if hn.client_sig != 0x7C {
            return Err(Error::Format(format!(
                "bClientSig is 0x{:02X}, not a table context (0x7C)",
                hn.client_sig
            )));
        }
        let subs = pff.subnodes(bid_sub)?;
        let info = hn.item(hn.hid_user_root)?.to_vec();
        if info.len() < 22 || info[0] != 0x7C {
            return Err(Error::Format("TCINFO is truncated or mistyped".into()));
        }
        let c_cols = info[1] as usize;
        // rgib[TCI_1b] ends the 1-byte cells and so begins the cell existence
        // bitmap; rgib[TCI_bm] ends that bitmap and is the row stride.
        let ceb_start = u16le(&info, 6) as usize;
        let row_size = u16le(&info, 8) as usize;
        let hnid_rows = u32le(&info, 14);

        let mut cols = Vec::with_capacity(c_cols);
        for i in 0..c_cols {
            let o = 22 + i * 8;
            if o + 8 > info.len() {
                break;
            }
            cols.push(TcCol {
                tag: u32le(&info, o),
                ib: u16le(&info, o + 4) as usize,
                cb: info[o + 6] as usize,
                i_bit: info[o + 7],
            });
        }

        // The row matrix is either a heap item or, once it outgrows the heap, a
        // subnode whose data tree spans many blocks. Rows never straddle a block.
        let mut rows = Vec::new();
        if hnid_rows != 0 && row_size != 0 {
            let blocks: Vec<Vec<u8>> = if hnid_rows & 0x1F == 0 {
                vec![hn.item(hnid_rows)?.to_vec()]
            } else {
                let (bid, _) = subs.get(&hnid_rows).ok_or_else(|| {
                    Error::Format(format!("hnidRows 0x{hnid_rows:08X} is not a subnode"))
                })?;
                pff.data_blocks(*bid)?
            };
            for blk in blocks {
                for i in 0..blk.len() / row_size {
                    rows.push(blk[i * row_size..(i + 1) * row_size].to_vec());
                }
            }
        }

        Ok(Tc {
            pff,
            hn,
            subs,
            cols,
            row_size,
            ceb_start,
            rows,
        })
    }

    /// Column sets differ per table, so every accessor is a lookup by property
    /// id and returns `None` when a table simply does not carry that column.
    pub fn col(&self, id: u16) -> Option<&TcCol> {
        self.cols
            .iter()
            .find(|c| c.id() == id)
            .filter(|c| c.ib + c.cb <= self.row_size)
    }

    /// The column, but only if this row actually has a value for it.
    ///
    /// A row's cells are fixed-width slots that are not cleared when a value is
    /// absent, so the bytes of an unset cell are whatever was there before —
    /// stale HIDs that either fail to resolve or, worse, resolve to some
    /// unrelated heap item. The cell existence bitmap is the only thing that
    /// distinguishes the two, so no accessor may skip it.
    fn cell(&self, row: &[u8], id: u16) -> Option<&TcCol> {
        self.col(id).filter(|c| self.exists(row, c))
    }

    /// Test a column's bit in the row's cell existence bitmap. The bitmap is
    /// big-endian within each byte: column `iBit` is bit `7 - iBit % 8` of byte
    /// `iBit / 8` (MS-PST 2.3.4.4.1).
    pub fn exists(&self, row: &[u8], c: &TcCol) -> bool {
        let i = self.ceb_start + c.i_bit as usize / 8;
        match row.get(i) {
            Some(b) => b & (0x80 >> (c.i_bit % 8)) != 0,
            // A row stride that leaves no room for the bitmap: nothing to test.
            None => true,
        }
    }

    /// `dwRowID`, the first 4 bytes of every row. In a hierarchy table it is the
    /// child folder's NID; in a contents table, the message's.
    pub fn row_id(row: &[u8]) -> u32 {
        if row.len() >= 4 {
            u32le(row, 0)
        } else {
            0
        }
    }

    pub fn bytes(&self, row: &[u8], id: u16) -> Option<Vec<u8>> {
        let c = self.cell(row, id)?;
        resolve_hnid(self.pff, &self.hn, &self.subs, u32le(row, c.ib))
    }

    pub fn string(&self, row: &[u8], id: u16) -> Option<String> {
        let c = self.cell(row, id)?;
        let b = resolve_hnid(self.pff, &self.hn, &self.subs, u32le(row, c.ib))?;
        Some(decode_string(c.ptype(), &b))
    }

    pub fn i32(&self, row: &[u8], id: u16) -> Option<i32> {
        let c = self.cell(row, id)?;
        Some(u32le(row, c.ib) as i32)
    }

    pub fn bool(&self, row: &[u8], id: u16) -> Option<bool> {
        let c = self.cell(row, id)?;
        Some(row[c.ib] != 0)
    }

    /// 8-byte values are stored inline in a row, unlike in a property context.
    pub fn u64(&self, row: &[u8], id: u16) -> Option<u64> {
        let c = self.cell(row, id)?;
        (c.cb >= 8).then(|| u64le(row, c.ib))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hid_split_is_version_specific() {
        // hidUserRoot of the Deleted Items contents table in a live v36 OST. Its
        // heap has 304 blocks, so only the bit-19 split is in range.
        assert_eq!(hid_parts(0x0348_4520, 19), (553, 105));
        assert_eq!(hid_parts(0x0348_4520, 16), (553, 840));
        // A single-block heap decodes the same either way, which is why v23 and
        // v36 agree on every small property context.
        assert_eq!(hid_parts(0x20, 19), (1, 0));
        assert_eq!(hid_parts(0x20, 16), (1, 0));
    }
}
