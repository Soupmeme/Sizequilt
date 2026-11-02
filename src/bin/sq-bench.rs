//! `sq-bench <path> [--mft]`: one scan of `path` with no UI, timing each stage.

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use sizequilt::format;
use sizequilt::guard::{Guard, Verdict};
use sizequilt::layout::{self, Metrics, Rect};
use sizequilt::raster;
use sizequilt::scan::{self, Engine, Outcome, Scan};
use sizequilt::{search, shell};
use sizequilt::tree::{Metric, Tree};

/// Asks the guard about every item of a scan, and prints what it would let
/// through: the check that a rule works on a real disk and not just in a test.
fn audit_policy(tree: &Tree) {
    let guard = Guard::system();
    let (mut allowed, mut caution, mut blocked) = (0_u64, 0_u64, 0_u64);
    let mut open: Vec<(u64, u32, bool)> = Vec::new();
    for node in 0..tree.len() as u32 {
        match guard.check_node(tree, node) {
            Verdict::Blocked(_) => blocked += 1,
            Verdict::Caution(_) => {
                caution += 1;
                open.push((tree.size(node), node, true));
            }
            Verdict::Allowed => {
                allowed += 1;
                open.push((tree.size(node), node, false));
            }
        }
    }
    println!(
        "items: {} blocked, {} caution, {} allowed",
        format::count(blocked),
        format::count(caution),
        format::count(allowed)
    );
    let critical = open
        .iter()
        .filter(|(_, n, _)| {
            let name = tree.name(*n).to_lowercase();
            !tree.is_dir(*n) && (name.ends_with(".sys") || name.ends_with(".efi"))
        })
        .count();
    println!("driver or boot files not blocked: {critical}");
    let system = shell::system_folders().system;
    let inside = |node: u32| {
        let path = tree.path(node).to_string_lossy().to_lowercase();
        system.iter().any(|s| path.starts_with(&s.to_lowercase()))
    };
    let escaped = open.iter().filter(|(_, n, _)| inside(*n)).count();
    println!("items inside {system:?} not blocked: {escaped}");
    open.sort_unstable_by_key(|e| std::cmp::Reverse(e.0));
    let shown = open.iter().filter(|(_, n, _)| !tree.is_link(*n));
    let mut listed = 0;
    for (size, node, _) in shown {
        // The full verdict, which looks inside folders.
        let verdict = match guard.check_tree(tree, *node) {
            Verdict::Blocked(why) => format!("blocked by contents: {why}"),
            Verdict::Caution(reasons) => format!("caution x{}", reasons.len()),
            Verdict::Allowed => "allowed".to_owned(),
        };
        println!("{:>9}  {verdict:<26.60}  {}", format::bytes(*size), tree.path(*node).display());
        listed += 1;
        if listed == 28 {
            break;
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let first = args.next().expect("usage: sq-bench <path> [--mft] | --drives");
    if first == "--drives" {
        for d in shell::drives() {
            let plan = scan::choose(d.table_readable(), d.seek_penalty);
            println!("{}: {:?} {:?} -> {plan:?}", d.letter, d.file_system, d.seek_penalty);
            let report = shell::disk_report(&d.root);
            println!("   full speed: {}  {report:?}", report.is_ssd());
        }
        return;
    }
    if first == "--disk" {
        let report = shell::disk_report(&PathBuf::from(args.next().expect("--disk <path>")));
        println!("full speed: {}  {report:?}", report.is_ssd());
        return;
    }
    let policy = first == "--policy";
    let root = PathBuf::from(if policy { args.next().expect("--policy <path>") } else { first });
    let engine = if args.next().as_deref() == Some("--mft") {
        Engine::Mft
    } else {
        Engine::Walker
    };

    let (tx, rx) = mpsc::channel::<()>();
    let scan = Scan::start(root, engine, Metric::Allocated, move || {
        let _ = tx.send(());
    });
    let tree = loop {
        let _ = rx.recv_timeout(Duration::from_millis(250));
        match scan.take() {
            Some(Outcome::Done(tree)) => break tree,
            Some(Outcome::Cancelled) => panic!("cancelled"),
            Some(Outcome::Failed(why)) => panic!("{why}"),
            None => {}
        }
    };
    if policy {
        audit_policy(&tree);
        return;
    }
    let total = scan.started.elapsed().as_secs_f64();
    let f = &tree.facts;
    println!("engine   {}", f.engine);
    if let Some(note) = &f.note {
        println!("note     {note}");
    }
    println!(
        "entries  {} ({} folders), denied {}, refused folders {}",
        format::count(f.entries),
        format::count(f.dirs),
        format::count(f.denied_entries),
        format::count(f.refused_dirs),
    );
    println!(
        "read     {:.0} ms ({:.0} entries/s)",
        f.wall_ms,
        f.entries as f64 * 1000.0 / f.wall_ms
    );
    println!("workers  {} at most", f.workers_peak);
    println!("build    {:.0} ms", f.build_ms);
    println!("total    {total:.3} s");
    println!(
        "size     {} on disk, {} logical",
        format::bytes(tree.allocated(0)),
        format::bytes(tree.logical(0))
    );
    println!(
        "memory   {} of columns ({:.1} bytes/entry)",
        format::bytes(tree.heap_bytes()),
        tree.heap_bytes() as f64 / tree.len() as f64
    );

    let timed = Instant::now();
    let m = Metrics { header: 22.0, pad: 2.0 };
    let (w, h) = (2560_usize, 1440_usize);
    let tiles = layout::layout(&tree, 0, Rect::new(0.0, 0.0, w as f32, h as f32), m);
    let laid = timed.elapsed();
    let canvas = raster::paint(&tree, &tiles, w, h, None);
    println!(
        "map      {} tiles: layout {:.1} ms, paint {:.1} ms at {w}x{h} ({} px)",
        format::count(tiles.len() as u64),
        laid.as_secs_f64() * 1000.0,
        (timed.elapsed() - laid).as_secs_f64() * 1000.0,
        canvas.pixels.len(),
    );

    let timed = Instant::now();
    let found = search::run(&tree, "config");
    println!(
        "search   {} matches in {:.1} ms",
        format::count(found.total),
        timed.elapsed().as_secs_f64() * 1000.0
    );
    let timed = Instant::now();
    let largest = tree.largest_files(1000);
    println!(
        "largest  {} files in {:.1} ms; first: {} ({})",
        largest.len(),
        timed.elapsed().as_secs_f64() * 1000.0,
        tree.path(largest[0]).display(),
        format::bytes(tree.size(largest[0])),
    );
}
