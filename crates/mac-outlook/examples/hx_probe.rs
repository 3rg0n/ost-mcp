//! Verification harness for the `HxStore.hxd` reader: block and field
//! coverage only, never real message content — unlike `crates/ost/examples/
//! dump.rs`, this prints nothing that needs redacting before it can be
//! pasted into an issue or a terminal someone else can see.
//!
//! Usage: `cargo run -p mac-outlook --example hx_probe <path-to-HxStore.hxd>`.
//! Work on a copy: the live file mutates while Outlook runs (see
//! `docs/mac-outlook-format.md`).

use mac_outlook::{hxrecord, hxstore};

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: cargo run -p mac-outlook --example hx_probe <path-to-HxStore.hxd>");
        std::process::exit(2);
    };

    let data = match std::fs::read(&path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("read {path} failed: {e}");
            std::process::exit(1);
        }
    };

    let header = match hxstore::check_header(&data) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    println!(
        "header    version {:?} (known: {}), page size {}",
        header.version as char, header.known_version, header.page_size
    );

    let blocks = hxstore::scan_blocks(&data);
    println!("blocks    {} verified (header CRC, payload CRC, exact inflated length)", blocks.len());

    let inflated: usize = blocks.iter().map(|b| b.data.len()).sum();
    println!("payload   {inflated} bytes decompressed across verified blocks");

    let kinds: std::collections::BTreeMap<u32, usize> = blocks.iter().fold(Default::default(), |mut m, b| {
        *m.entry(b.kind).or_insert(0) += 1;
        m
    });
    println!("kinds     {kinds:?}");

    let raw: Vec<_> = blocks.iter().flat_map(|b| hxrecord::extract(&b.data)).collect();
    println!("records   {} IPM.Note records across all blocks", raw.len());

    let messages = hxrecord::deduplicate(raw);
    let has = |pred: fn(&hxrecord::HxMessage) -> bool| messages.iter().filter(|m| pred(m)).count();
    let n = messages.len().max(1);
    println!("messages  {} distinct (deduplicated by sender + send time)", messages.len());
    println!(
        "coverage  sender {:.1}%  name {:.1}%  subject {:.1}%  preview/body {:.1}%  full html {:.1}%  timestamp {:.1}%",
        100.0 * has(|m| m.sender_address.is_some()) as f64 / n as f64,
        100.0 * has(|m| m.sender_name.is_some()) as f64 / n as f64,
        100.0 * has(|m| m.subject.is_some()) as f64 / n as f64,
        100.0 * has(|m| m.preview.is_some() || m.html.is_some()) as f64 / n as f64,
        100.0 * has(|m| m.html.is_some()) as f64 / n as f64,
        100.0 * has(|m| m.sent_unix.is_some()) as f64 / n as f64,
    );
}
