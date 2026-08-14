# OST format version 36 — deltas from MS-PST

Reverse-engineered 2026-08-14 from a live Outlook OST
(`%LOCALAPPDATA%\Microsoft\Outlook\<upn>.ost`, 4.32 GB, Outlook running, file
being actively written).

MS-PST does not document this variant. libpff describes it only as "64-bit
Unicode with 4k pages, discovered in an Outlook 2013 OST file". Everything below
was read directly off the file and confirmed by a full node-BTree descent.

## Summary

The node database is **MS-PST Unicode compatible except for page framing, block
sizing, and block compression**. Entry records (BTENTRY, NBTENTRY, BBTENTRY),
the ROOT struct, NID semantics, header field offsets, and the structures of the
LTP layer (HNHDR, HNPAGEMAP, BTH, PC, TC) are unchanged.

`bCryptMethod` is `NDB_CRYPT_NONE` — no permute or cyclic decode. Compression is
not encryption; there is nothing to decrypt.

There are **five** deltas from MS-PST:

1. `wMagicClient` is `'SO'`, not `'SM'`.
2. `wVer` is 36 (and `wVerClient` 12); pages are 4096 bytes, not 512.
3. BTPAGE counts widened u8 → u16; PAGETRAILER 16 → 24 bytes.
4. Block payloads may be zlib-compressed (see below).
5. Max block payload is **65,512** bytes, not 8,176 — and the HID bit split moves
   with it (see below).

Everything else works untouched.

## HEADER

Standard MS-PST Unicode offsets, all validated:

| Offset | Field | Observed | MS-PST expects |
|---|---|---|---|
| 0x000 | `dwMagic` | `!BDN` | `!BDN` — same |
| 0x008 | `wMagicClient` | `SO` (0x4F53) | `SM` (0x4D53) — **differs** |
| 0x00A | `wVer` | **36** | 14/15 ANSI, 23 Unicode — **differs** |
| 0x00C | `wVerClient` | 12 | 19 — differs |
| 0x00E | `bPlatformCreate` | 0x01 | 0x01 — same |
| 0x00F | `bPlatformAccess` | 0x01 | 0x01 — same |
| 0x02C | `rgnid[]` | plausible NID allocations | same |
| 0x0B4 | ROOT.`dwReserved` | 0 | same |
| 0x0B8 | ROOT.`ibFileEof` | 4,636,876,800 — exact file size | same |
| 0x0C0 | ROOT.`ibAMapLast` | 4,620,197,888 | same |
| 0x0C8 | ROOT.`cbAMapFree` | 1,455,480,832 | same |
| 0x0D0 | ROOT.`cbPMapFree` | 0 | same |
| 0x0D8 | ROOT.`BREFNBT` | bid 0x1D6E5DD, ib 0x6C963000 | same |
| 0x0E8 | ROOT.`BREFBBT` | bid 0x1D6E5E5, ib 0x6C7BF000 | same |
| 0x0F8 | ROOT.`fAMapValid` | 2 | same |
| 0x200 | `wSentinel` | 0x80 | 0x80 — same |
| 0x201 | `bCryptMethod` | 0 = `NDB_CRYPT_NONE` | same field, no crypt |

Header validation must therefore accept `wMagicClient == 'SO'` and
`wVer == 36`. A strict MS-PST reader rejects the file on the magic-client check
before it ever reaches the page layer.

## Page layout — 4096 bytes

```
  0x000 ┌──────────────────────────────────────┐
        │ rgentries[cEntMax] — cEntMax * cbEnt │  4056 bytes
  0xFD8 ├──────────────────────────────────────┤
        │ cEnt      u16   (u8  in MS-PST)      │
        │ cEntMax   u16   (u8  in MS-PST)      │
        │ cbEnt     u8                         │
        │ cLevel    u8                         │
        │ padding   10 bytes, zero             │
  0xFE8 ├──────────────────────────────────────┤
        │ PAGETRAILER — 24 bytes, see below    │
  0x1000└──────────────────────────────────────┘
```

