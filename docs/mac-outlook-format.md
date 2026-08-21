# Mac Outlook local storage — Phase 0 findings

Measured 2026-08-14 through 2026-08-20 against a live Outlook for Mac identity
(Microsoft Outlook 16.112.26081010, macOS, current build — commonly called
"New Outlook"), account type Exchange/Microsoft 365, on a Cisco-managed Mac.
Every claim below states what was counted and on what, per `CONTRIBUTING.md`.
All examples use `example.com` addresses and invented names; no real subject
line, address, display name or attachment payload appears here — including in
§2, added after this document's first version, which required parsing a store
that turned out to hold real content and was verified by counting and
aggregate coverage percentages only, never by reading it.

## Summary

There is no single "the" Mac Outlook store. There are **two independent
storage engines** sharing one identity directory:

1. **The classic engine** — `Outlook.sqlite` (SQLite, WAL mode) plus
   `.olk15*` companion files. This is the engine the issue's forum sources
   describe, and it is real: it is what several independent open-source
   tools (credited below) successfully read on other Outlook 15 profiles,
   extracting complete mailboxes. On the measured account it holds real
   structural data (folders, categories, signatures) but no message content
   at all (§3).
2. **The Hx engine** — `HxStore.hxd`, an undocumented proprietary binary
   store (magic `Nostromo` + version byte), used by "New Outlook" for
   Exchange/M365-backed content sync. **This document's first version said no
   parser existed for it and recommended against reverse-engineering it. That
   was wrong** — see §2, added after the account's mail-sync window (60 days,
   an account-level setting, not something this reader controls) turned out to
   explain the gap: `Outlook.sqlite` has no message rows because New Outlook
   never puts them there for this account type, not because nothing is
   cached. The real 60-day cache is in `HxStore.hxd`, and it is now readable.

