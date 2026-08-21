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
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: cargo run -p mac-outlook --example hx_probe <path-to-HxStore.hxd> [--layout-near <unix_seconds>]");
        std::process::exit(2);
    };
    let layout_near: Option<i64> = match args.next().as_deref() {
        Some("--layout-near") => Some(args.next().and_then(|s| s.parse().ok()).unwrap_or_else(|| {
            eprintln!("--layout-near needs a unix-seconds value");
            std::process::exit(2);
        })),
        _ => None,
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

    if let Some(target) = layout_near {
        // Redacted structural dump only: byte offset relative to the anchor,
        // whether a field parses as an email or a person name, and its
        // length — never the field's actual text.
        let mut best: Option<(i64, Vec<hxrecord::FieldShape>)> = None;
        for b in &blocks {
            for (rec, shapes) in hxrecord::extract_with_shapes(&b.data) {
                if let Some(t) = rec.sent_unix {
                    let dist = (t - target).abs();
                    if best.as_ref().map(|(d, _)| dist < *d).unwrap_or(true) {
                        best = Some((dist, shapes));
                    }
                }
            }
        }
        match best {
            Some((dist, shapes)) => {
                println!("layout    nearest record found, {dist}s from target");
                for s in shapes {
                    println!(
                        "  rel={:>6}  email={:<5}  person_name={:<5}  chars={}",
                        s.rel, s.is_email, s.is_person_name, s.char_len
                    );
                }
            }
            None => println!("layout    no record with a timestamp found"),
        }
        return;
    }

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
