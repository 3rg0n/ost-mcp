//! `ost-mcp` — mount an Outlook OST/PST and serve it over MCP, or query it once
//! from the shell.
//!
//! ```text
//! model  <-->  ost-mcp (MCP stdio + DuckDB)  <-->  .ost
//! ```
//!
//! One process, one mapped file. The store is never copied, indexed or exported:
//! DuckDB reads it through the table functions in [`vtab`] as each query runs,
//! and the mapping is read-only, so a live mailbox with Outlook running is safe
//! to query.
//!
//! Every MCP tool has a flag that prints the same JSON and exits, which is what
//! the bundled skill drives — see `skills/ost-mcp/SKILL.md`. `--sql` covers
//! `list_folders` and `search` on its own, since both are queries over the same
//! two tables.
//!
//! Usage:
//! ```text
//! ost-mcp                             # serve stdio MCP on the profile's store
//! ost-mcp <file.ost>                  # serve stdio MCP on a named store
//! ost-mcp [file.ost] --info           # store path, version, size, schema
//! ost-mcp [file.ost] --sql "..."      # run one query and print JSON
//! ost-mcp [file.ost] --message <nid>  # one message with its body
//! ost-mcp --list                      # show the stores that were discovered
//! ```

mod discover;
mod server;
mod sql;
mod vtab;

use std::path::PathBuf;
use std::sync::Arc;

use ost::Store;

type Fail = Box<dyn std::error::Error>;

/// What to do with the store once it is open. Serving is the default; every
/// other variant prints one JSON document and exits.
enum Action {
    Info,
    Sql(String),
    Message(u32),
    Attachments(u32),
    Attachment(u32, u32),
}

fn main() -> Result<(), Fail> {
    let mut path: Option<String> = None;
    let mut action: Option<Action> = None;
    let mut list = false;
    let mut limit = 10_000usize;
    let mut max_body_chars: Option<usize> = None;
    let mut max_bytes: Option<usize> = None;
    let mut out: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);

    while let Some(a) = args.next() {
        match a.as_str() {
            "--info" => set(&mut action, Action::Info)?,
            "--sql" => set(&mut action, Action::Sql(need(&mut args, "--sql")?))?,
            "--message" => set(&mut action, Action::Message(number(&mut args, "--message")?))?,
            "--attachments" => {
                set(&mut action, Action::Attachments(number(&mut args, "--attachments")?))?
            }
            "--attachment" => {
                let pair = need(&mut args, "--attachment")?;
                let (m, at) = pair
                    .split_once(':')
                    .ok_or("--attachment wants <message_nid>:<attachment_nid>")?;
                set(&mut action, Action::Attachment(m.parse()?, at.parse()?))?
            }
            "--limit" => limit = number(&mut args, "--limit")? as usize,
            "--max-body-chars" => max_body_chars = Some(number(&mut args, "--max-body-chars")? as usize),
            "--max-bytes" => max_bytes = Some(number(&mut args, "--max-bytes")? as usize),
            "--out" => out = Some(PathBuf::from(need(&mut args, "--out")?)),
            "--list" => list = true,
            "-h" | "--help" => {
                eprintln!("{HELP}");
                return Ok(());
            }
            other if other.starts_with('-') => return Err(format!("unknown flag {other}").into()),
            other => path = Some(other.to_string()),
        }
    }

    if list {
        for f in discover::stores() {
            println!(
                "{:>14}  {:<18}  {:<24}  {}",
                f.bytes,
                f.source,
                f.account.unwrap_or_default(),
                f.path.display()
            );
        }
        return Ok(());
    }

    let path = match path {
        Some(p) => PathBuf::from(p),
        None => discover::primary().ok_or(
            "no .ost or .pst in any Outlook profile or in the Outlook directory; \
             pass one as an argument (try --list)",
        )?,
    };
    let store = Arc::new(Store::open(&path)?);

    // Only SQL and the MCP server need DuckDB. Reading one body or one payload
    // is a single-node read, so building the database for it would be waste.
    match action {
        Some(Action::Info) => print(&server::store_info(&store, &path.display().to_string())?)?,
        Some(Action::Message(nid)) => {
            print(&server::message_report(&store, nid, max_body_chars)?)?
        }
        Some(Action::Attachments(nid)) => print(&server::attachment_list(&store, nid)?)?,
        Some(Action::Attachment(msg, att)) => match out {
            // Raw bytes to a file, because base64 of a megabyte through a
            // terminal is not usable by anything.
            Some(dest) => {
                let bytes = store.attachment_bytes(msg, att)?;
                std::fs::write(&dest, &bytes)?;
                let meta = server::attachment_list(&store, msg)?
                    .into_iter()
                    .find(|a| a.nid == att);
                print(&serde_json::json!({
                    "message_nid": msg,
                    "attachment_nid": att,
                    "filename": meta.as_ref().and_then(|m| m.filename.clone()),
                    "mime": meta.as_ref().and_then(|m| m.mime.clone()),
                    "written_bytes": bytes.len(),
                    "path": dest.display().to_string(),
                }))?
            }
            None => print(&server::attachment_data(&store, msg, att, max_bytes)?)?,
        },
        Some(Action::Sql(q)) => {
            sql::check_read_only(&q).map_err(|e| -> Fail { e.into() })?;
            let conn = open_db(&store)?;
            let rows = sql::query(&conn, &q, [], limit)?;
            print(&rows.rows)?
        }
        None => {
            let conn = open_db(&store)?;
            return serve(store, conn, path.display().to_string());
        }
    }
    Ok(())
}