- Page size **4096** (MS-PST: 512).
- `cEnt` / `cEntMax` widened to **u16** (MS-PST: u8).
- BTPAGE header sits at **0xFD8**, is 16 bytes, 10 of them zero padding.
- Entries occupy exactly `169 * 24 = 4056` bytes for internal pages.

### PAGETRAILER at 0xFE8 — 24 bytes

| Offset | Size | Field | Note |
|---|---|---|---|
| +0x00 | 1 | `ptype` | 0x81 NBT, 0x80 BBT, 0x84 AMap, … as MS-PST |
| +0x01 | 1 | `ptypeRepeat` | equals `ptype` |
| +0x02 | 2 | `wSig` | |
| +0x04 | 4 | `dwCRC` | |
| +0x08 | 8 | `bid` | matches the BREF that pointed here |
| +0x10 | 8 | reserved | observed `0x0000000000000001` |

MS-PST's Unicode PAGETRAILER is the same first 16 bytes; v36 appends 8 bytes.

## Verified NBT descent

Root at `ib 0x6C963000`, following entry[0] at each level:

| Level | ib | ptype | cLevel | cEnt | cEntMax | cbEnt |
|---|---|---|---|---|---|---|
| 0 | 0x6C963000 | 0x81 | 2 | 4 | 169 | 24 |
| 1 | 0x6C962000 | 0x81 | 1 | 109 | 169 | 24 |
| 2 | 0x6C90C000 | 0x81 | 0 | 124 | 126 | 32 |

- Internal `BTENTRY` = 24 bytes: `btkey u64`, `BREF { bid u64, ib u64 }` —
  identical to MS-PST Unicode.
- Leaf `NBTENTRY` = 32 bytes: `nid u64`, `bidData u64`, `bidSub u64`,
  `nidParent u32`, `dwPadding u32` — identical to MS-PST Unicode.

First leaf entries, all standard NIDs:

| nid | nidType | bidData | bidSub | meaning |
|---|---|---|---|---|
| 0x00000021 | 0x01 | 0x6CCBAA4 | 0 | `NID_MESSAGE_STORE` |
| 0x00000061 | 0x01 | 0x6C8CB20 | 0x6C8CB26 | `NID_NAME_TO_ID_MAP` |
| 0x00000122 | 0x02 | 0x582C84 | 0x6CCA322 | `NID_ROOT_FOLDER` |
| 0x0000012D | 0x0D | 0x67514 | 0 | special |
| 0x0000012E | 0x0E | 0x8 | 0x16 | special |
| 0x0000012F | 0x0F | 0x18 | 0 | special |

## Block payload compression — zlib

This is not in MS-PST at all and is the one delta that silently breaks an
otherwise-correct reader: the block parses as garbage rather than failing loudly.

`NID_MESSAGE_STORE`'s data block began `78 9C 6D 91 4F 48 ...`. `78 9C` is a
zlib header (deflate, default level). Inflating gave a valid HN whose `bSig` is
`0xEC` and which parses as a property context with 26 properties. Before
inflating, `bSig` read `0x6D` — the third byte of the zlib stream.

- `cb` in the BBTENTRY is the **stored (compressed)** size. In the observed case
  `cb` = 483 while the inflated payload was 654 bytes.
- Compression is **per block**, not per file. Many blocks are stored raw: the
  root folder's PC and the root hierarchy TC both parsed with no inflate.

### There is no reliable compression flag in the BBTENTRY

`dwPadding` was the obvious candidate and it does **not** work. Tallying
`dwPadding` against the presence of a zlib header across 200,000 blocks:

| `dwPadding` | zlib header | blocks |
|---|---|---|
| 0x00000000 | no | 238 |
| 0x00000000 | yes | 11 |
| 0x00000001 | no | 219 |
| 0x00000001 | yes | 150 |
| 0x00000002 | no | 47,457 |
| 0x00000002 | yes | 150,299 |
| 0x00000003 | no | 106 |
| 0x00000003 | yes | 457 |

Every `dwPadding` value carries both compressed and uncompressed blocks, so it
is not the flag. Whatever `dwPadding` means here, it is not "compressed".

