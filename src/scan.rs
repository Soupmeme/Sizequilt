//! Running a scan: the NTFS master file table when it can be read, the
//! directory walker otherwise, and always something the app can show.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tm_governor::{Budget, Governor, Preset, platform_sampler, platform_signals};
use tm_walk::output::Refusal;
use tm_walk::{
    FastPath, GovernorPacer, ListBuffer, Listed, Lister, Meta, Pacer, WalkError, WalkOptions,
    WalkOutput,
};

use crate::shell;
use crate::tree::{Metric, Tree};

/// How a scan should read the disk.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Engine {
    /// Whichever suits the disk: see [`choose`].
    Auto,
    /// The master file table, with the walker as the fallback.
    Mft,
    /// The directory walker alone.
    Walker,
}

impl Engine {
    /// How the choice reads on screen.
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Automatic",
            Self::Mft => "File table",
            Self::Walker => "Folder walker",
        }
    }
}

/// What [`Engine::Auto`] uses. An SSD answers folder listings so quickly that
/// the walker beats reading the whole table, and spares the administrator
/// prompt. A spinning disk seeks for every folder, which costs minutes, so it
/// reads the table in one pass. A disk that will not say is treated as slow,
/// since guessing wrong that way costs a prompt and not minutes of waiting.
pub fn choose(table_readable: bool, seek_penalty: Option<bool>) -> Engine {
    if table_readable && seek_penalty != Some(false) {
        Engine::Mft
    } else {
        Engine::Walker
    }
}

/// The most workers an SSD walk runs: the highest count measured to pay.
const MAX_SSD_WORKERS: usize = 16;
/// The workers on any disk not proven to be an SSD, and on one that slows down.
const CAUTIOUS_WORKERS: usize = 2;
/// The share of the machine a walk may use, leaving the rest for everything else.
const WALK_CPU_PERCENT: u8 = 75;

/// The workers a walk runs. Every walk gets a fixed count, because the
/// walker's hill-climber is limited only by the CPU's threads and could run all
/// of them against a hard drive. Only a disk that passes
/// [`shell::DiskReport::is_ssd`] gets more than [`CAUTIOUS_WORKERS`].
fn walker_workers(verified_ssd: bool, threads: usize) -> usize {
    if verified_ssd {
        threads.clamp(CAUTIOUS_WORKERS, MAX_SSD_WORKERS)
    } else {
        CAUTIOUS_WORKERS
    }
}

/// A folder listing that takes this long has waited on the disk.
const SLOW_LISTING: Duration = Duration::from_millis(20);
/// The listings counted together.
const BRAKE_WINDOW: u32 = 32;
/// How many slow listings in a window apply the brake. One huge folder is slow
/// on any disk, but this many are a disk seeking or struggling.
const BRAKE_SLOW_LISTINGS: u32 = 8;

/// Judges a disk by how it behaves, because what it reports can be wrong and
/// says nothing of its health. Once on, the brake stays on for the whole scan.
#[derive(Default)]
struct Brake {
    listings: AtomicU32,
    slow: AtomicU32,
    on: AtomicBool,
}

impl Brake {
    fn note(&self, took: Duration) {
        if took >= SLOW_LISTING
            && self.slow.fetch_add(1, Ordering::AcqRel) + 1 >= BRAKE_SLOW_LISTINGS
        {
            self.on.store(true, Ordering::Release);
        }
        if self.listings.fetch_add(1, Ordering::AcqRel) + 1 >= BRAKE_WINDOW {
            self.listings.store(0, Ordering::Release);
            self.slow.store(0, Ordering::Release);
        }
    }

    fn is_on(&self) -> bool {
        self.on.load(Ordering::Acquire)
    }
}

/// A pacer that holds a walk to [`CAUTIOUS_WORKERS`] once the brake is on.
struct BrakedPacer {
    pacer: Arc<dyn Pacer>,
    brake: Arc<Brake>,
}

impl Pacer for BrakedPacer {
    fn on_worker_start(&self) {
        self.pacer.on_worker_start();
    }

    fn throttle(&self, cancelled: &dyn Fn() -> bool) {
        self.pacer.throttle(cancelled);
    }

    fn worker_limit(&self) -> u32 {
        let limit = self.pacer.worker_limit();
        if self.brake.is_on() {
            limit.min(CAUTIOUS_WORKERS as u32)
        } else {
            limit
        }
    }
}

