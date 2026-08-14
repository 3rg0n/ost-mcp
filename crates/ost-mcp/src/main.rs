//! `ost-mcp` — mount an Outlook mailbox and serve it over MCP.
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
//! Usage:
//! ```text
//! ost-mcp                          # serve stdio MCP on the discovered store
//! ost-mcp <file.ost>                serve stdio MCP on a named OST/PST
//! ost-mcp <profile Data dir>         serve stdio MCP on a named Mac profile
//! ost-mcp [store] --sql "..."       run one query and print JSON, then exit
//! ost-mcp --list                    show the stores that were discovered
//! ```

mod discover;
mod ost_adapter;
mod server;
mod sql;
mod vtab;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ost_adapter::OstMailbox;
use vtab::MailboxRef;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut path: Option<String> = None;
    let mut query: Option<String> = None;
    let mut list = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--sql" => query = Some(args.next().ok_or("--sql needs a statement")?),
            "--list" => list = true,
            "-h" | "--help" => {
                eprintln!("{}", HELP);
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
    let conn = open_db(&store)?;

    match query {
        Some(q) => {
            sql::check_read_only(&q).map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
            let rows = sql::query(&conn, &q, [], 10_000)?;
            println!("{}", serde_json::to_string_pretty(&rows.rows)?);
            Ok(())
        }
        None => serve(store, conn, display_path),
    }
}

const HELP: &str = "\
ost-mcp — query an Outlook mailbox over MCP

  ost-mcp [<store>]                 serve MCP over stdio
  ost-mcp [<store>] --sql <sql>     run one read-only query, print JSON
  ost-mcp --list                    list the stores that were discovered

<store> is an .ost/.pst file, or a Mac Outlook profile's Data directory
(or its Outlook.sqlite file, or the identity directory above it).
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
fn open_named(path: &Path) -> Result<(MailboxRef, String), Box<dyn std::error::Error>> {
    if is_ost_or_pst(path) {
        let store = ost::Store::open(path)?;
        return Ok((
            Arc::new(OstMailbox(store)),
            path.display().to_string(),
        ));
    }

    let data_dir = mac_data_dir(path)?;
    let display = data_dir.display().to_string();
    let profile = mac_outlook::Profile::open(data_dir)?;
    Ok((Arc::new(profile), display))
}

/// The `Data` directory for a Mac profile path given in any of the forms
/// `open_named` accepts.
fn mac_data_dir(path: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
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

fn open_discovered() -> Result<(MailboxRef, String), Box<dyn std::error::Error>> {
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
fn open_db(store: &MailboxRef) -> Result<duckdb::Connection, Box<dyn std::error::Error>> {
    let conn = duckdb::Connection::open_in_memory()?;
    vtab::register(&conn, store)?;
    conn.execute_batch("SET enable_external_access = false")?;
    Ok(conn)
}

/// Serve MCP on stdio. Nothing may be printed to stdout after this point — it is
/// the protocol channel; diagnostics go to stderr.
fn serve(
    store: MailboxRef,
    conn: duckdb::Connection,
    path: String,
) -> Result<(), Box<dyn std::error::Error>> {
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
