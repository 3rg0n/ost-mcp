//! `ost-mcp` — mount an Outlook mailbox and serve it over MCP, or query it
//! once from the shell.
//!
//! ```text
//! model  <-->  ost-mcp (MCP stdio + DuckDB)  <-->  .ost / Outlook.sqlite
//! ```
//!
//! One process, one mounted store. The store is never copied, indexed or
//! exported: DuckDB reads it through the table functions in [`vtab`] as each
//! query runs, and the mapping is read-only, so a live mailbox with Outlook
//! running is safe to query. Two backends currently exist behind the
//! [`mailbox::Mailbox`] trait — the OST/PST reader in `crates/ost`, and the
//! Mac Outlook reader in `crates/mac-outlook` — see
//! `docs/adr/0001-mailbox-backend-trait.md`.
//!
//! Every MCP tool has a flag that prints the same JSON and exits, which is
//! what the bundled skill drives — see `skills/ost-mcp/SKILL.md`. `--sql`
//! covers `list_folders` and `search` on its own, since both are queries over
//! the same two tables.
//!
//! Usage:
//! ```text
//! ost-mcp                             # serve stdio MCP on the discovered store
//! ost-mcp <store>                     # serve stdio MCP on a named store
//! ost-mcp [store] --info              # store path, backend kind, size, schema
//! ost-mcp [store] --sql "..."         # run one query and print JSON
//! ost-mcp [store] --message <nid>     # one message with its body
//! ost-mcp [store] --attachments <nid> # attachment metadata for a message
//! ost-mcp [store] --attachment <m>:<a> [--out <path>]
//! ost-mcp --list                      # show the stores that were discovered
//! ```
//!
//! `<store>` is an `.ost`/`.pst` file, or a Mac Outlook profile's `Data`
//! directory (or its `Outlook.sqlite` file, or the identity directory above
//! it).

mod discover;
mod ost_adapter;
mod server;
mod sql;
mod vtab;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ost_adapter::OstMailbox;
use vtab::MailboxRef;

type Fail = Box<dyn std::error::Error>;

/// What to do with the store once it is open. Serving is the default; every
/// other variant prints one JSON document and exits.
enum Action {
    Info,
    Sql(String),
    Message(i64),
    Attachments(i64),
    Attachment(i64, i64),
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
            // A binary downloaded from a release has no other way to say what
            // it is, and the installer reports what it just put on PATH.
            "-V" | "--version" => {
                println!("ost-mcp {}", env!("CARGO_PKG_VERSION"));
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
        for dir in mac_outlook::discover::data_dirs() {
            println!("{:>14}  {:<18}  {:<24}  {}", "-", "mac profile", "", dir.display());
        }
        return Ok(());
    }

    let (store, display_path) = match path {
        Some(p) => open_named(Path::new(&p))?,
        None => open_discovered()?,
    };