/// What a running scan is doing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Starting = 0,
    /// Waiting on the UAC prompt.
    Elevating = 1,
    /// Reading the master file table; no live counts come from it.
    ReadingTable = 2,
    /// Walking folders; the counters move.
    Walking = 3,
    /// Building the tree from the finished read.
    Building = 4,
    /// The folders are listed; the walker is reading each hard-linked file once.
    CheckingLinks = 5,
}

/// The counters a scan publishes to the UI.
#[derive(Default)]
pub struct Live {
    phase: AtomicU8,
    pub entries: AtomicU64,
    pub bytes: AtomicU64,
    pub cancel: AtomicBool,
    pub current: Mutex<String>,
}

impl Live {
    pub fn phase(&self) -> Phase {
        match self.phase.load(Ordering::Acquire) {
            1 => Phase::Elevating,
            2 => Phase::ReadingTable,
            3 => Phase::Walking,
            4 => Phase::Building,
            5 => Phase::CheckingLinks,
            _ => Phase::Starting,
        }
    }

    fn set_phase(&self, phase: Phase) {
        self.phase.store(phase as u8, Ordering::Release);
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Acquire)
    }
}

/// How a scan ended.
pub enum Outcome {
    Done(Box<Tree>),
    Cancelled,
    Failed(String),
}

/// A scan on its own thread.
pub struct Scan {
    pub root: PathBuf,
    pub live: Arc<Live>,
    pub started: Instant,
    result: Arc<Mutex<Option<Outcome>>>,
}

impl Scan {
    /// Starts scanning `root`. `on_change` runs whenever the UI should look again.
    pub fn start(
        root: PathBuf,
        engine: Engine,
        metric: Metric,
        on_change: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let live = Arc::new(Live::default());
        let result = Arc::new(Mutex::new(None));
        let thread_live = Arc::clone(&live);
        let thread_result = Arc::clone(&result);
        let thread_root = root.clone();
        let on_change = Arc::new(on_change);
        let ticker = Arc::clone(&on_change);
        std::thread::Builder::new()
            .name("scan".to_owned())
            .spawn(move || {
                let outcome = run(thread_root, engine, metric, &thread_live, &*ticker);
                if let Ok(mut slot) = thread_result.lock() {
                    *slot = Some(outcome);
                }
                on_change();
            })
            .expect("the scan thread could not start");
        Self {
            root,
            live,
            started: Instant::now(),
            result,
        }
    }

    pub fn cancel(&self) {
        self.live.cancel.store(true, Ordering::Release);
    }

    /// The outcome, once there is one.
    pub fn take(&self) -> Option<Outcome> {
        self.result.lock().ok()?.take()
    }
}

/// A root path as the engine wants it: `C:` is `C:\`, and no trailing slash
/// otherwise.
pub fn normalize_root(root: &Path) -> PathBuf {
    let text = root.to_string_lossy();
    let trimmed = text.trim_end_matches(['\\', '/']);
    if trimmed.len() == 2 && trimmed.ends_with(':') {
        PathBuf::from(format!("{trimmed}\\"))
    } else {
        PathBuf::from(trimmed)
    }
}

fn run(
    root: PathBuf,
    engine: Engine,
    metric: Metric,
    live: &Live,
    tick: &(dyn Fn() + Send + Sync),
) -> Outcome {
    let root = normalize_root(&root);
    let seek_penalty = shell::seek_penalty(&root);
    let engine = match engine {
        Engine::Auto => choose(tm_mft::precheck(&root).is_ok(), seek_penalty),
        chosen => chosen,
    };
    let mut note = None;
    if engine == Engine::Mft {
        match read_table(&root, live, tick) {
            Ok(out) => return finish(root, out, "NTFS master file table", None, metric, live, tick),
            Err(TableEnd::Cancelled) => return Outcome::Cancelled,
            Err(TableEnd::Fallback(why)) => note = Some(why),
        }
    }
    let threads = std::thread::available_parallelism().map_or(1, usize::from);
    let workers = walker_workers(shell::disk_report(&root).is_ssd(), threads);
    let brake = Arc::new(Brake::default());
    match walk(&root, workers, &brake, live, tick) {
        Ok(out) => {
            let name = match out.stats.fast_path {
                FastPath::ExtdDirInfo => "Directory walker",
                _ => "Directory walker (per-entry)",
            };
            if brake.is_on() {
                let eased = format!("the disk answered slowly, so the scan eased off to {CAUTIOUS_WORKERS} workers");
                note = Some(note.map_or(eased.clone(), |why| format!("{why}; {eased}")));
            }
            finish(root, out, name, note, metric, live, tick)
        }
        Err(WalkError::Cancelled) => Outcome::Cancelled,
        Err(e) => Outcome::Failed(e.to_string()),
    }
}