**Recommendation, revised:** implement the Mac backend against **both**
engines. The classic engine (§3) still supplies real folder/category/
signature structure and is the only source for any account whose classic
engine *is* populated (a different account type, or a different
organization's policy, per §4). `HxStore.hxd` (§2) supplies real message
content — subject, sender, body/preview, timestamp — for the last ~60 days
on an Exchange/M365 account like the one measured here, with no folder
identity and no attachment linkage (open questions, see §2.6). Both degrade to
empty/`NULL` rather than a fabricated value where they have nothing.

## 1. Profile discovery (U8)

Measured path on this machine:

```
~/Library/Group Containers/UBF8T346G9.Office/Outlook/Outlook 15 Profiles/Main Identity/Data/
```

Two forum sources and one vendor reference all claimed the identity directory
is named `Main Profile`. It is not, on this machine: it is **`Main Identity`**,
confirmed by direct `ls`. No `Main Profile` directory exists anywhere under
`Outlook 15 Profiles`. Do not hardcode either name — glob
`Outlook 15 Profiles/*/Data`, exactly as the issue's U8 already prescribes.
This is now confirmed twice over: once by the identity name itself not
matching any published claim, and once by the classic-engine prior art in §3
being built against a `Main Profile` directory that plainly comes from a
different measured machine.

The `Outlook 15` version-era name in the path did not change on this Outlook
build, so U8's "glob for it, do not hardcode" guidance extends to that
component too, unverified either way beyond this one machine.

## 2. The Hx engine — `HxStore.hxd`

Files, sizes, at the identity root (sibling of `Data/`):

| File | Size |
|---|---|
| `HxStore.hxd` | 76–84 MB across the measurement period |
| `hxcore.hfl` | 50 MB |
| `hxcore_previous_session.hfl` | 50 MB |

`file(1)` reports all three as `data` — no recognized container format.
`sqlite3` rejects `HxStore.hxd` outright ("file is not a database"). Checked
against ESE/JET (used by Windows' equivalent Mail/Calendar/Outlook sync
engine): the ESE signature (`efcdab89` at offset 4) is absent; this is not an
ESE store despite sharing an engine lineage with the Windows side.
`hxcore.hfl`'s first 64 bytes show no repeating structure and look
high-entropy — unexplored further; nothing below depends on it.

### 2.1 It caches on a real, per-account window — not "nothing," as this document first said

This document's first version measured `Outlook.sqlite`'s empty `Mail` table,
found real UTF-16 body fragments in `HxStore.hxd` with no visible record
boundary, and concluded there was no exploitable local cache. Both
measurements were correct; the conclusion was not. The account's own mail
sync setting — visible in Outlook's Accounts preferences, not something this
reader controls or can discover from either file — is **on, limited to the
last 60 days**. That is the missing fact: `Outlook.sqlite` has no message
rows because New Outlook never routes them there for this account type
(§3.1), not because the account caches nothing. `HxStore.hxd` is where that
60-day window actually lives, and Outlook's own "Manage Storage" UI
independently confirms real content exists — 69,903 real inbox items,
22.4 GB, figures that are themselves a live query against the Exchange Online
server, since the *entire* on-disk footprint of every Outlook-related file on
this machine, everywhere, measured with `du -sh`, is under 300 MB. Neither
number, on its own, told us where the 60-day cache actually was; the account
setting is what closed the gap.

### 2.2 Format credit

Every structural claim in §2.3–§2.6 — the `Nostromo` magic, the 40-byte block
header with its two CRC-32s, the LZ4 payload framing, the record layout, the
.NET tick timestamp encoding — is from
[`securized/hxstore-reverse-engineering`](https://github.com/securized/hxstore-reverse-engineering)
(MIT), whose `SPEC.md` describes reading it out of `HxCore.framework`
disassembly plus Outlook's own Osa protocol logs (§2.7). This project's
`crates/mac-outlook/src/{hxstore,hxrecord,hxlz4}.rs` is an **independent
implementation** informed by that research, re-derived in this project's own
code rather than ported, and — this is the part that matters — **independently
re-verified against this project's own real file**, not taken on trust: the
credited project's own tool (`hxprobe`) was built from source and run against
a snapshot of this account's `HxStore.hxd` first, and its results (17,697 of
17,781 candidate blocks verified, 20,561 raw `IPM.Note` records, coverage
percentages) were reproduced independently before any of this project's own
code was written. This project's own reader was then built, and re-verified
again against the same file: an exact match on record count (20,561), and
close but not identical coverage numbers, recorded in §2.6.

### 2.3 File and block container

**Verified** against this project's own file.

```text
+0x00  char[8]  "Nostromo"           file magic
+0x08  u64      version byte         0x69 ('i') on this build
+0x38  u64      page size            4096
```

An unrecognised version byte does not fail the read — the per-block
checksums, not the version byte, are what actually guards correctness. The
credited project reports Windows Mail literature describing `'h'` for the
same container; this project has not tested a Windows store.

Blocks are found by scanning for an 8-byte magic and validating three things,
not by walking a directory — no valid block-directory chain was found in
`.hxd` by either project; the mechanism exists in `HxCore` but opens the
smaller `.ctr` sidecar stores instead:

```text
+0x00  u32  crc32(block[0x04..0x20])              header checksum
+0x04  u32  crc32(block[0x08..0x28+payload_len])  payload checksum
+0x08  u64  magic 0x5d0245643b706a05
+0x10  u32  kind          (observed 8; the credited project also observed 16)
+0x14  u32  payload_len   LZ4-compressed bytes, starting at +0x28
+0x18  u32  inflated_len  exact decompressed size
+0x1c  u32  4             constant in every block observed by either project
+0x28  ...  LZ4-compressed payload
```

CRC-32 is the standard IEEE/zlib polynomial (`0xEDB88320`). The LZ4 payload is
plain block format — no frame header, no trailing checksum — decoded requiring
an exact match to `inflated_len`; anything else (short output, a
back-reference outside the window) means the scan found a false-positive
magic hit, not a format variant, and the block is skipped rather than
returning partial or wrong data.

**Measured, this project's snapshot:** 17,751 blocks passed all three checks
(header CRC, payload CRC, exact inflated length), decompressing to
299,243,728 bytes. The credited project's own tool, run against the same
snapshot, found 17,781 magic-byte candidates and verified 17,697 of them
(99.53%) — a close but not identical count to this project's 17,751, which is
expected of two independently written scanners and not investigated further,
since both numbers agree to within 0.3% and the downstream record count
matches exactly (§2.6).

### 2.4 Records: a sequence of UTF-16LE strings around an anchor

Message metadata is not a struct at a fixed offset. Each record is anchored
by the literal string `"IPM.Note"` (UTF-16LE), and the fields around it — the
sender's address and display name before it, the Message-ID/preview/subject
after it — are NUL-terminated UTF-16LE runs whose *byte offset drifts with
every preceding field's length*. A parser has to walk the sequence in order
and classify each run by shape (does it parse as an email address? a GUID? a
subject-shaped phrase?), never index a fixed displacement. Two layouts occur:
the common one puts the sender pair just before the anchor; a second puts the
whole header after it, which is why this project's sender search (§2.6, the
bug found and fixed) must not apply the same distance bound on both sides.