    // Only SQL and the MCP server need DuckDB. Reading one body or one
    // payload is a single-message read, so building the database for it
    // would be waste.
    match action {
        Some(Action::Info) => print(&server::store_info(&store, &display_path)?)?,
        Some(Action::Message(nid)) => print(&server::message_report(&store, nid, max_body_chars)?)?,
        Some(Action::Attachments(nid)) => print(&server::attachment_list(&store, nid)?)?,
        Some(Action::Attachment(msg, att)) => match out {
            // Raw bytes to a file, because base64 of a megabyte through a
            // terminal is not usable by anything.
            Some(dest) => {
                let bytes = store.attachment_bytes(msg, att)?;
                std::fs::write(&dest, &bytes)?;
                let meta = server::attachment_list(&store, msg)?.into_iter().find(|a| a.nid == att);
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
            return serve(store, conn, display_path);
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
        return Err("pick one of --info, --sql, --message, --attachments, --attachment".into());
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

/// `i64`, not `u32`: a Mac-backend id (e.g. one recovered from `HxStore.hxd`,
/// `docs/mac-outlook-format.md` §2) can be negative, and this flag is the
/// only place a caller can type one in directly.
fn number(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<i64, Fail> {
    let raw = need(args, flag)?;
    raw.parse()
        .map_err(|_| format!("{flag} wants a number, got {raw:?}").into())
}

const HELP: &str = "\
ost-mcp — query an Outlook mailbox in place

  ost-mcp [<store>]                       serve MCP over stdio
  ost-mcp [<store>] --info                store path, backend kind, size, schema
  ost-mcp [<store>] --sql <sql>           one read-only query, prints JSON
  ost-mcp [<store>] --message <nid>       one message, headers and body
  ost-mcp [<store>] --attachments <nid>   attachment metadata for a message
  ost-mcp [<store>] --attachment <m>:<a>  one attachment payload
  ost-mcp --list                          list the stores that were discovered
  ost-mcp --version                       print the version and exit

Options:
  --limit <n>             max rows for --sql (default 10000)
  --max-body-chars <n>    body characters for --message (default 20000)
  --max-bytes <n>         payload bytes for --attachment (default 1048576)
  --out <path>            write the payload to a file instead of encoding it

<store> is an .ost/.pst file, or a Mac Outlook profile's Data directory (or its
Outlook.sqlite file, or the identity directory above it). With no argument the
store is resolved from the Outlook profile registry on Windows, or the Outlook
group container on macOS.
";

fn is_ost_or_pst(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()).map(|e| e.to_lowercase()),
        Some(ref e) if e == "ost" || e == "pst"
    )
}

/// Resolve an explicit argument to a backend. Anything not named `.ost`/`.pst`
/// is treated as a Mac profile location, accepting either the identity
/// directory, its `Data` subdirectory, or the `Outlook.sqlite` file itself.
fn open_named(path: &Path) -> Result<(MailboxRef, String), Fail> {
    if is_ost_or_pst(path) {
        let store = ost::Store::open(path)?;
        return Ok((Arc::new(OstMailbox(store)), path.display().to_string()));
    }

    let data_dir = mac_data_dir(path)?;
    let display = data_dir.display().to_string();
    let profile = mac_outlook::Profile::open(data_dir)?;
    Ok((Arc::new(profile), display))
}

/// The `Data` directory for a Mac profile path given in any of the forms
/// `open_named` accepts.
fn mac_data_dir(path: &Path) -> Result<PathBuf, Fail> {
    if path.file_name().and_then(|n| n.to_str()) == Some("Outlook.sqlite") {
        return path
            .parent()
            .map(PathBuf::from)
            .ok_or_else(|| "Outlook.sqlite has no parent directory".into());
    }
    if path.join("Outlook.sqlite").is_file() {
        return Ok(path.to_path_buf());
    }
    let data = path.join("Data");
    if data.join("Outlook.sqlite").is_file() {
        return Ok(data);
    }
    Err(format!("no Outlook.sqlite found under {}", path.display()).into())
}

fn open_discovered() -> Result<(MailboxRef, String), Fail> {
    if let Some(p) = discover::primary() {
        let store = ost::Store::open(&p)?;
        return Ok((Arc::new(OstMailbox(store)), p.display().to_string()));
    }
    if let Some(dir) = mac_outlook::discover::primary() {
        let display = dir.display().to_string();
        let profile = mac_outlook::Profile::open(dir)?;
        return Ok((Arc::new(profile), display));
    }
    Err("no .ost/.pst in any Outlook profile and no Mac Outlook profile found; \
         pass a store path as an argument (try --list)"
        .into())
}

/// An in-memory DuckDB with the store's table functions registered.
///
/// External access is disabled: the model's SQL can read the mailbox but cannot
/// reach the filesystem or the network, so no query can copy mail out of the
/// process.
fn open_db(store: &MailboxRef) -> Result<duckdb::Connection, Fail> {
    let conn = duckdb::Connection::open_in_memory()?;
    vtab::register(&conn, store)?;
    conn.execute_batch("SET enable_external_access = false")?;
    Ok(conn)
}

/// Serve MCP on stdio. Nothing may be printed to stdout after this point — it is
/// the protocol channel; diagnostics go to stderr.
fn serve(store: MailboxRef, conn: duckdb::Connection, path: String) -> Result<(), Fail> {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
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
