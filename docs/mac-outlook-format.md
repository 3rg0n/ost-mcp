# Mac Outlook local storage — Phase 0 findings

Measured 2026-08-14 against a live Outlook for Mac identity (Microsoft Outlook
16.112.26081010, macOS, current build — commonly called "New Outlook"), account
type Exchange/Microsoft 365, on a Cisco-managed Mac. Every claim below states
what was counted and on what, per `CONTRIBUTING.md`. All examples use
`example.com` addresses and invented names; no real subject line, address,
display name or attachment payload appears here.

## Summary

There is no single "the" Mac Outlook store. There are **two independent
storage engines** sharing one identity directory, and which one holds an
account's actual mail depends on the account type and possibly on
organization policy:

1. **The classic engine** — `Outlook.sqlite` (SQLite, WAL mode) plus
   `.olk15*` companion files. This is the engine the issue's forum sources
   describe, and it is real: it is what several independent open-source
   tools (credited below) successfully read on other Outlook 15 profiles,
   extracting complete mailboxes.
2. **The Hx engine** — `HxStore.hxd`, an undocumented proprietary binary
   store (magic string `Nostromoi`), used by "New Outlook" for at least
   Exchange/M365-backed content sync. No public format documentation or
   parser is known to exist for it.

On the measured account, **the classic engine's content tables are empty**
(`Mail`, `Notes`, `Tasks`, `CalendarEvents` all 0 rows) while its structural
tables are partly real and partly inert placeholders (see below). The Hx
engine holds the actual mail content but its format is opaque. This is not a
timing artifact: it was re-checked after a full app quit + WAL checkpoint, and
again after manually opening several messages, with no change.

**Recommendation:** implement the Mac backend against the classic engine's
on-disk format (§3), since it is the one with a real, working, independently
verified format and it is what the issue's original hypothesis describes.
Have it degrade to empty/`NULL` — never a fabricated value — on an account
like the one measured here, where the classic engine's content tables are
unpopulated. Do not attempt to reverse-engineer `HxStore.hxd` (§2): no prior
art exists, the companion `.hfl` files show no exploitable structure, and per
§4, the accounts that would need it may have organization policies that
block local caching and export alike, independent of any parser quality.

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
| `HxStore.hxd` | 76 MB |
| `hxcore.hfl` | 50 MB |
| `hxcore_previous_session.hfl` | 50 MB |

`file(1)` reports all three as `data` — no recognized container format.
`sqlite3` rejects `HxStore.hxd` outright ("file is not a database"). First 16
bytes are the ASCII string `Nostromoi` followed by zero padding — a custom
magic header, not any documented format. Checked against ESE/JET (used by
Windows' equivalent Mail/Calendar/Outlook sync engine): the ESE signature
(`efcdab89` at offset 4) is absent; this is not an ESE store despite sharing
an engine lineage with the Windows side.

The file is page-sparse: bytes at offset 4096 are non-zero and structured;
offsets 8192, 16384 and 65536 are all-zero. This is consistent with a paged
allocator, not with a flat blob.

Extracting strings under a naive ASCII scan produces garbled fragments
(`Dpadd`, `Qdecor`, `PHyper`) that are explained by UTF-16LE-encoded text
being misaligned by single-byte scanning — i.e., HTML/CSS body content is
present in the file, consistent with real cached message bodies, but no
record boundary, length field or index structure was found bounding it.
`hxcore.hfl`'s first 64 bytes show no repeating structure and look
high-entropy, consistent with an encrypted or hashed companion file.

A `Files/` directory alongside `HxStore.hxd` (not the `Data/` tree) holds a
real, ordinary filesystem cache of attachments and inline images —
`Files/S0/<n>/Attachments/0/*`, 1,189 plain files with real extensions in the
measured sample (`.png`, `.jpg`, `.gif`, undecorated MIME-derived names).
These are directly readable with no format work; the missing piece is the
mapping from a message to its subdirectory, which was not resolved on this
account because no message row exists to map from (§1 of the Summary).

No further work is recommended here: the format is proprietary, undocumented,
has no known prior art, and the one exploitable lead (UTF-16 body fragments)
gives no way to delimit one message from the next.

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

No key was found that explicitly disables local content caching by name; the
empty `Mail` table cannot be attributed to this policy with certainty rather
than to New Outlook's Hx architecture running by default. Either way, the
practical conclusion is the same: **on a managed corporate device of this
kind, neither the live SQLite store nor `.olm` export may be available**,
independent of how complete a Mac backend's implementation is. A backend
should be written and tested against the classic engine (§3), which is real
and works on the accounts prior art demonstrates it working on, and should
report — not guess past — an account where it does not apply.

## 5. Resolution of the issue's Unknowns

| # | Question | Resolution |
|---|---|---|
| U1 | Read-only WAL access while Outlook runs | Both `mode=ro` and `mode=ro&immutable=1` succeed with Outlook running; no corruption; safe. Copy-to-scratch also works and was used throughout. |
| U2 | Schema: messages/folders/recipients/attachments, PK shape | `Record_RecordID` is an `INTEGER PRIMARY KEY ASC AUTOINCREMENT` (rowid alias) on every table. Folder hierarchy is `Folder_ParentID` (id reference), not a materialized path. See §3.1/§3.2 for full column detail. |
| U3 | Timestamp epoch/unit | Not confirmed against a real message row; `Record_ModDate` on real rows here is Unix seconds. Needs re-verification once a populated account is available. |
| U4 | Body location: column vs external file | External file, via `PathToDataFile`, confirmed structurally for every populated table. `.olk15MsgSource` is the "cheap path" the issue hoped for, but covers a minority of messages; `.olk15Message` is the 100%-coverage fallback and needs real parsing work, not just a MIME parser. |
| U5 | `.olk15` wrapper layout | Resolved for `Message` and `MsgAttachment` (§3.2), credited from external measurement, not yet independently re-verified against a real file from this account. |
| U6 | Attachments: payload location and linkage | `Blocks`/`Mail_OwnedBlocks` join (§3.2). Separately, a plain-file attachment cache exists under the Hx side too (§2), unlinked from any message on this account. |
| U7 | Rest of the message row | Present in the `Mail` schema: `Message_ReadFlag`, `Message_Size`, `Message_HasAttachment`, `Message_ThreadTopic`, `Message_type` (message class). See §3.1. |
| U8 | Discovery without hardcoding | Confirmed necessary twice over (§1). Glob `Outlook 15 Profiles/*/Data`. |
| U9 | Does New Outlook use this store at all | **No, not for content, on this account.** Structural tables (`Folders`, `Categories`, `Signatures`) are written by the classic engine; content (`Mail`, `Notes`, `Tasks`, `CalendarEvents`) is not, and appears to route through the undocumented Hx engine (§2) instead. This is New Outlook-specific (`IsRunningNewOutlook = 1` in `com.microsoft.Outlook` preferences) and was confirmed by direct evidence of a `sync.GetMessage` network call being logged (`Osa/OutlookServiceApiLogs_*/*.req.xmlgz`), not merely inferred from the empty table. |
