//! Verification harness: open a store and exercise every layer of the reader.
//!
//! Usage: `cargo run -p ost --example dump <path-to-ost-or-pst>`. The path is
//! required here; `ost-mcp` is the thing that finds a store on its own.
//!
//! Subjects and names are truncated, but this still prints real mailbox content;
//! don't redirect it into the repo.

use ost::props::format_time_us;
use ost::store::{
    make_nid, nid_index, nid_type, NID_TYPE_CONTENTS_TABLE, NID_TYPE_NORMAL_FOLDER,
};
use ost::Store;

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: cargo run -p ost --example dump <path-to-ost-or-pst>");
        std::process::exit(2);
    };

    let store = match Store::open(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("open {path} failed: {e}");
            std::process::exit(1);
        }
    };

    println!("path      {path}");
    println!(
        "wVer      {}  pages {} B  mapped {} bytes",
        store.pff.ver,
        store.pff.geom.page,
        store.pff.len()
    );
    println!(
        "btrees    {} blocks, {} nodes",
        store.pff.bbt.len(),
        store.pff.nbt.len()
    );
    println!("store     {:?}", store.display_name());
    match ost::ltp::Pc::open(&store.pff, ost::store::NID_MESSAGE_STORE) {
        Ok(pc) => println!(
            "          message store node has {} properties, display-name prop {:?}",
            pc.props.len(),
            pc.prop(ost::props::pid::DISPLAY_NAME)
        ),
        Err(e) => println!("          message store node unreadable: {e}"),
    }

    let mut folder_nodes = 0;
    for nid in store.pff.nbt.keys() {
        if nid_type(*nid) == NID_TYPE_NORMAL_FOLDER {
            folder_nodes += 1;
        }
    }

    let folders = match store.folders() {
        Ok(f) => f,
        Err(e) => {
            eprintln!("folders failed: {e}");
            std::process::exit(1);
        }
    };
    println!(
        "\nfolders   {} reachable from root ({folder_nodes} folder nodes in the NBT)",
        folders.len()
    );
    let mut by_count = folders.clone();
    by_count.sort_by_key(|f| -f.item_count.unwrap_or(-1));
    for f in by_count.iter().take(8) {
        println!(
            "  {:>7} items  nid=0x{:<7X} {}",
            f.item_count.unwrap_or(-1),
            f.nid,
            f.path
        );
    }

    let Some(inbox) = folders
        .iter()
        .find(|f| f.name.eq_ignore_ascii_case("Inbox"))
    else {
        eprintln!("\nno Inbox in this store; stopping");
        return;
    };

    let rows = match store.messages(inbox.nid) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("\nInbox contents failed: {e}");
            return;
        }
    };
    println!("\ninbox     {} rows from {}", rows.len(), inbox.path);
    for r in rows.iter().take(3) {
        println!(
            "  [{}] {:<24} | {}",
            r.delivered_us.map(format_time_us).unwrap_or_else(|| "?".into()),
            trunc(r.sender_name.as_deref().unwrap_or("-"), 24),
            trunc(r.subject.as_deref().unwrap_or("-"), 44)
        );
    }

    // A message with attachments exercises the deepest path: message property
    // context -> subnode BTree -> attachment table -> attachment property
    // context -> payload block tree. This contents table carries no
    // PidTagHasAttachments column, so probe the messages themselves.
    let with_att = rows
        .iter()
        .take(200)
        .find(|r| store.attachments(r.nid).map(|a| !a.is_empty()).unwrap_or(false))
        .or_else(|| rows.first());
    let Some(row) = with_att else { return };

    match store.message(row.nid) {
        Ok(m) => {
            println!("\nmessage   nid=0x{:X}", m.nid);
            println!("  subject      {}", trunc(m.subject.as_deref().unwrap_or("-"), 60));
            println!("  from         {} <{}>",
                trunc(m.sender_name.as_deref().unwrap_or("-"), 30),
                trunc(m.sender_email.as_deref().unwrap_or("-"), 40));
            println!("  class        {:?}", m.message_class);
            println!("  recipients   {}", m.recipients.len());
            for (label, body) in [
                ("body-plain", &m.body_plain),
                ("body-html", &m.body_html),
                ("body-rtf", &m.body_rtf),
            ] {
                match body {
                    Some(b) => println!("  {label:<12} {} chars, starts {:?}", b.len(), trunc(b.trim_start(), 24)),
                    None => println!("  {label:<12} -"),
                }
            }
            println!("  attachments  {}", m.attachments.len());
            for a in &m.attachments {
                println!(
                    "    nid=0x{:<6X} {:<34} mime={:<24} bytes={:?}",
                    a.nid,
                    trunc(a.filename.as_deref().unwrap_or("<unnamed>"), 34),
                    trunc(a.mime.as_deref().unwrap_or("-"), 24),
                    a.data_len
                );
            }
            // Round-trip the first payload through the public accessor.
            if let Some(a) = m.attachments.first() {
                match store.attachment_bytes(m.nid, a.nid) {
                    Ok(b) => {
                        let head: Vec<String> =
                            b.iter().take(8).map(|x| format!("{x:02X}")).collect();
                        println!("  payload      {} bytes, {}", b.len(), head.join(" "));
                    }
                    Err(e) => println!("  payload      {e}"),
                }
            }
        }
        Err(e) => println!("\nmessage 0x{:X} failed: {e}", row.nid),
    }

    // Coverage sweep over every folder that actually holds items. Search folders
    // (node type 0x03) own no contents table at their own node index, so they are
    // expected to be absent rather than broken.
    let mut rows_read = 0usize;
    let mut ok = 0usize;
    let mut failed = Vec::new();
    let targets: Vec<_> = folders
        .iter()
        .filter(|f| nid_type(f.nid) == NID_TYPE_NORMAL_FOLDER)
        .collect();
    for f in &targets {
        match store.messages(f.nid) {
            Ok(r) => {
                ok += 1;
                rows_read += r.len();
            }
            Err(e) => failed.push(format!("{} ({e})", f.path)),
        }
    }
    println!(
        "\nsweep     {rows_read} rows across {ok}/{} normal folders",
        targets.len()
    );
    for f in failed.iter().take(5) {
        println!("  unreadable: {f}");
    }

    // Every message in the store, to prove the property-context path holds up
    // beyond a hand-picked sample.
    let mut msg_ok = 0usize;
    let mut msg_err: Vec<String> = Vec::new();
    let mut att_total = 0usize;
    let all: Vec<u32> = store
        .pff
        .nbt
        .keys()
        .copied()
        .filter(|n| nid_type(*n) == 0x04)
        .collect();
    for nid in all.iter().copied() {
        match store.message(nid) {
            Ok(m) => {
                msg_ok += 1;
                att_total += m.attachments.len();
            }
            Err(e) => {
                if msg_err.len() < 5 {
                    msg_err.push(format!("0x{nid:X}: {e}"));
                }
            }
        }
    }
    println!(
        "messages  {msg_ok}/{} parsed, {att_total} attachments",
        all.len()
    );
    for e in &msg_err {
        println!("  failed: {e}");
    }

    // The largest contents tables spread their heap over hundreds of blocks, so
    // report each one's size: a HID misdecode shows up here first.
    let mut widest = 0usize;
    for f in &targets {
        let cnid = make_nid(nid_index(f.nid), NID_TYPE_CONTENTS_TABLE);
        if let Ok(node) = store.pff.node(cnid) {
            if let Ok(b) = store.pff.data_blocks(node.bid_data) {
                widest = widest.max(b.len());
            }
        }
    }
    println!("heaps     widest contents-table heap spans {widest} blocks");
}

fn trunc(s: &str, n: usize) -> String {
    let s = s.replace(['\r', '\n', '\t'], " ");
    let c: Vec<char> = s.chars().collect();
    if c.len() <= n {
        s
    } else {
        format!("{}…", c[..n].iter().collect::<String>())
    }
}
