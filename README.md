# ost-mcp

Query an Outlook mailbox file over MCP, in place.

```text
model  <-->  ost-mcp (MCP stdio + DuckDB)  <-->  .ost / Outlook.sqlite
```

Point it at an `.ost`, a `.pst`, or a Mac Outlook profile, and a model can search
it with SQL, read message bodies, and pull attachment payloads. Nothing is
exported, converted or indexed: DuckDB table functions read the store as each
query runs, the mapping is read-only, and **Outlook can stay open** while you
query.

It is a single binary in pure Rust with the DuckDB amalgamation compiled in, so
there is nothing to install alongside it.

## Why not export first

The usual approach converts a mailbox into an intermediate — an mbox tree, a
SQLite index, a folder of `.eml`. That copies every message you were trying to ask
one question about, goes stale the moment Outlook writes, and leaves a second
plaintext copy of your mail on disk. Mounting sidesteps all three, the way DuckDB
reads a Parquet file rather than loading it.

## Install

One line, in PowerShell:

```powershell
irm https://raw.githubusercontent.com/3rg0n/ost-mcp/main/install.ps1 | iex
```

That checks for a Rust toolchain and the MSVC build tools, activates the x64
toolchain so the linker and the CRT come from one Visual Studio install, builds the
binary with `cargo install`, drops the skill in `~/.claude/skills/ost-mcp`, then
opens your store and runs a query to show it works. The first build compiles DuckDB,
so allow a few minutes.

Read it before you run it. Piping a remote script into a shell is a decision, not a
default:

```powershell
irm https://raw.githubusercontent.com/3rg0n/ost-mcp/main/install.ps1 | more
```

Options need a scriptblock rather than a pipe:

```powershell
& ([scriptblock]::Create((irm https://raw.githubusercontent.com/3rg0n/ost-mcp/main/install.ps1))) -InstallPrereqs
```

| Option | What it does |
|---|---|
| `-InstallPrereqs` | Installs a missing Rust toolchain or MSVC build tools with winget |
| `-SkillScope project` | Puts the skill in `.claude/skills` of `-ProjectPath` instead of your profile |
| `-SkipSkill` | Binary only |
| `-Force` | Reinstalls even when the same version is present, and ignores the MSVC check |
| `-Ref <branch>` | Installs from a branch other than `main` |

### Let an agent install it

Copy this to Claude Code, or any agent that can run PowerShell. It runs the
installer, fixes what fails, and checks the result.

```text
Install ost-mcp on this Windows machine and confirm it works.

1. Run this in PowerShell:
   irm https://raw.githubusercontent.com/3rg0n/ost-mcp/main/install.ps1 | iex

2. If it stops on a missing prerequisite, install what it names and run it again.
   To let it install the Rust toolchain and the MSVC build tools itself, run:
   & ([scriptblock]::Create((irm https://raw.githubusercontent.com/3rg0n/ost-mcp/main/install.ps1))) -InstallPrereqs

3. If the build fails with LNK1104 on a library such as msvcrt.lib, cargo picked
   the wrong linker. Re-run from a Developer PowerShell for VS 2022, or run
   vcvars64.bat first, so link.exe and the CRT come from one toolchain.

4. Check all three, and report the actual output of each:
   - `ost-mcp --list` names at least one .ost or .pst
   - `ost-mcp --info` prints a format version and a folder count
   - the file ~/.claude/skills/ost-mcp/SKILL.md exists

5. Then tell me to restart my Claude Code session so the skill loads.

This tool reads my real mailbox, read-only. While you check the install, do not
print, quote or save a subject line, a sender address or any message body. Counts,
the format version and the file size are the only evidence you need.
```

### From source

Needs a Rust toolchain and, on Windows, the MSVC build tools.

```sh
cargo build --release
```

The binary lands at `target/release/ost-mcp`. Use `cargo install --path
crates/ost-mcp` instead to put `ost-mcp` on your `PATH`, which is what the skill
expects.

## Use

```sh
ost-mcp                                # serve MCP over stdio on the discovered store
ost-mcp <file.ost>                     # serve MCP on a named OST/PST
ost-mcp <profile Data dir>             # serve MCP on a named Mac Outlook profile
ost-mcp [store] --list                 # list the stores that were discovered
ost-mcp [store] --info                 # path, backend kind, size, schema
ost-mcp [store] --sql "..."            # run one query, print JSON, exit
ost-mcp [store] --message <nid>        # one message: headers, recipients, body
ost-mcp [store] --attachments <nid>    # attachment metadata for a message
ost-mcp [store] --attachment <m>:<a>   # one payload, as text or base64
ost-mcp [store] --attachment <m>:<a> --out f   # write the payload to a file
```

Every MCP tool has a flag that prints the same JSON and exits, so the binary is
usable from a shell or a script without speaking the protocol. `--sql` covers
folder listing and search on its own, since both are queries over the same two
tables.

With no argument, the store is resolved from the Outlook profile registry on
Windows (`DefaultProfile` first) or from the Outlook group container on macOS —
rather than guessed from a filename or picked by directory scan. `--list` shows
which profile and account each store belongs to.

```sh
$ ost-mcp --sql "SELECT folder_path, count(*) FROM messages GROUP BY 1 ORDER BY 2 DESC LIMIT 3"
```

### As a Claude Code skill

Lower friction than an MCP server: no config file to edit and no restart. The
installer above does this for you; from a clone it is two commands.

```sh
cargo install --path crates/ost-mcp
cp -r skills/ost-mcp ~/.claude/skills/          # or .claude/skills/ in one project
```

