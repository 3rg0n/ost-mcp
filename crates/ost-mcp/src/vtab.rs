//! DuckDB table functions over a live OST.
//!
//! Each function reads the store when the query runs — there is no import step
//! and nothing is written to disk. What gets materialised is only the *row
//! metadata* the query needs: folder entries and contents-table rows are cheap,
//! so they are collected in `init` and handed back in chunk-sized slices. Bodies
//! and attachment payloads are never touched here; those come from the
//! `get_message` and `read_attachment` tools, which read one node on demand.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use duckdb::core::{DataChunkHandle, Inserter, LogicalTypeHandle, LogicalTypeId};
use duckdb::vtab::{BindInfo, InitInfo, TableFunctionInfo, VTab};
use duckdb::Connection;
use ost::store::{nid_type, NID_TYPE_NORMAL_FOLDER};
use ost::{Attachment, Folder, MessageRow, Store};

type BoxError = Box<dyn std::error::Error>;

/// Node type 0x04 is a normal message.
const NID_TYPE_NORMAL_MESSAGE: u32 = 0x04;

/// Register `ost_folders`, `ost_messages` and `ost_attachments`, then wrap the
/// two cheap ones in views so queries can say `FROM messages`.
pub fn register(conn: &Connection, store: &Arc<Store>) -> duckdb::Result<()> {
    conn.register_table_function_with_extra_info::<Folders, _>("ost_folders", store)?;
    conn.register_table_function_with_extra_info::<Messages, _>("ost_messages", store)?;
    conn.register_table_function_with_extra_info::<Attachments, _>("ost_attachments", store)?;
    conn.execute_batch(
        "CREATE VIEW folders AS SELECT * FROM ost_folders();
         CREATE VIEW messages AS SELECT * FROM ost_messages();",
    )
}

/// Clone the `Arc<Store>` that was handed to
/// `register_table_function_with_extra_info`.
///
/// # Safety
/// DuckDB owns the boxed extra info for as long as the function is registered,
/// which outlives every bind, init and execution callback.
unsafe fn store_of(ptr: *const Arc<Store>) -> Arc<Store> {
    unsafe { (*ptr).clone() }
}

/// Emit `rows[cursor..]` one chunk per call. A zero length is how a table
/// function signals it has finished.
fn emit<R>(
    rows: &[R],
    cursor: &AtomicUsize,
    output: &mut DataChunkHandle,
    write: impl Fn(&[R], &mut DataChunkHandle),
) {
    let start = cursor.load(Ordering::Relaxed);
    if start >= rows.len() {
        output.set_len(0);
        return;
    }
    let end = (start + output.flat_vector(0).capacity()).min(rows.len());
    let slice = &rows[start..end];
    write(slice, output);
    output.set_len(slice.len());
    cursor.store(end, Ordering::Relaxed);
}

/// Absent values become SQL NULL rather than an empty string: "this contents
/// table carries no such column" and "this message has an empty subject" are
/// different facts, and a model should be able to tell them apart.
fn put_str(output: &mut DataChunkHandle, col: usize, vals: impl Iterator<Item = Option<String>>) {
    let mut v = output.flat_vector(col);
    for (row, val) in vals.enumerate() {
        match val {
            Some(s) => v.insert(row, s.as_str()),
            None => v.set_null(row),
        }
    }
}

/// Scalar columns are written through a typed slice, so a null still needs a
/// value in the slot; nulls are marked after the slice borrow ends.
fn put_i64(output: &mut DataChunkHandle, col: usize, len: usize, vals: impl Iterator<Item = Option<i64>>) {
    let mut v = output.flat_vector(col);
    let mut nulls = Vec::new();
    {
        let slice = unsafe { v.as_mut_slice_with_len::<i64>(len) };
        for (row, val) in vals.enumerate() {
            slice[row] = val.unwrap_or(0);
            if val.is_none() {
                nulls.push(row);
            }
        }
    }
    for row in nulls {
        v.set_null(row);
    }
}

fn put_bool(output: &mut DataChunkHandle, col: usize, len: usize, vals: impl Iterator<Item = Option<bool>>) {
    let mut v = output.flat_vector(col);
    let mut nulls = Vec::new();
    {
        let slice = unsafe { v.as_mut_slice_with_len::<bool>(len) };
        for (row, val) in vals.enumerate() {
            slice[row] = val.unwrap_or(false);
            if val.is_none() {
                nulls.push(row);
            }
        }
    }
    for row in nulls {
        v.set_null(row);
    }
}

fn varchar() -> LogicalTypeHandle {
    LogicalTypeHandle::from(LogicalTypeId::Varchar)
}

