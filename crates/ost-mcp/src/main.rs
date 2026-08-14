//! `ost-mcp` — mount an Outlook OST/PST and serve it over MCP.
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
//! Usage:
//! ```text
//! ost-mcp                          # serve stdio MCP on the discovered store
//! ost-mcp <file.ost>               # serve stdio MCP on a named store
//! ost-mcp [file.ost] --sql "..."   # run one query and print JSON, then exit
//! ost-mcp --list                   # show the stores that were discovered
//! ```

mod discover;
mod server;
mod sql;
mod vtab;

use std::sync::Arc;

use ost::Store;

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
        let dir = discover::default_dir().ok_or("LOCALAPPDATA is not set")?;
        for p in discover::candidates_in(&dir) {
            let bytes = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
            println!("{:>14}  {}", bytes, p.display());
        }
        return Ok(());
    }

    let path = match path {
        Some(p) => std::path::PathBuf::from(p),
        None => discover::primary()
            .ok_or("no .ost or .pst found; pass one as an argument (try --list)")?,
    };
    let store = Arc::new(Store::open(&path)?);
    let conn = open_db(&store)?;

    match query {
        Some(q) => {
            sql::check_read_only(&q).map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
            let rows = sql::query(&conn, &q, [], 10_000)?;
            println!("{}", serde_json::to_string_pretty(&rows.rows)?);
            Ok(())
        }
        None => serve(store, conn, path.display().to_string()),
    }
}

const HELP: &str = "\
ost-mcp — query an Outlook OST/PST over MCP

  ost-mcp [<file.ost>]              serve MCP over stdio
  ost-mcp [<file.ost>] --sql <sql>  run one read-only query, print JSON
  ost-mcp --list                    list discovered stores
";

/// An in-memory DuckDB with the store's table functions registered.
///
/// External access is disabled: the model's SQL can read the mailbox but cannot
/// reach the filesystem or the network, so no query can copy mail out of the
/// process.
fn open_db(store: &Arc<Store>) -> Result<duckdb::Connection, Box<dyn std::error::Error>> {
    let conn = duckdb::Connection::open_in_memory()?;
    vtab::register(&conn, store)?;
    conn.execute_batch("SET enable_external_access = false")?;
    Ok(conn)
}

/// Serve MCP on stdio. Nothing may be printed to stdout after this point — it is
/// the protocol channel; diagnostics go to stderr.
fn serve(
    store: Arc<Store>,
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
