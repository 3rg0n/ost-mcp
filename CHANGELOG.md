# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- `ost-mcp` serves an Outlook OST/PST over MCP with seven tools, reading the file in
  place through DuckDB table functions — no export, no index, and no need to close
  Outlook first.
- The store to open is resolved from the Outlook profile registry, so `ost-mcp`
  needs no path argument and `--list` reports which profile and account each store
  belongs to; scanning the Outlook directory is now only a fallback.
- Support for the undocumented OST format version 36 written by Outlook 2013 and
  later: 4 KB pages, widened BTree page counts, and zlib-compressed block payloads
  (`docs/ost-v36-format.md`).
- `ost-mcp` can now mount a Mac Outlook profile, read-only, reading real folder,
  category and signature structure from `Outlook.sqlite` + `.olk15*` and real
  message content from `HxStore.hxd` — New Outlook's undocumented local cache,
  independently parsed (block container, LZ4 payloads, record layout credited to
  `securized/hxstore-reverse-engineering`, MIT) and exposed as one recovered-mail
  folder alongside the classic ones, since it carries no folder identity of its
  own (`docs/mac-outlook-format.md`).
- A bundled Claude Code skill (`skills/ost-mcp/SKILL.md`) drives the binary from the
  shell, so a model can query the mailbox without an MCP server registration.
- Every MCP tool now has a command-line flag that prints the same JSON and exits:
  `--info`, `--message`, `--attachments` and `--attachment` join `--sql`, which
  already covered folder listing and search.
- `--attachment <msg>:<att> --out <path>` writes an attachment's bytes to a file,
  which base64 through a terminal cannot usefully do.
- `install.ps1` installs the binary and the skill from one `irm ... | iex` line. It
  checks for the Rust toolchain and the MSVC build tools first, names the winget
  command for whichever is missing, and finishes by opening the store and running a
  query rather than claiming success. The README carries a prompt that hands the
  whole job to an agent.

### Changed
- Every backend now implements a shared `Mailbox` trait instead of `ost-mcp` calling
  the OST reader directly, so the Mac backend and a future `.olm` reader can sit
  behind the same MCP surface (`docs/adr/0001-mailbox-backend-trait.md`).
- Message, folder and attachment ids widened from `u32` to `i64` at the MCP tool
  boundary and in the CLI flags, to accommodate backends whose native ids do not
  fit a `u32` — including negative ids for messages recovered from `HxStore.hxd`.
- `store_info` reports a `kind` string (e.g. `ost-v36`, `mac-olk15`) instead of a
  numeric `version` field, since format version is not a concept every backend has.
- A scope-less `ost_attachments()` sweep (no `message_nid` argument) now only covers
  messages reachable through `folders()`/`messages()`, matching what `search` and
  `list_folders` already see; it no longer separately walks associated-content
  (FAI) items such as rules, forms and views.
- The four non-DuckDB MCP tools (`store_info`, `get_message`, `list_attachments`,
  `read_attachment`) now build their replies through plain functions shared with
  the equivalent CLI flags, so the two transports cannot drift apart.

### Security
- A Mac profile's `PathToDataFile` value (from `Outlook.sqlite`) is now checked for
  a `..` component or an absolute path before being joined onto the profile
  directory, closing an arbitrary-file-read path a corrupted or crafted database
  row could otherwise trigger through `get_message`/`read_attachment`.
- `.olk15Message`/`.olk15MsgAttachment` header parsing no longer slices a string at
  an offset computed from a separately case-folded copy of it, which could panic on
  certain non-ASCII input (case-folding can change a character's byte length).

### Fixed
- `messages.sender_name` and `messages.sender_email` now come from
  `PidTagSenderEntryId`, which every contents-table row carries, instead of the
  `PidTagSentRepresentingName` cells, which are not resolvable heap ids in a v36
  table and returned unrelated property values.
- Table-context reads honour each row's cell existence bitmap, so a column with no
  value for a row reads as absent rather than as whatever bytes the cell still holds.
- `messages.has_attachments` falls back to `PidTagMessageFlags`, which is where a
  contents table actually records it.
- A Mac `HxStore.hxd` record whose sender metadata sits entirely after the anchor
  and holds more than one address, one a participant's and one the true sender's,
  no longer reports the nearer (and sometimes wrong) one as the sender; it reports
  neither rather than guess. Found by using the tool to answer a real question and
  checking the answer against the message's own quoted text, not by review.