fn finish(
    root: PathBuf,
    out: WalkOutput,
    engine: &str,
    note: Option<String>,
    metric: Metric,
    live: &Live,
    tick: &(dyn Fn() + Send + Sync),
) -> Outcome {
    live.entries.store(out.stats.entries, Ordering::Release);
    live.set_phase(Phase::Building);
    tick();
    Outcome::Done(Box::new(Tree::build(
        root,
        out,
        engine.to_owned(),
        note,
        metric,
    )))
}

/// How often a walk reads its root on its own: once at the start and once at
/// the end. Every read after those is a hard-link family's.
const ROOT_READS: u64 = 2;

/// The platform's lister, watched from outside. Its single-path reads are the
/// only sign that the walker has moved on from listing to its hard links, and
/// its listing times tell the [`Brake`] how the disk is coping.
struct Watched {
    lister: Arc<dyn Lister>,
    reads: Arc<AtomicU64>,
    brake: Arc<Brake>,
}

impl Lister for Watched {
    fn stat_dir(&self, path: &Path, want_atime: bool) -> Result<Meta, Refusal> {
        self.reads.fetch_add(1, Ordering::Release);
        self.lister.stat_dir(path, want_atime)
    }

    fn list(
        &self,
        dir: &Path,
        want_atime: bool,
        buf: &mut ListBuffer,
    ) -> Result<FastPath, Refusal> {
        let started = Instant::now();
        let listed = self.lister.list(dir, want_atime, buf);
        self.brake.note(started.elapsed());
        listed
    }

    fn list_until(
        &self,
        dir: &Path,
        want_atime: bool,
        buf: &mut ListBuffer,
        limit: usize,
    ) -> Result<Listed, Refusal> {
        self.lister.list_until(dir, want_atime, buf, limit)
    }

    fn list_more(&self, buf: &mut ListBuffer, limit: usize) -> Result<Listed, Refusal> {
        self.lister.list_more(buf, limit)
    }
}

