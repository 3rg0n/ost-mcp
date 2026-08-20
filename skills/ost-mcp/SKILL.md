---
name: ost-mcp
description: Query the local Outlook mailbox file (.ost/.pst) in place with SQL, read message bodies, and extract attachments. Use for any question about the user's own email — find or count messages by sender, folder, subject or date, see what arrived recently, check unread counts, or pull a file off a message. Needs the ost-mcp binary; Windows only.
---

# ost-mcp

Reads an Outlook `.ost`/`.pst` **as a file**. No Graph API, no OAuth, no COM, and
Outlook does not need to be running. The mailbox is memory-mapped read-only and
queried by an embedded DuckDB as each query runs, so nothing is exported or
indexed first.

Every command below prints one JSON document to stdout and exits.

## Resolve the binary

Use `ost-mcp` if it is on `PATH`. Otherwise use `$env:OST_MCP_BIN`. If neither
resolves, stop and tell the user to run `cargo install --path crates/ost-mcp`
from a clone of `github.com/3rg0n/ost-mcp` — do not guess at a path.

With no file argument the store is resolved from the Outlook profile registry,
so a plain `ost-mcp --info` normally works. Pass a path as the first argument to
read a specific file, such as an archive `.pst`.

## Commands

| Command | Returns |
|---|---|
| `ost-mcp --list` | every store found, with its profile and account |
| `ost-mcp --info` | path, format version, size, folder count, schema |
| `ost-mcp --sql "<query>"` | rows as JSON — the main tool |
| `ost-mcp --message <nid>` | one message: headers, recipients, body, attachment list |
| `ost-mcp --attachments <nid>` | attachment metadata for a message, no payloads |
| `ost-mcp --attachment <msg>:<att>` | one payload, as text or base64 |
| `ost-mcp --attachment <msg>:<att> --out <path>` | writes the payload to a file |

Options: `--limit <n>` caps `--sql` rows (default 10000), `--max-body-chars <n>`
caps a body (default 20000), `--max-bytes <n>` caps an inline payload (default
1048576).

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

`nid` is the handle for everything. A message's `nid` comes from a query; an
attachment's comes from `--attachments`. `delivered`, `submitted` and `modified`
are timestamps, so `delivered >= '2026-08-01'` works.

`ost_attachments` is a table function and needs a message id, so it cannot be
scanned across the whole store. To find messages with attachments, filter on
`has_attachments` instead.

## Recipes

Recent mail in a folder:

```sql
SELECT nid, delivered, sender_name, sender_email, subject
FROM messages WHERE folder_path ILIKE '%Inbox%'
ORDER BY delivered DESC LIMIT 20
```

Who sends the most:

```sql
SELECT sender_email, count(*) AS n FROM messages
WHERE sender_email IS NOT NULL
GROUP BY 1 ORDER BY n DESC LIMIT 20
```

Unread by folder:

```sql
SELECT path, item_count, unread_count FROM folders
WHERE unread_count > 0 ORDER BY unread_count DESC
```

Find a message, then read it:

```powershell
ost-mcp --sql "SELECT nid, subject FROM messages WHERE subject ILIKE '%invoice%' ORDER BY delivered DESC LIMIT 10"
ost-mcp --message 24295876
```

Get a file off a message:

```powershell
ost-mcp --attachments 24295876
ost-mcp --attachment 24295876:24277637 --out "$env:TEMP\report.pdf"
```

## Rules

- **Always bound the query.** Put `LIMIT` in the SQL and select only the columns
  needed. Output is pretty-printed JSON, so `SELECT *` over a large result wastes
  a great deal of context. The store can hold tens of thousands of messages.
- **Aggregate in SQL, not by reading rows.** Counting, grouping and date
  filtering all belong in the query; the whole point is that one scan answers
  the question.
- **This is the user's real mail.** Summarise it in the conversation. Never write
  message content, a subject line, a sender address or an attachment payload into
  a repository file, a commit message, an issue or a PR. Write extracted
  attachments to a temp directory, not into a repo.
- **Read-only, by construction.** There is no code path that writes to the store
  and no way to send, delete, move or flag a message. Do not offer to.
- **`--sql` accepts one read-only statement.** Multiple statements and anything
  that writes are rejected before execution.

## Known gaps — do not report these as errors

- **`sender_name` and `sender_email` are NULL in `messages` for internal Exchange
  senders.** A contents table carries no usable sender-name column, so bulk rows
  take the sender from the EntryID, which spells out a name and address only for
  internet senders. `--message <nid>` resolves the rest. A NULL here means "not
  available in the index", not "unknown sender" — say so rather than reporting
  the mailbox as broken.
- **Encrypted stores are rejected** with a clear error. Only `NDB_CRYPT_NONE` is
  implemented. Cached Exchange OSTs are usually unencrypted; a PST often is not.
- **Named properties (0x8000 and above) are not resolved**, so custom and some
  Outlook-specific fields are absent.
- **Windows only.** Mac Outlook keeps no OST — it uses `Outlook.sqlite` plus
  `.olk15*` files, which is a different reader and not built yet.