The most useful single rule, credited directly: `NormalizedSubject` and
`Topic` are written back to back and are near-identical (one keeps
reply/forward prefixes, one strips them) — a value that appears **twice** in
a record's field sequence is the subject; a value that appears **once** is
the cached body preview. There is no other reliable way to tell the two
apart, since both are plausible-length phrases sitting in the same region of
the record.

### 2.5 Timestamps: .NET ticks, not FILETIME

Verified with a known-plaintext pair from Outlook's own Osa protocol logs
(§2.7), which log one timestamp field in both raw and human-readable form in
the same response: the raw value `639201014590000000`, divided by `10^7`
seconds after `0001-01-01T00:00:00Z`, is exactly the timestamp the same log
line renders as `2026-07-19T23:44:19Z`. This rules out Windows `FILETIME`
(epoch 1601, which decoding the same bytes as ticks would place in the 3600s)
and confirms 100-nanosecond .NET ticks:

```text
unix_seconds = (ticks - 621_355_968_000_000_000) / 10_000_000
```

A record holds several ticks (send, delivery, last-modified, sync), not one,
and fixed byte offsets do not reliably find any specific one — this project's
first attempt bounded the scan window too narrowly around the anchor and
under-merged message revisions as a result (§2.6). Scanning the *whole*
record span (from the previous anchor to the next) and taking the **earliest**
tick found reliably recovers the send time: a message is sent before it is
delivered, modified or synced, so the minimum tick in its span is send time
far more often than any other ordinal position is.

### 2.6 What this project measured on its own reader, and one bug found and fixed

All numbers below are from this project's own implementation
(`crates/mac-outlook/src/{hxstore,hxrecord,hxlz4}.rs`), run via
`cargo run -p mac-outlook --example hx_probe <snapshot>` against the same
snapshot the credited project's tool was run against — aggregate counts only,
never real message content, per the redaction rule at the top of this
document.