fn bigint() -> LogicalTypeHandle {
    LogicalTypeHandle::from(LogicalTypeId::Bigint)
}

fn boolean() -> LogicalTypeHandle {
    LogicalTypeHandle::from(LogicalTypeId::Boolean)
}

/// Microseconds since the Unix epoch, which is DuckDB's physical `TIMESTAMP`.
fn timestamp() -> LogicalTypeHandle {
    LogicalTypeHandle::from(LogicalTypeId::Timestamp)
}

fn declare(bind: &BindInfo, cols: Vec<(&str, LogicalTypeHandle)>) {
    for (name, ty) in cols {
        bind.add_result_column(name, ty);
    }
}

// ---------------------------------------------------------------- ost_folders

pub struct Folders;

pub struct FoldersInit {
    rows: Vec<Folder>,
    cursor: AtomicUsize,
}

impl VTab for Folders {
    type BindData = ();
    type InitData = FoldersInit;

    fn bind(bind: &BindInfo) -> Result<(), BoxError> {
        declare(
            bind,
            vec![
                ("nid", bigint()),
                ("parent_nid", bigint()),
                ("name", varchar()),
                ("path", varchar()),
                ("item_count", bigint()),
                ("unread_count", bigint()),
                ("has_subfolders", boolean()),
                ("is_search_folder", boolean()),
            ],
        );
        Ok(())
    }

    fn init(init: &InitInfo) -> Result<FoldersInit, BoxError> {
        init.set_max_threads(1);
        let store = unsafe { store_of(init.get_extra_info()) };
        Ok(FoldersInit {
            rows: store.folders()?,
            cursor: AtomicUsize::new(0),
        })
    }

    fn func(func: &TableFunctionInfo<Self>, output: &mut DataChunkHandle) -> Result<(), BoxError> {
        let init = func.get_init_data();
        emit(&init.rows, &init.cursor, output, |rows, out| {
            let n = rows.len();
            put_i64(out, 0, n, rows.iter().map(|f| Some(f.nid as i64)));
            put_i64(out, 1, n, rows.iter().map(|f| Some(f.parent_nid as i64)));
            put_str(out, 2, rows.iter().map(|f| Some(f.name.clone())));
            put_str(out, 3, rows.iter().map(|f| Some(f.path.clone())));
            put_i64(out, 4, n, rows.iter().map(|f| f.item_count.map(i64::from)));
            put_i64(out, 5, n, rows.iter().map(|f| f.unread_count.map(i64::from)));
            put_bool(out, 6, n, rows.iter().map(|f| Some(f.has_subfolders)));
            put_bool(
                out,
                7,
                n,
                rows.iter().map(|f| Some(nid_type(f.nid) != NID_TYPE_NORMAL_FOLDER)),
            );
        });
        Ok(())
    }
}

// --------------------------------------------------------------- ost_messages

pub struct Messages;

pub struct MessagesInit {
    /// Folder path alongside the row, so a query can filter by folder without a
    /// join.
    rows: Vec<(String, MessageRow)>,
    cursor: AtomicUsize,
}

impl VTab for Messages {
    type BindData = ();
    type InitData = MessagesInit;

    fn bind(bind: &BindInfo) -> Result<(), BoxError> {
        declare(
            bind,
            vec![
                ("nid", bigint()),
                ("folder_nid", bigint()),
                ("folder_path", varchar()),
                ("subject", varchar()),
                ("sender_name", varchar()),
                ("sender_email", varchar()),
                ("delivered", timestamp()),
                ("submitted", timestamp()),
                ("modified", timestamp()),
                ("size", bigint()),
                ("unread", boolean()),
                ("has_attachments", boolean()),
                ("message_class", varchar()),
            ],
        );
        Ok(())
    }

    fn init(init: &InitInfo) -> Result<MessagesInit, BoxError> {
        init.set_max_threads(1);
        let store = unsafe { store_of(init.get_extra_info()) };
        let mut rows = Vec::new();
        // Search folders own no contents table of their own. A folder whose
        // table will not open is skipped rather than failing the whole query;
        // `ost_folders` is where its existence is still visible.
        for f in store.folders()? {
            if nid_type(f.nid) != NID_TYPE_NORMAL_FOLDER {
                continue;
            }
            if let Ok(msgs) = store.messages(f.nid) {
                rows.extend(msgs.into_iter().map(|m| (f.path.clone(), m)));
            }
        }
        Ok(MessagesInit {
            rows,
            cursor: AtomicUsize::new(0),
        })
    }

