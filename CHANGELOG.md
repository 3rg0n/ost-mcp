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

### Fixed
- `messages.sender_name` and `messages.sender_email` now come from
  `PidTagSenderEntryId`, which every contents-table row carries, instead of the
  `PidTagSentRepresentingName` cells, which are not resolvable heap ids in a v36
  table and returned unrelated property values.
- Table-context reads honour each row's cell existence bitmap, so a column with no
  value for a row reads as absent rather than as whatever bytes the cell still holds.
- `messages.has_attachments` falls back to `PidTagMessageFlags`, which is where a
  contents table actually records it.
