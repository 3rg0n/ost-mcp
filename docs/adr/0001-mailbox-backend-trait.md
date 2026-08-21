# 0001. A `Mailbox` trait behind the MCP surface

- Status: accepted
- Date: 2026-08-14

## Context

`ost-mcp` currently has one backend: `crates/ost-mcp/src/vtab.rs` and
`server.rs` call `ost::Store` directly, and its node ids are the OST/PST
`u32` NID. Issue #1 adds a second backend for Mac Outlook, targeting
`Outlook.sqlite` + `.olk15*` (see `docs/mac-outlook-format.md`), with `.olm`
archive import as a further, lower-priority source. A third source
(`.olm`) is already scoped in the same issue. Three sources sharing one MCP
surface is a cross-cutting change: it touches every tool in `server.rs`, the
DuckDB table functions in `vtab.rs`, and how `main.rs` picks a store — and
it is expensive to redo once tool schemas are published, so it gets an ADR
rather than being decided implicitly by whatever the first PR happens to do.

## Decision

Introduce a `Mailbox` trait that `ost::Store` and the new Mac backend both
implement, and make `server.rs`/`vtab.rs` depend on `Arc<dyn Mailbox>`
instead of `Arc<ost::Store>`:

```rust
pub trait Mailbox: Send + Sync {
    fn kind(&self) -> &'static str;           // "ost-v36", "mac-olk15", "olm"
    fn display_name(&self) -> Option<String>;
    fn folders(&self) -> Result<Vec<Folder>>;
    fn messages(&self, folder_id: i64) -> Result<Vec<MessageRow>>;
    fn message(&self, id: i64) -> Result<Message>;
    fn attachments(&self, id: i64) -> Result<Vec<Attachment>>;
    fn attachment_bytes(&self, msg: i64, att: i64) -> Result<Vec<u8>>;
}
```

**Identifier width: `i64` everywhere, including inside the OST backend.**
The OST reader's NID is a `u32`; a SQLite rowid is `i64`; the public `nid`
column DuckDB exposes is already `BIGINT`. Widening at the trait boundary
means one cast (`nid as i64`) at the edge of the existing OST code instead of
a second, parallel narrow-id path threaded through `vtab.rs` and every tool
in `server.rs`. `Folder`, `MessageRow`, `Message` and `Attachment` move to
this crate (or a new shared one) with `i64` ids; `ost::Store`'s own methods
keep returning `u32` and get wrapped, not rewritten.

**What stays optional is `None`, not a guess.** A backend returns `None` for
anything it cannot resolve, full stop — this already governs `ost::Store`
and does not change. The Mac backend adds one more case to watch for: a
value that is *present* but is Microsoft's own placeholder (a `Folder_Name`
literally equal to `Placeholder_Inbox_Placeholder`, measured in
`docs/mac-outlook-format.md` §3.1) is not a real value either, and gets
detected and nulled at the backend boundary rather than surfaced as if it
were a folder name.

`main.rs` picks a `Box<dyn Mailbox>` based on what `discover` finds
(`.ost`/`.pst` → OST backend, an `Outlook 15 Profiles/*/Data` glob hit → Mac
backend), rather than the caller naming a backend explicitly.

## Consequences

Every tool signature in `server.rs` and every table function in `vtab.rs`
changes from `Store`-shaped to `Mailbox`-shaped — a one-time, mechanical but
wide diff. `ost::Store` itself is untouched; a thin adapter implements
`Mailbox` for it. Adding the `.olm` backend later (Phase 3 of issue #1) is
then a new struct plus a new `discover` case, not another surface change.

The cost is indirection: every call through `vtab.rs` now goes through a
trait object instead of a concrete struct, which is one vtable dispatch per
call — immaterial next to a page read or a network round trip, so not worth
measuring further.