/// The directory walk, at Turbo: a person is waiting on this scan. `workers`
/// is what [`walker_workers`] chose.
fn walk(
    root: &Path,
    workers: usize,
    brake: &Arc<Brake>,
    live: &Live,
    tick: &(dyn Fn() + Send + Sync),
) -> Result<WalkOutput, WalkError> {
    live.set_phase(Phase::Walking);
    let governor = Arc::new(Governor::start(
        Budget {
            preset: Preset::Turbo,
            cpu_percent: Some(WALK_CPU_PERCENT),
            auto_base: Preset::Turbo,
        },
        false,
        platform_sampler(),
        platform_signals(),
    ));
    let mut options = WalkOptions::new(root);
    options.max_workers = workers;
    let reads = Arc::new(AtomicU64::new(0));
    let lister = Arc::new(Watched {
        lister: tm_walk::lister_for(&options)?,
        reads: Arc::clone(&reads),
        brake: Arc::clone(brake),
    });
    let pacer = Arc::new(BrakedPacer {
        pacer: Arc::new(GovernorPacer::new(Arc::clone(&governor))),
        brake: Arc::clone(brake),
    });
    let handle = tm_walk::start_with(options, pacer, lister)?;
    loop {
        let progress = handle.progress();
        if reads.load(Ordering::Acquire) > ROOT_READS {
            live.set_phase(Phase::CheckingLinks);
        }
        live.entries.store(progress.entries, Ordering::Release);
        live.bytes.store(progress.bytes, Ordering::Release);
        if let (Some(path), Ok(mut slot)) = (progress.current_path, live.current.lock()) {
            *slot = path;
        }
        tick();
        if progress.done {
            break;
        }
        if live.cancelled() {
            handle.cancel();
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let out = handle.take();
    governor.stop();
    out
}

enum TableEnd {
    Cancelled,
    /// The walker takes over; the text says why, for the status bar.
    Fallback(String),
}

fn read_table(
    root: &Path,
    live: &Live,
    tick: &(dyn Fn() + Send + Sync),
) -> Result<WalkOutput, TableEnd> {
    if let Err(e) = tm_mft::precheck(root) {
        return Err(TableEnd::Fallback(e.to_string()));
    }
    if shell::is_elevated() {
        live.set_phase(Phase::ReadingTable);
        tick();
        return tm_mft::read_volume(root, false).map_err(|e| TableEnd::Fallback(e.to_string()));
    }
    read_table_elevated(root, live, tick)
}

/// Runs this same executable as the elevated, read-only helper, and takes its
/// columns file.
fn read_table_elevated(
    root: &Path,
    live: &Live,
    tick: &(dyn Fn() + Send + Sync),
) -> Result<WalkOutput, TableEnd> {
    use tm_mft::columns::{ColumnsFile, OUTPUT_EXTENSION, decode};

    let folder = tm_mft_helper::app_temp_folder();
    std::fs::create_dir_all(&folder)
        .map_err(|e| TableEnd::Fallback(format!("the temp folder could not be made: {e}")))?;
    let name = format!(
        "scan-{}-{}{OUTPUT_EXTENSION}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    );
    let output = folder.join(name);
    let volume: String = root.to_string_lossy().chars().take(2).collect();

    live.set_phase(Phase::Elevating);
    tick();
    let helper = shell::start_elevated_helper(&volume, root, &output).map_err(|e| match e {
        shell::ElevateError::Declined => {
            TableEnd::Fallback("the administrator prompt was declined".to_owned())
        }
        shell::ElevateError::Failed(why) => {
            TableEnd::Fallback(format!("the elevated helper could not start: {why}"))
        }
    })?;

    live.set_phase(Phase::ReadingTable);
    tick();
    while !helper.wait(Duration::from_millis(100)) {
        if live.cancelled() {
            helper.terminate();
            let _ = std::fs::remove_file(&output);
            return Err(TableEnd::Cancelled);
        }
        tick();
    }

    let bytes = std::fs::read(&output);
    let _ = std::fs::remove_file(&output);
    let bytes = bytes.map_err(|e| {
        TableEnd::Fallback(format!("the helper's output could not be read: {e}"))
    })?;
    match decode(&bytes) {
        Ok(ColumnsFile::Columns(out)) => Ok(*out),
        Ok(ColumnsFile::Refusal(why)) => Err(TableEnd::Fallback(why)),
        Err(e) => Err(TableEnd::Fallback(e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_walks_an_ssd_and_reads_the_table_of_a_hard_drive() {
        assert_eq!(choose(true, Some(false)), Engine::Walker);
        assert_eq!(choose(true, Some(true)), Engine::Mft);
    }

    #[test]
    fn automatic_treats_an_unknown_disk_as_slow() {
        assert_eq!(choose(true, None), Engine::Mft);
    }

    #[test]
    fn automatic_walks_whatever_has_no_readable_table() {
        for penalty in [Some(true), Some(false), None] {
            assert_eq!(choose(false, penalty), Engine::Walker);
        }
    }

    #[test]
    fn only_a_verified_ssd_gets_more_than_the_cautious_count() {
        assert_eq!(walker_workers(true, 8), 8);
        assert_eq!(walker_workers(true, 32), MAX_SSD_WORKERS);
        assert_eq!(walker_workers(true, 1), CAUTIOUS_WORKERS);
        for threads in [1, 8, 32, 128] {
            assert_eq!(walker_workers(false, threads), CAUTIOUS_WORKERS);
        }
    }

    /// A folder of `count` empty subfolders; the caller removes it.
    fn folders(name: &str, count: usize) -> PathBuf {
        let folder = std::env::temp_dir().join(format!("sq-{name}-{}", std::process::id()));
        for i in 0..count {
            std::fs::create_dir_all(folder.join(i.to_string())).unwrap();
        }
        folder
    }

    #[test]
    fn a_capped_walk_never_runs_more_workers_than_its_cap() {
        let folder = folders("cap", 200);
        let brake = Arc::new(Brake::default());
        let walked = walk(&folder, CAUTIOUS_WORKERS, &brake, &Live::default(), &|| {});
        std::fs::remove_dir_all(&folder).unwrap();
        assert!(walked.unwrap().stats.workers_peak <= CAUTIOUS_WORKERS as u32);
    }

    #[test]
    fn the_brake_needs_many_slow_listings_and_then_stays_on() {
        let slow = SLOW_LISTING;
        let fast = Duration::from_micros(100);
        let brake = Brake::default();
        for _ in 0..3 {
            for i in 0..BRAKE_WINDOW {
                brake.note(if i < BRAKE_SLOW_LISTINGS - 1 { slow } else { fast });
            }
        }
        assert!(!brake.is_on(), "a few huge folders are not a slow disk");
        for _ in 0..BRAKE_SLOW_LISTINGS {
            brake.note(slow);
        }
        assert!(brake.is_on());
        for _ in 0..BRAKE_WINDOW * 4 {
            brake.note(fast);
        }
        assert!(brake.is_on(), "a disk that was slow stays under suspicion");
    }

    /// Allows eight workers and does nothing else.
    struct OpenPacer;

    impl Pacer for OpenPacer {
        fn on_worker_start(&self) {}
        fn throttle(&self, _cancelled: &dyn Fn() -> bool) {}
        fn worker_limit(&self) -> u32 {
            8
        }
    }

    /// A real lister over a disk that takes a long time, recording how many
    /// listings ran at once after the brake had had time to act.
    struct SlowDisk {
        lister: Arc<dyn Lister>,
        brake: Arc<Brake>,
        running: AtomicU32,
        since_brake: AtomicU32,
        peak: AtomicU32,
    }

    impl Lister for SlowDisk {
        fn stat_dir(&self, path: &Path, want_atime: bool) -> Result<Meta, Refusal> {
            self.lister.stat_dir(path, want_atime)
        }

        fn list(
            &self,
            dir: &Path,
            want_atime: bool,
            buf: &mut ListBuffer,
        ) -> Result<FastPath, Refusal> {
            let running = self.running.fetch_add(1, Ordering::AcqRel) + 1;
            // The eight already listing when the brake came on are let finish.
            if self.brake.is_on() && self.since_brake.fetch_add(1, Ordering::AcqRel) >= 8 {
                self.peak.fetch_max(running, Ordering::AcqRel);
            }
            std::thread::sleep(SLOW_LISTING);
            let listed = self.lister.list(dir, want_atime, buf);
            self.running.fetch_sub(1, Ordering::AcqRel);
            listed
        }
    }

    #[test]
    fn a_slow_disk_brings_a_running_walk_down_to_the_cautious_count() {
        let folder = folders("brake", 120);
        let brake = Arc::new(Brake::default());
        let mut options = WalkOptions::new(&folder);
        options.max_workers = 8;
        let disk = Arc::new(SlowDisk {
            lister: tm_walk::lister_for(&options).unwrap(),
            brake: Arc::clone(&brake),
            running: AtomicU32::new(0),
            since_brake: AtomicU32::new(0),
            peak: AtomicU32::new(0),
        });
        let lister = Arc::new(Watched {
            lister: Arc::clone(&disk) as Arc<dyn Lister>,
            reads: Arc::new(AtomicU64::new(0)),
            brake: Arc::clone(&brake),
        });
        let pacer = Arc::new(BrakedPacer {
            pacer: Arc::new(OpenPacer),
            brake: Arc::clone(&brake),
        });
        let walked = tm_walk::start_with(options, pacer, lister).unwrap().take();
        std::fs::remove_dir_all(&folder).unwrap();
        walked.unwrap();
        assert!(brake.is_on());
        let peak = disk.peak.load(Ordering::Acquire);
        assert!(peak > 0 && peak <= CAUTIOUS_WORKERS as u32, "peak {peak}");
    }

    /// The phase a walk of `name` ends in, after `fill` has made its folder.
    fn phase_after_walking(name: &str, fill: impl Fn(&Path)) -> Phase {
        let folder = std::env::temp_dir().join(format!("sq-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        fill(&folder);
        let live = Live::default();
        let brake = Arc::new(Brake::default());
        let walked = walk(&folder, CAUTIOUS_WORKERS, &brake, &live, &|| {});
        std::fs::remove_dir_all(&folder).unwrap();
        walked.unwrap();
        live.phase()
    }

    #[test]
    fn hard_links_get_a_phase_of_their_own() {
        let phase = phase_after_walking("links", |folder| {
            std::fs::write(folder.join("a.txt"), "a").unwrap();
            std::fs::hard_link(folder.join("a.txt"), folder.join("b.txt")).unwrap();
        });
        assert_eq!(phase, Phase::CheckingLinks);
    }

    #[test]
    fn a_folder_without_hard_links_stays_walking() {
        let phase = phase_after_walking("plain", |folder| {
            std::fs::write(folder.join("a.txt"), "a").unwrap();
            std::fs::write(folder.join("b.txt"), "b").unwrap();
        });
        assert_eq!(phase, Phase::Walking);
    }

    #[test]
    fn roots_are_normalized() {
        assert_eq!(normalize_root(Path::new("C:")), PathBuf::from("C:\\"));
        assert_eq!(normalize_root(Path::new("C:\\")), PathBuf::from("C:\\"));
        assert_eq!(
            normalize_root(Path::new("C:\\Users\\me\\")),
            PathBuf::from("C:\\Users\\me")
        );
    }
}