**Working approach:** sniff the zlib header and require a successful inflate.
Validate the two-byte header properly (`b[0] & 0x0F == 8` and
`(b[0] << 8 | b[1]) % 31 == 0`) rather than hardcoding `78 9C`, then fall back to
the raw bytes if inflate fails. This is a heuristic, and it is the one part of
the implementation that deserves a real corpus test — a raw block whose first two
bytes happen to form a valid zlib header would be misread. In practice the
inflate itself is the check: a false positive almost always fails to inflate.

## Block size — 64 KB, and the HID split moves with it

MS-PST caps a block payload at **8,176** bytes (8,192 minus a 16-byte
BLOCKTRAILER). v36 raises it: every block of a large heap measured exactly
**65,512** bytes = 65,536 − 24, consistent with the 24-byte trailer seen on
pages. The largest contents table in the sample file spans **304** such blocks,
19,867,982 bytes of heap.

That change propagates into the LTP layer, which is otherwise unchanged. A HID
is still `hidType` in bits 0–4, then `hidIndex`, then `hidBlockIndex` — but the
boundary between the latter two is **not** where MS-PST puts it:

| | `hidIndex` | `hidBlockIndex` |
|---|---|---|
| MS-PST | bits 5–15 (11 bits) | bits 16–31 |
| **v36** | **bits 5–18 (14 bits)** | **bits 19–31** |

This is the one delta that a single-block heap cannot reveal: when
`hidBlockIndex` is 0 both layouts decode identically, which is why every small
property context and every message in the file parses correctly under the
MS-PST split. It only surfaces on a heap spanning hundreds of blocks.

**How it was pinned down.** Two contents tables would not open — Deleted Items
(`hidUserRoot` 0x03484520, 304-block heap) and Calendar (0x04B828C0, 207
blocks). Under the MS-PST split they resolve to block 840 and block 1208, both
far out of range. Two hypotheses:

1. *A 65,512-byte physical block is subdivided into MS-PST-sized logical heap
   pages.* **Disproved.** Probing for an `ibHnpm` at 8,176- and 8,192-byte
   strides inside block 0 gave incoherent values (`26161, 107, 77, 24050`),
   while block 0's own HNHDR is valid (`ibHnpm` 63,286, `bSig` 0xEC,
   `bClientSig` 0x7C). One physical block is one heap page.
2. *The bit split differs.* Confirmed. Trying every boundary from bit 16 to bit
   22 and requiring the resolved item to begin with TCINFO's `0x7C`, **only
   bit 19 resolves at all**, and it yields a valid TCINFO on both files:

```
Deleted Items  split@bit19: blk=105 idx=553 cAlloc=575  item=678 bytes, first=0x7C
Calendar       split@bit19: blk=151 idx=326 cAlloc=496  item=854 bytes, first=0x7C
```