The skill teaches the schema and the query patterns, then shells out to the flags
above. It costs nothing until a question actually needs the mailbox, where an MCP
server's tool definitions sit in the context of every turn. Set `OST_MCP_BIN` if
the binary is somewhere unusual. See [`skills/ost-mcp/SKILL.md`](skills/ost-mcp/SKILL.md).

### As an MCP server

```json
{
  "mcpServers": {
    "ost": {
      "command": "C:\\path\\to\\ost-mcp.exe"
    }
  }
}
```

Add the store path as an argument if you want a specific one:
`"args": ["C:\\path\\to\\archive.pst"]`.

## Tools

| Tool | What it does | CLI |
|---|---|---|
| `store_info` | Path, backend kind, folder count, schema | `--info` |
| `list_folders` | The folder tree with item and unread counts | `--sql` |
| `search` | Messages by text, folder, date range, attachment presence | `--sql` |
| `sql` | One read-only statement, for anything `search` cannot express | `--sql` |
| `get_message` | One message: headers, recipients, body, attachment list | `--message` |
| `list_attachments` | Attachment metadata without reading a payload | `--attachments` |
| `read_attachment` | One attachment's bytes, as text when they are text | `--attachment` |

Both surfaces build their replies from the same functions, so a tool and its flag
return identical JSON.

`sql` runs against a DuckDB with external access disabled, so a query can read the
mailbox but cannot reach the filesystem or the network.

## Schema

```sql
folders(nid, parent_nid, name, path, item_count, unread_count,
        has_subfolders, is_search_folder)

messages(nid, folder_nid, folder_path, subject, sender_name, sender_email,
         delivered, submitted, modified, size, unread, has_attachments,
         message_class)

ost_attachments(message_nid => <nid>)
  -> (message_nid, nid, filename, mime, content_id, declared_size, data_len)
```

`nid` is the handle for everything — a message's comes from a query, an
attachment's from `list_attachments`.

## Format support

The reader implements MS-PST's three layers (NDB, LTP, messaging) plus the
**undocumented format version 36** that Outlook 2013 and later write for cached
Exchange mailboxes: 4 KB pages, widened BTree page counts, a moved heap-id bit
split, and zlib-compressed block payloads with no flag announcing them. That work
is written up in [`docs/ost-v36-format.md`](docs/ost-v36-format.md), which is
probably more useful than this reader if you are implementing your own.

Known limits:

- **Encrypted stores are rejected**, cleanly. Only `NDB_CRYPT_NONE` is
  implemented; `NDB_CRYPT_PERMUTE` and `NDB_CRYPT_CYCLIC` need a substitution
  table from the spec that has not been added. Cached Exchange OSTs are typically
  unencrypted; PSTs often are not.
- **Version 23 is implemented but untested** against a real PST. Versions 14 and
  15 (Outlook 2002 and earlier) are rejected.
- **Bulk `sender_name` and `sender_email` are NULL for internal Exchange senders.**
  A contents table has no usable sender-name column, so those come from the sender
  EntryID, which spells out a name and address only for internet senders.
  `get_message` resolves the rest.
- **Named properties (0x8000 and above) are not resolved** — the named-property
  map is not parsed yet.
- **Torn pages are not retried.** Outlook writes while you read and every page
  carries a CRC, so this is detectable, but validation is not wired up.

**Mac Outlook** (`Outlook.sqlite` + `.olk15*` + `HxStore.hxd`,
`docs/mac-outlook-format.md`):

- **Two local stores, read together.** `Outlook.sqlite` + `.olk15*` (the
  classic engine) supplies folder, category and signature structure.
  `HxStore.hxd` — New Outlook's undocumented local cache, independently
  parsed here — supplies message content for whatever window the account's
  own sync setting keeps locally (e.g. the last 60 days), for at least
  Exchange/M365 accounts. Either can be empty on a given account without
  the other being; both return nothing rather than a guess when they are.
- **Recovered mail has no folder identity.** `HxStore.hxd` does not tie a
  message to a specific folder, so every message it supplies is exposed
  under one synthetic "Recovered Mail (Hx cache)" folder, not sorted into
  Inbox/Sent/etc.
- **Recovered mail has no attachment linkage.** A separate plain-file
  attachment cache exists (`Files/S0/<n>/Attachments/0/*`), but nothing
  found so far ties one of its files to a specific message.
- **Recovered mail's sender is sometimes NULL rather than a guess.** When a
  record's sender metadata sits after the anchor and holds more than one
  address (a participant's and the true sender's), there is no reliable way
  yet to tell them apart — see `docs/mac-outlook-format.md` §2.6.1.
- **Classic-engine message bodies come from `.olk15Message` only.**
  `.olk15MsgSource` (the higher-fidelity, full-RFC822 file some messages
  get) is located but not parsed yet — that needs a real MIME parser.
- **Classic-engine recipients are not populated.** `Mail` carries flat
  recipient-address-list columns, not a structured table, and the
  delimiter/encoding has not been measured against a real row.
- **Classic-engine folder names for the standard special folders (Inbox,
  Sent Items, …) are inferred from a measured type code, not read
  verbatim** — Outlook itself writes a literal placeholder string into
  `Folder_Name` on an account whose classic engine holds no real folder
  data, and that string is never surfaced.

## Safety

Read-only by construction: an OST/PST is memory-mapped without write access, a Mac
profile's `Outlook.sqlite` is opened with SQLite's read-only flag, and neither
backend has a code path that writes to a store. `--attachment --out` is the only
thing that writes anywhere, and it writes one payload to the path you name.

It is still your mail — a model with this server or skill attached can read every
message in the mailbox, so attach it deliberately.

## License

MIT. See [LICENSE](LICENSE).