    fn func(func: &TableFunctionInfo<Self>, output: &mut DataChunkHandle) -> Result<(), BoxError> {
        let init = func.get_init_data();
        emit(&init.rows, &init.cursor, output, |rows, out| {
            let n = rows.len();
            put_i64(out, 0, n, rows.iter().map(|(_, m)| Some(m.nid as i64)));
            put_i64(out, 1, n, rows.iter().map(|(_, m)| Some(m.folder_nid as i64)));
            put_str(out, 2, rows.iter().map(|(p, _)| Some(p.clone())));
            put_str(out, 3, rows.iter().map(|(_, m)| m.subject.clone()));
            put_str(out, 4, rows.iter().map(|(_, m)| m.sender_name.clone()));
            put_str(out, 5, rows.iter().map(|(_, m)| m.sender_email.clone()));
            put_i64(out, 6, n, rows.iter().map(|(_, m)| m.delivered_us));
            put_i64(out, 7, n, rows.iter().map(|(_, m)| m.submitted_us));
            put_i64(out, 8, n, rows.iter().map(|(_, m)| m.modified_us));
            put_i64(out, 9, n, rows.iter().map(|(_, m)| m.size.map(i64::from)));
            put_bool(out, 10, n, rows.iter().map(|(_, m)| m.unread));
            put_bool(out, 11, n, rows.iter().map(|(_, m)| m.has_attachments));
            put_str(out, 12, rows.iter().map(|(_, m)| m.message_class.clone()));
        });
        Ok(())
    }
}

// ------------------------------------------------------------ ost_attachments

pub struct Attachments;

pub struct AttachmentsInit {
    rows: Vec<(u32, Attachment)>,
    cursor: AtomicUsize,
}

impl VTab for Attachments {
    /// The `message_nid` named parameter, if the query scoped the sweep.
    type BindData = Option<u32>;
    type InitData = AttachmentsInit;

    fn bind(bind: &BindInfo) -> Result<Option<u32>, BoxError> {
        declare(
            bind,
            vec![
                ("message_nid", bigint()),
                ("nid", bigint()),
                ("filename", varchar()),
                ("mime", varchar()),
                ("content_id", varchar()),
                ("declared_size", bigint()),
                ("data_len", bigint()),
            ],
        );
        Ok(bind
            .get_named_parameter("message_nid")
            .filter(|v| !v.is_null())
            .map(|v| v.to_int64() as u32))
    }

    /// Listing attachments opens one subnode BTree per message, so unlike the
    /// other two functions this one is only cheap when scoped. `message_nid`
    /// scopes it; with no argument it sweeps every message in the store.
    fn init(init: &InitInfo) -> Result<AttachmentsInit, BoxError> {
        init.set_max_threads(1);
        let store = unsafe { store_of(init.get_extra_info()) };
        let scope = unsafe { *init.get_bind_data::<Option<u32>>() };

        let targets: Vec<u32> = match scope {
            Some(nid) => vec![nid],
            None => {
                let mut all: Vec<u32> = store
                    .pff
                    .nbt
                    .keys()
                    .copied()
                    .filter(|n| nid_type(*n) == NID_TYPE_NORMAL_MESSAGE)
                    .collect();
                all.sort_unstable();
                all
            }
        };
        let mut rows = Vec::new();
        for nid in targets {
            if let Ok(atts) = store.attachments(nid) {
                rows.extend(atts.into_iter().map(|a| (nid, a)));
            }
        }
        Ok(AttachmentsInit {
            rows,
            cursor: AtomicUsize::new(0),
        })
    }

    fn func(func: &TableFunctionInfo<Self>, output: &mut DataChunkHandle) -> Result<(), BoxError> {
        let init = func.get_init_data();
        emit(&init.rows, &init.cursor, output, |rows, out| {
            let n = rows.len();
            put_i64(out, 0, n, rows.iter().map(|(m, _)| Some(*m as i64)));
            put_i64(out, 1, n, rows.iter().map(|(_, a)| Some(a.nid as i64)));
            put_str(out, 2, rows.iter().map(|(_, a)| a.filename.clone()));
            put_str(out, 3, rows.iter().map(|(_, a)| a.mime.clone()));
            put_str(out, 4, rows.iter().map(|(_, a)| a.content_id.clone()));
            put_i64(out, 5, n, rows.iter().map(|(_, a)| a.declared_size.map(i64::from)));
            put_i64(out, 6, n, rows.iter().map(|(_, a)| a.data_len.map(|l| l as i64)));
        });
        Ok(())
    }

    fn named_parameters() -> Option<Vec<(String, LogicalTypeHandle)>> {
        Some(vec![("message_nid".to_string(), bigint())])
    }
}