The widths are self-consistent: a 14-bit `hidIndex` addresses up to 16,383
allocations (a 65,512-byte page's map holds at most ~1,111), and a 13-bit
`hidBlockIndex` allows 8,191 blocks ≈ 536 MB of heap.

## Contents-table columns — measured, not assumed

This is not a format delta; it is what a v36 contents table actually contains,
and it caught out two rounds of implementation. Measured on the Inbox contents TC
(6,781 rows, 85 columns, `rowSize` 381) by reading every column and comparing it
against the same property on the message node itself, sampling 68 rows.

**Every column agrees with the message node except one.** Subject, message class,
flags, importance, all three timestamps, size, DisplayTo/Cc, conversation topic,
internet message id, priority and the boolean columns matched on every sampled
row. The reader is right about the row matrix, the column descriptors and the HID
split.

The exception is sender identity:

| Property | In the table? | Usable? |
|---|---|---|
| 0x0C1A `PidTagSenderName` | **no such column** | — |
| 0x0C1F `PidTagSenderEmailAddress` | **no such column** | — |
| 0x0E1B `PidTagHasAttachments` | **no such column** | — |
| 0x0042 `PidTagSentRepresentingName` | yes, `PT_UNICODE`, cb 4 | **no** — see below |
| 0x0C19 `PidTagSenderEntryId` | yes, `PT_BINARY` | **yes**, resolves on 100% of rows |

So `has_attachments` comes from `PidTagMessageFlags` bit 4 (`MSGFLAG_HASATTACH`),
and sender identity comes out of the EntryID.

### The 0x0042 column is present but its cells are not HNIDs

Every row has a non-zero value in the 0x0042 cell and its cell-existence bit is
set, so the cell looks live. It is not:

- Roughly half the values (3,323 of 6,781 in the Inbox) fail to resolve at all —
  `hidIndex` runs to ~933 in blocks whose allocation map holds ~470 entries.
- The half that *do* resolve return **garbage**: of 68 sampled rows, **zero**
  matched the message node's own `PidTagSentRepresentingName`. The values that
  came back were other properties' items — body text, a message class, a
  DisplayTo list, and outright mojibake.
- The values are not subnode NIDs and not NBT nodes either (0 of 3,323 hits in
  both).
- The true name *is* in the heap. For one row whose real sender is
  `"Ritu Ved (rved)"`, that exact 30-byte allocation exists at block 22, index
  289 — HID `0x00B02420` — while the cell holds `0x00206E20`. Scanning all 381
  bytes of each row for a HID that resolves to the correct name finds nothing.

Conclusion: **do not read 0x0042 from a v36 contents table.** Whatever those four
bytes are, they are not a heap or subnode reference. Reading them yields wrong
sender names on live data, which is worse than reading none — the first attempt
here populated 23,026 rows with values that looked plausible and were fabricated.

### Sender identity from the EntryID

`PidTagSenderEntryId` resolves for every row and, when the sender is an internet
address, is a **One-Off EntryID** (MS-OXCDATA 2.2.5.1, provider UID `muidOOP`)
that spells out both fields inline:

```
flags(4) muidOOP(16) Version(2) Flags(2) DisplayName\0 AddressType\0 Address\0
```

Strings are UTF-16 when `Flags & 0x8000`. Parsing that gives name and address
with **no wrong values in any sampled row** across Inbox, Sent Items and Deleted
Items — but only ~50% coverage, because an Exchange sender's EntryID uses
`muidEMSAB` and carries an X500 DN with no display name in it:

```
/o=ExchangeLabs/ou=Exchange Administrative Group (FYDIBOHF23SPDLT)/cn=Recipients/cn=<hash>
```

Sent Items is 0% covered for exactly this reason: the sender is always the
mailbox owner, an Exchange user. Across the whole store 16,556 of 42,023 rows get
a sender in bulk; the message node has it for the rest, at the cost of a property
context open per message.

### Cell existence bitmap

Honour it. `rgib[TCI_1b]` (TCINFO +6) is where a row's bitmap starts and
`rgib[TCI_bm]` (+8, the row stride) is where it ends; a column's bit is `iBit`
from `TCOLDESC` +7, tested MSB-first. In the measured tables all 85 bits are set
on every row and absent values are zero-filled instead, so it changes no output
here — but a clear bit means the cell's bytes are stale, and a stale HID that
happens to resolve is indistinguishable from data without this check.

## Live-file access

Outlook does **not** prevent reading a live OST:

- Opening with `FileShare::ReadWrite` **succeeds** while Outlook holds the file.
- Outlook takes a **byte-range lock on bytes 0–1023** only (`LockFileEx`).
  Byte 1024 onward reads normally. The 564-byte HEADER falls entirely inside
  the locked range, so a naive reader dies on its very first read.
- A **memory-mapped view bypasses the byte-range lock entirely** — the header
  was read this way with Outlook running and writing.

Consequence for the implementation: I/O must go through an mmap-backed reader,
not `File::read`. `outlook-pst`'s entry point is
`open_store(path: impl AsRef<Path>)`, so it needs a reader-generic I/O path to
support this at all.

Torn reads remain possible since Outlook writes concurrently, but every page
carries `dwCRC`, so a torn page is **detectable** — validate the CRC and retry
the page on mismatch.

### BTree roots move while the file is live

Across two runs seconds apart, `BREFNBT` moved `0x6C963000` → `0x6C9B4000` →
`0x6C870000` and the node count rose 43,331 → 43,332. Outlook is writing
continuously and the BTrees are copy-on-write.

Consequences for a mount: re-read the header at the start of every session (never
cache page offsets across sessions), and treat a lookup miss as "the file moved
under me," not as corruption. Every page carries `dwCRC`, so torn reads are
detectable — validate and retry.

### Locating the store

The filename is not derivable. It is usually the account UPN plus `.ost`, but the
profile is the only authority, and the same directory holds a `.nst` Groups store
with the same stem — so guessing the name, or scanning for the largest file, both
pick the wrong thing on some machines.

Outlook writes the path it opened into the profile registry:

```text
HKCU\Software\Microsoft\Office\<ver>\Outlook
  DefaultProfile                = REG_SZ, the profile Outlook opens
  Profiles\<profile>\<service>
    001f6610                    = REG_BINARY, UTF-16 store path
    001f3001                    = REG_BINARY, UTF-16 account display name
```

`001f6610` is `PR_PROFILE_OFFLINE_STORE_PATH` and holds the OST for a cached
Exchange mailbox. Two things measured here contradict what the MAPI docs imply:

- **The strings are `REG_BINARY`, not `REG_SZ`** — NUL-terminated UTF-16, 146
  bytes for a 72-character path. A reader expecting `REG_SZ` finds nothing.
- **`<service>` is an opaque key name**, `507bb8f9…` on this machine, not the
  documented `9375CFF0413111d3B88A00104B2A6676\0000000x`. Both key shapes exist in
  the same profile, so the subtree has to be walked rather than indexed into.

The Groups store appears as `001f6610` too, on a `GroupsStore` subkey one level
below the mailbox service, pointing at a `.nst`. Filtering on the `.ost`/`.pst`
extension excludes it and also picks up added PSTs whatever tag they are filed
under, which avoids having to enumerate tag numbers at all.

## Verified end to end

A spike (`spike/src/main.rs`) reads the live 4.32 GB OST with Outlook running and
resolves the whole stack. Measured output:

- **BBT**: 534,813 block entries. **NBT**: 43,332 nodes.
- Node census: 42,093 `NORMAL_MESSAGE`, 95 `NORMAL_FOLDER`, 26 `SEARCH_FOLDER`,
  96 each of `HIERARCHY_TABLE` / `CONTENTS_TABLE` / `FAI_TABLE`.
- `NID_MESSAGE_STORE` PC: 26 properties (after inflate).
- Folder tree via hierarchy TCs: `Root - Public` and `Root - Mailbox`, recursing
  through `IPM_SUBTREE`, `NON_IPM_SUBTREE`, `EFORMS REGISTRY`.
- 94 named folders with item counts (Inbox 6,777; Deleted Items 7,312;
  Calendar 7,166).
- Inbox contents TC: 6,777 rows, 85 columns, `rowSize` 381 — subjects and
  timestamps read per row.
- A message PC: 256 properties; subject, sender name, sender address, recipient.
- `PidTagHtml` (0x1013): 8,944 bytes beginning `3C 68 74 6D 6C 3E` = `<html>`.
- Attachment table (subnode NID `0x671`) → attachment PC: `image.png`,
  `mime="image/png"`, 152,052 bytes of real payload recovered.

LTP *structures* are therefore confirmed unchanged: HNHDR/HNPAGEMAP, BTH
(`bType` 0xB5), PC (`bClientSig` 0xBC), TC (`bClientSig` 0x7C, TCINFO +
TCOLDESC + row matrix), subnode BTrees (SLBLOCK/SIBLOCK), and XBLOCK data trees
all parse per MS-PST. Only HID *decoding* differs, as above.

### Library results (`crates/ost`)

The spike was then ported to a library. Against the same live file, with Outlook
running:

- wVer 36, 4096-byte pages, 4,636,876,800 bytes mapped, 533,687 blocks,
  43,259 nodes.
- **121 folders** reachable from the root = 95 normal + 26 search, matching the
  node census exactly.
- **95/95** normal folders' contents tables read, **42,020 rows** total — equal
  to the number of `NORMAL_MESSAGE` nodes in the NBT, so every message is
  reachable through exactly one folder.
- **42,020/42,020 messages** parsed with zero failures, **18,604** attachments.
- Attachment payload round-tripped through the public API: `image.png`,
  `image/png`, 152,052 bytes opening `89 50 4E 47 0D 0A 1A 0A`.

## Remaining unknowns

- **BLOCKTRAILER layout is still unverified** — and turned out to be unnecessary.
  `cb` from the BBTENTRY gives the payload directly, so the spike never parses a
  block trailer. A writer, or CRC validation of blocks, would need it; a reader
  does not.
- Search folders (NID type 0x03) own no hierarchy table; deriving one from the
  node index fails. Only recurse into type 0x02.
- Sender name has no usable column in a contents TC; it comes from
  `PidTagSenderEntryId` for internet senders and from the message PC otherwise.
  See "Contents-table columns" above — that section is the measurement, and the
  four bytes in the 0x0042 cell remain unexplained. Contents-table column sets
  vary per folder; do not assume a fixed schema.
- Named properties (`NID_NAME_TO_ID_MAP`, 0x61) not yet parsed — needed for any
  property above 0x8000.
- Compressed-RTF bodies (`PidTagRtfCompressed`, 0x1009) not yet exercised; the
  sampled messages carried HTML instead.
- **No compression flag found** — the zlib sniff is a heuristic (see above).
- `PidTagDisplayName` on the message store node exists with HNID 0, i.e. an
  empty value, so the store has no readable display name. Fall back to the file
  stem.
- `NDB_CRYPT_PERMUTE` / `NDB_CRYPT_CYCLIC` are deliberately unimplemented, so
  most real *PST* files are refused at open. v36 OSTs use neither.
- **Bits 16–18 of a HID are unattributed.** `hidBlockIndex` demonstrably starts
  at bit 19 — the Inbox's 0x0037 column resolves for **6,781 of 6,781** rows
  under the bit-19 split and only 816 under MS-PST's bit-16 split, across a
  194-block heap — but whether the three bits below it extend `hidIndex` to 14
  bits or are reserved cannot be told from this file. The largest allocation map
  measured holds 1,198 entries, so every observed index is under 2,048 and those
  bits are zero in every sample. The reader masks
  them into `hidIndex`, which is correct under either reading as long as they
  stay zero — and a 65,512-byte page's allocation map can hold at most ~1,111
  entries, so an 11-bit index never overflows anyway.

## Build note for this machine — use MSVC via Build Tools 2022

Two Visual Studio instances are installed, and only one can link:

| Instance | Path | MSVC `lib` subdirs | `vcvarsall.bat` |
|---|---|---|---|
| VS Enterprise **2026** (18.8) | `C:\Program Files\Microsoft Visual Studio\18\Enterprise` | `onecore` only | **missing** |
| VS **Build Tools 2022** (17.14) | `C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools` | `arm arm64 arm64ec onecore x64 x86` | present |

The Windows SDK **is** installed and complete — `Windows Kits\10\Lib\10.0.26100.0`
has both `um\x64\kernel32.lib` and `ucrt\x64\libucrt.lib`.

The failure mode is narrow: a bare `cargo build` picks `link.exe` from the 2026
Enterprise MSVC (14.51.36231), whose `lib` directory contains only `onecore` — no
`x64` — so it dies with `LNK1104: cannot open file 'msvcrt.lib'`. Enterprise 2026's
desktop C++ workload is incomplete (its `vcvars64.bat` even calls a
`vcvarsall.bat` that does not exist).

**Build inside the Build Tools 2022 environment and MSVC works:**

```
cmd /c 'call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat" && cargo build --release'
```

Verified: links clean, 0.29 MB binary, and produces identical results on the live
OST (534,761 blocks / 43,324 nodes / 8,944-byte HTML body / 152,052-byte PNG).

Prefer MSVC for the real project — `duckdb` (bundled C++), `rmcp`, and tokio are
all better supported there than on windows-gnu.

The GNU toolchain also works as a fallback, but needs the full **toolchain**, not
just the target — build scripts such as `crc32fast` (under `flate2`) compile for
the host and need a working host linker:

```
rustup toolchain install stable-x86_64-pc-windows-gnu
cargo +stable-x86_64-pc-windows-gnu build --release
```