fn print<T: serde::Serialize>(value: &T) -> Result<(), Fail> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

/// One action per run, so a typo cannot silently override an earlier flag.
fn set(slot: &mut Option<Action>, a: Action) -> Result<(), Fail> {
    if slot.is_some() {
        return Err(
            "pick one of --info, --sql, --message, --attachments, --attachment".into(),
        );
    }
    *slot = Some(a);
    Ok(())
}

fn need(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, Fail> {
    match args.next() {
        Some(v) => Ok(v),
        None => Err(format!("{flag} needs a value").into()),
    }
}

fn number(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<u32, Fail> {
    let raw = need(args, flag)?;
    raw.parse()
        .map_err(|_| format!("{flag} wants a number, got {raw:?}").into())
}

const HELP: &str = "\
ost-mcp — query an Outlook OST/PST in place

  ost-mcp [<file.ost>]                       serve MCP over stdio
  ost-mcp [<file.ost>] --info                store path, version, size, schema
  ost-mcp [<file.ost>] --sql <sql>           one read-only query, prints JSON
  ost-mcp [<file.ost>] --message <nid>       one message, headers and body
  ost-mcp [<file.ost>] --attachments <nid>   attachment metadata for a message
  ost-mcp [<file.ost>] --attachment <m>:<a>  one attachment payload
  ost-mcp --list                             list the stores in Outlook profiles

Options:
  --limit <n>             max rows for --sql (default 10000)
  --max-body-chars <n>    body characters for --message (default 20000)
  --max-bytes <n>         payload bytes for --attachment (default 1048576)
  --out <path>            write the payload to a file instead of encoding it

With no file argument the store comes from the Outlook profile registry.
";

/// An in-memory DuckDB with the store's table functions registered.
///
/// External access is disabled: the model's SQL can read the mailbox but cannot
/// reach the filesystem or the network, so no query can copy mail out of the
/// process.
fn open_db(store: &Arc<Store>) -> Result<duckdb::Connection, Fail> {
    let conn = duckdb::Connection::open_in_memory()?;
    vtab::register(&conn, store)?;
    conn.execute_batch("SET enable_external_access = false")?;
    Ok(conn)
}

/// Serve MCP on stdio. Nothing may be printed to stdout after this point — it is
/// the protocol channel; diagnostics go to stderr.
fn serve(store: Arc<Store>, conn: duckdb::Connection, path: String) -> Result<(), Fail> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        use rmcp::ServiceExt;
        eprintln!("ost-mcp: serving {path}");
        let service = server::OstServer::new(store, conn, path)
            .serve(rmcp::transport::stdio())
            .await?;
        service.waiting().await?;
        Ok(())
    })
}