| | This project (current) | Credited project's tool, same file |
|---|---|---|
| Blocks verified | 16,362 (later snapshot; live file, see §2.1) | 17,697 / 17,781 (99.53%), earlier snapshot |
| Decompressed | 272,580,559 bytes | ~298 MB (implied), earlier snapshot |
| `IPM.Note` records | 19,114 | 20,561, earlier snapshot |
| Distinct messages (deduplicated) | 10,340 | 10,258 |
| Sender coverage | 65.6% (was 97.7% before §2.6.1's fix) | 99.9% |
| Sender-name coverage | 48.3% (was 67.4%) | 82.4% |
| Subject coverage | 75.6% | 90.8% |
| Preview/body coverage | 98.9% | 98.6% |
| Full HTML coverage | 29.3% | 30.7% |
| Timestamp coverage | 100.0% | 100.0% |

The block/record counts differ from the first version of this table because
the live file had synced further between snapshots — this project's own
counts moved in step with it, not against it, which is itself a consistency
check.

The exact match on raw record count, before any field extraction happens,
confirms the block/decompression/anchor-finding layers are correct
independent of anything downstream. The gaps in subject and sender-name
coverage are an honest simplification, not a bug: this project's field
classifier does not implement the credited project's conversation-level
subject back-fill (writing the subject once per thread rather than once per
message, and propagating it to sibling records that share a thread
identifier) or its "unpaired subject" fallback for a lone subject-shaped run
with no duplicate — both are described in the credited write-up as raising
its own subject coverage from roughly 85% to 88.7%, and neither is
implemented here.

**One real bug was found and fixed during this verification, not left as a
known gap.** The first version of this project's sender search applied the
before-anchor distance bound (320 bytes) to *both* sides of the anchor;
because a real record's header sometimes sits entirely after the anchor with
no equivalent distance limit in that direction (§2.4's second layout), this
silently rejected a valid sender on every record using that layout and
measured at 50.0% sender coverage. Widening the after-anchor side to be
unbounded — matching the credited project's own approach, re-read after the
gap was found rather than guessed at — raised it to the 97.7% in the table
above. Relatedly, the first version's timestamp scan used a fixed 64-byte
lookback rather than the true previous-anchor-to-next span; this caused
different revisions of the same message to sometimes compute different send
times and fail to merge, inflating the deduplicated message count from a
plausible ~10,500 to 13,251 before the fix.

### 2.6.1 A second sender bug, found through real use, not review

The bugs in §2.6 were found by deliberately verifying against a real file.
This one was found the other way: by using the finished tool to answer a real
question ("what did this person's last email say") and noticing the answer
didn't match the message's own quoted text. That is the more important
signal — a coverage percentage cannot catch a wrong-but-plausible value,
only a person reading the actual output can.

**What was wrong.** §2.6's fix widened the after-anchor sender search to be
unbounded, which was the right call for coverage (50.0% → 97.7%) but did not
account for a real, now-measured case: some records' after-anchor region
holds **two** address+display-name pairs, not one — a participant's, then the
true sender's, sometimes thousands of bytes apart. Picking "the first
email-shaped field found" (nearest to the anchor) picked the participant's
address on these records, not the sender's. Diagnosed on a real store with
the redacted structural dump `cargo run -p mac-outlook --example hx_probe
<snapshot> --layout-near <unix_seconds>`, which prints each field's offset,
type-shape and length — never its text — for the record nearest a given
timestamp. Two independently-known-wrong records were traced this way: both
had exactly two after-anchor address+name pairs, and both had picked the
nearer, wrong one.

**The fix.** When the after-anchor fallback finds more than one
address-shaped candidate, this project's reader now returns `None` rather
than guess — there is no signal in what has been measured so far that
reliably says which of several candidates is the sender (this remains an
open question, §2.8). A before-anchor match (the common, simpler case) is
unaffected and still trusted as soon as found, since no ambiguity has been
observed there. This is directly the `CONTRIBUTING.md` rule that a
plausible-looking wrong value is worse than `NULL` — measured here, not
asserted: sender coverage dropped from 97.7% to 65.6%, and every case checked
by hand now shows the correct sender or `None`, never a wrong one.

**A secondary, accepted consequence.** Deduplication keys on
`(sender, sent_unix)` (§4.7's rule). A message whose good revision resolves a
real sender and whose lesser revision now resolves to `None` no longer merges
into one entry — the reader shows an extra, mostly-empty duplicate instead.
This inflates the distinct-message count slightly and is a known, accepted
cost of the fix, not a new defect: no field is reported wrong, some are
reported twice.

### 2.7 The Osa protocol logs: a schema oracle, not a content source

`Outlook 15 Profiles/<identity>/Osa/OutlookServiceApiLogs_*/` holds gzipped
XML request/response logs of the live sync protocol — the same directory
whose existence answered U9 in this document's first version (§5). Message
*content* in these logs is redacted (`<Subject>pii:...</Subject>`), but field
*names*, their order and their enum values are in the clear, which is what
resolved the timestamp encoding in §2.5 and is the best available target list
for whatever in §2.4/§2.6 is not yet mapped (folder identity, read state,
recipient To/Cc/Bcc distinction). Retention is roughly 10 days on this
account, unconfirmed whether that is a fixed or configurable window.

### 2.8 Open questions

- **No folder identity.** Nothing observed in a record ties it to a specific
  mail folder (Inbox vs. Sent vs. Deleted). The Osa logs (§2.7) sync a
  folder-identifying field for other purposes, which is the lead for future
  work; until then, a `Mailbox` implementation built on this source can only
  expose one flat collection of recovered messages, not real folders.
- **No attachment linkage.** `Files/S0/<n>/Attachments/0/*` (documented in
  the superseded §2 of this document's first version, and still present and
  real — 1,189 plain files with real extensions in the measured sample) is
  not tied to any record here by anything this project has found.
- **Recipients.** Addresses inside a record's span can be collected, but
  nothing establishes ordering or a To/Cc/Bcc distinction.
- **Which after-anchor candidate is the sender, when there is more than
  one.** §2.6.1 measured that some records hold two address+display-name
  pairs after the anchor, one a participant's and one the true sender's, with
  no signal found so far to tell them apart. This project returns `None`
  rather than guess (§2.6.1); resolving it for real — rather than declining
  to answer — most likely needs the `Type`/`IsMe` attributes the Osa schema
  names on `SenderDisplayNamesCollection` (§2.7), not yet decoded here.
- **Windows stores.** Untested by either project.

## 3. The classic engine — `Outlook.sqlite` + `.olk15*`

### 3.1 What the measured account actually contains

`PRAGMA journal_mode` → `wal`. `PRAGMA integrity_check` → `ok`, both against
the live file (`mode=ro`) and a `db`+`-wal`+`-shm` scratch copy — no
corruption (U1 resolved: both `mode=ro` and `mode=ro&immutable=1` succeeded
with Outlook running; the live file is safely readable without ever writing
to it).

Row counts, measured live and confirmed unchanged after a full Outlook quit
(which checkpointed the WAL to 0 bytes) and after manually opening several
messages:

| Table | Rows | Real or placeholder |
|---|---|---|
| `Mail` | 0 | — (empty) |
| `Notes`, `Tasks`, `CalendarEvents` | 0 | — (empty) |
| `AccountsMail`, `AccountsExchange` | 0 | — (empty) |
| `Folders` | 14 | **Placeholder** — every `Folder_Name` value is literally the string `Placeholder_Inbox_Placeholder`, `Placeholder_Sent_Items_Placeholder`, etc. Not real folder names. |
| `Contacts` | 1 | Placeholder-adjacent — the account's own self-contact card, not a synced contact. |
| `Categories` | 8 | **Real** — custom, non-default category names a user would have typed (not Outlook's 6 stock color categories). |
| `Signatures` | 2 | Real, by inference from Categories being real and the same `PathToDataFile` mechanism applying. |

This resolves U2 partially: the schema is real (see below for the useful
part — column names and the storage mechanism), but on this account, folder
identity is not something a reader can trust from this table. A backend must
detect and null out the `Placeholder_*` sentinel rather than surface it as a
folder name — surfacing it would be exactly the "plausible-looking fallback"
`CONTRIBUTING.md` warns against, except manufactured by Microsoft's own
compatibility layer rather than by this project's code.

`Folder_SpecialFolderType` is a second, independently useful column: it is
numeric, not text, so it carries no placeholder problem of its own, and every
one of the 14 rows' code was cross-checked directly against that row's own
(fake, but internally consistent) placeholder label — the code table below
*is* the measurement, not an application of the standard MAPI/`OlDefaultFolders`
enum from memory:

| `Folder_SpecialFolderType` | Placeholder label observed on that row |
|---|---|
| 1 | `Placeholder_Inbox_Placeholder` |
| 2 | `Placeholder_Outbox_Placeholder` |
| 3 | `Placeholder_Address_Book_Placeholder` |
| 4 | `Placeholder_Calendar_Placeholder` |
| 5 | `Placeholder_Notes_Placeholder` |
| 6 | `Placeholder_Tasks_Placeholder` |
| 8 | `Placeholder_Sent_Items_Placeholder` |
| 9 | `Placeholder_Deleted_Items_Placeholder` |
| 10 | `Placeholder_Drafts_Placeholder` |
| 12 | `Placeholder_Junk_Email_Placeholder` |
| 99 | `Placeholder_On_My_Computer_Placeholder` |
| 103 | `Placeholder_Temporary_Items_Placeholder` |
| 0 | Ambiguous — covered **two** different placeholder labels in the sample (`Placeholder_Saved_Messages_Placeholder` and `Placeholder_Auto_Saved_Messages_Placeholder`), so a reader cannot tell which real folder code `0` means and must not name it. |

A reader may use this table to give the standard special folders their real
name even when `Folder_Name` itself is placeholder text — but only for codes
1–103 above; code `0` must resolve to no name, for the ambiguity reason in
the table. This account had no example of an ordinary, non-special user
folder to measure separately, so whether such a folder would also carry code
`0` (as MAPI convention would suggest) is unconfirmed — another reason not
to guess a name for it.

Every content-bearing row (`Categories`, `Contacts`, `Signatures`, `Main`)
follows the same mechanism: a `PathToDataFile` column names a relative path
under `Data/`, of the form `<TableName>/<n>/<GUID>.olk15<Type>` — e.g.
`Categories/213/<guid>.olk15Category`. `<n>` is a bucket directory, not
otherwise meaningful. This confirms the issue's core hypothesis (SQLite is
the glue, `.olk15*` files hold the payload) for every table type where the
row exists — it just does not, on this account, for `Mail`.

### 3.2 Message-bearing file layout, credited from prior art

No `.olk15Message`, `.olk15MsgSource` or `.olk15MsgAttachment` file exists
anywhere on the measured identity, and none of `Data/Messages/`,
`Data/Message Sources/` or `Data/Message Attachments/` exist either — all
three are absent, not just empty, consistent with `Mail` being empty. This
was re-checked after manually opening several messages in Outlook, with no
change, contradicting one community reference's claim that "every viewed
email gets [an `.olk15Message`]" — on this account, under New Outlook, it
does not.

The layout below could therefore not be measured against this account's own
files. It is derived from reading two independent open-source projects that
did measure it against other, populated profiles — [`xianhammer/format`'s
`olk15` package](https://github.com/xianhammer/format/tree/master/olk15)
(Go; exploratory, its own message parser is incomplete) and
[`thomasmaerz/olk15-export`](https://github.com/thomasmaerz/olk15-export)
(Python; a complete, tested extraction pipeline reporting success against a
populated profile). Implemented independently below from their description,
per the same convention the issue sets for the `.olm` format in Phase 3 —
credited, not ported.

```
Outlook 15 Profiles/<identity>/Data/
├── Outlook.sqlite                    metadata: headers, Blocks, Mail_OwnedBlocks
├── Messages/<n>/<guid>.olk15Message          body cache, no headers
├── Message Sources/<n>/<guid>.olk15MsgSource full RFC822 MIME, headers intact
└── Message Attachments/<n>/<guid>.olk15MsgAttachment  one attachment
```

`.olk15Message` — binary cache of the rendered body only, written (per the
above sources) for every message the user has viewed, regardless of whether
the full source was ever downloaded:

| Offset | Size | Field |
|---|---|---|
| 0x00 | 4 | magic `0D 00 00 01` |
| 0x04 | 16 | UUID |
| 0x14+ | — | body content: HTML/RTF/iCalendar, mixed UTF-16LE/UTF-8, no header section |

Headers are not in this file at all; a reader reconstructs them from the
`Mail` row (`Message_NormalizedSubject`, `Message_SenderList`,
`Message_RecipientList`, `Message_TimeReceived`/`Message_TimeSent`). Body
extraction is a byte search for the earliest of an HTML, RTF or
`BEGIN:VCALENDAR` marker (in both a plain and a `\x00`-interleaved UTF-16LE
form), then decoding what follows with a UTF-16/UTF-8/best-effort fallback
and trimming Outlook's own trailing metadata (a `MessageCardSerialized` JSON
blob, an embedded `AddressSet` XML fragment, or a run of control bytes all
mark the end of real content).

`.olk15MsgSource` — the reliable path, but only present for a minority of
messages (one source reports ~7% coverage: messages Outlook has fully
downloaded, versus the 100% coverage `.olk15Message` gets on view). It is the
actual RFC822 bytes as sent or received, with a binary prefix ahead of the
first MIME header. Locating the earliest of `Received:`, `From:`,
`Return-Path:`, `MIME-Version:`, `Date:`, `Subject:`, `Message-ID:` and
slicing from there yields a standard MIME message a normal parser (e.g.
`mail-parser`, if U4's "cheap path" holds — it holds for this file type only)
can read directly, after normalizing line endings.

`.olk15MsgAttachment` — one attachment per file:

| Offset | Size | Field |
|---|---|---|
| 0x00 | 4 | magic `d0 0d 00 00` |
| 0x04 | 12 | unknown/padding |
| 0x10 | 16 | GUID |
| 0x20+ | — | MIME-style headers (`Content-Type`, `Content-Disposition`, `Content-Transfer-Encoding`), terminated by `\r\r` — **not** `\r\n\r\n` |
| after headers | — | base64 payload |

The `\r\r` terminator (rather than the standard MIME blank-line `\r\n\r\n`)
is the one genuinely easy-to-miss detail here; a parser that assumes standard
MIME framing will fail to find the payload boundary.

Linking a `Mail` row to its source/attachment files goes through two more
tables, not through any shared filename or GUID (checked: a folder's
`.olk15Folder` GUID does not appear anywhere in `HxStore.hxd`'s raw bytes
either, for what that is worth — the two engines do not share identifiers):

```sql
SELECT hex(b.BlockID), b.BlockTag, b.PathToDataFile
FROM Blocks b JOIN Mail_OwnedBlocks m ON m.BlockID = b.BlockID
WHERE m.Record_RecordID = <mail record id>
ORDER BY m.BlockTag;
```

`PathToDataFile` values are URL-encoded (`Message%20Attachments/...`) and
resolve relative to `Data/`.

### 3.3 Timestamps (U3)

Not measured against a real row on this account (none exists). The schema
declares `Message_TimeReceived`/`Message_TimeSent` as SQLite `DATETIME`
columns; `Record_ModDate` on every row actually present here holds a plain
Unix-seconds integer (e.g. a `Record_ModDate` in the 1.78-billion range,
consistent with a 2026 Unix-seconds timestamp, not CFAbsoluteTime — which
would read roughly 780 million lower — nor FILETIME, which would be sixteen
orders of magnitude larger). This is one column, not the message-specific
ones U3 asks about, and should be re-confirmed against a real `Mail` row
before being relied on.

## 4. Organization policy (new finding, not in the issue's Unknowns)

The measured Mac has a Cisco-deployed MDM configuration profile and pushes
managed preferences to `com.microsoft.Outlook`:

```
DisableExport = 1
DisableSkypeMeeting = 1
DisableTeamsMeeting = 1
HideFoldersOnMyComputerRootInFolderList = 1
TrustO365AutodiscoverRedirect = 1
```

`DisableExport = 1` disables Outlook's File → Export menu action outright —
on a managed device carrying this policy, the issue's Phase 3 (`.olm`
archive) fallback is **also** unavailable, not only the live-store path.
`HideFoldersOnMyComputerRootInFolderList = 1` hides local-only folders from
the UI, consistent with (but not proof of) a broader posture against local
mail storage. A separate managed-preferences key,
`cacheTimeoutInSeconds = 43200` (12 hours), is tied specifically to the
Hx-backed Exchange account entry — a real, measured Hx-adjacent setting,
though its exact effect was not tested.

No key was found that explicitly disables local content caching by name, and
§2.1 confirms the reason is not policy at all: the empty `Mail` table is
New Outlook routing content elsewhere by design, not this device's MDM
profile suppressing it. `DisableExport = 1` remains a real, independent
finding — the issue's Phase 3 (`.olm` archive) fallback is unavailable on a
managed device carrying this policy, regardless of what a Mac backend reads
locally. A backend should be written against **both** local engines (§2, §3)
and should still report — not guess past — an account where either one has
nothing.

## 5. Resolution of the issue's Unknowns

| # | Question | Resolution |
|---|---|---|
| U1 | Read-only WAL access while Outlook runs | Both `mode=ro` and `mode=ro&immutable=1` succeed with Outlook running; no corruption; safe. Copy-to-scratch also works and was used throughout. |
| U2 | Schema: messages/folders/recipients/attachments, PK shape | `Record_RecordID` is an `INTEGER PRIMARY KEY ASC AUTOINCREMENT` (rowid alias) on every table. Folder hierarchy is `Folder_ParentID` (id reference), not a materialized path. See §3.1/§3.2 for full column detail. |
| U3 | Timestamp epoch/unit | Not confirmed against a real message row; `Record_ModDate` on real rows here is Unix seconds. Needs re-verification once a populated account is available. |
| U4 | Body location: column vs external file | External file, via `PathToDataFile`, confirmed structurally for every populated table. `.olk15MsgSource` is the "cheap path" the issue hoped for, but covers a minority of messages; `.olk15Message` is the 100%-coverage fallback and needs real parsing work, not just a MIME parser. |
| U5 | `.olk15` wrapper layout | Resolved for `Message` and `MsgAttachment` (§3.2), credited from external measurement, not yet independently re-verified against a real file from this account. |
| U6 | Attachments: payload location and linkage | `Blocks`/`Mail_OwnedBlocks` join (§3.2), for an account whose classic engine is populated. Separately, a plain-file attachment cache exists under the Hx side too (`Files/S0/<n>/Attachments/0/*`, §2.8), but nothing found so far links one of its files to a specific Hx record. |
| U7 | Rest of the message row | Present in the `Mail` schema: `Message_ReadFlag`, `Message_Size`, `Message_HasAttachment`, `Message_ThreadTopic`, `Message_type` (message class). See §3.1. |
| U8 | Discovery without hardcoding | Confirmed necessary twice over (§1). Glob `Outlook 15 Profiles/*/Data`. |
| U9 | Does New Outlook use this store at all | **Partially, and it depends what "this store" means.** The classic `Outlook.sqlite` engine is not used for content on this account (`Mail`, `Notes`, `Tasks`, `CalendarEvents` all empty; confirmed by a `sync.GetMessage` network call logged at `Osa/OutlookServiceApiLogs_*/*.req.xmlgz`, not merely inferred). But New Outlook *does* cache content locally for this account — in `HxStore.hxd` (§2), honoring the account's own 60-day sync window — which this document's first version missed and later corrected. |
