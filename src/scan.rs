//! Running a scan: the NTFS master file table when it can be read, the
//! directory walker otherwise, and always something the app can show.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tm_governor::{Budget, Governor, Preset, platform_sampler, platform_signals};
use tm_walk::{FastPath, WalkError, WalkOptions, WalkOutput};

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
    let engine = match engine {
        Engine::Auto => choose(tm_mft::precheck(&root).is_ok(), shell::seek_penalty(&root)),
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
    match walk(&root, live, tick) {
        Ok(out) => {
            let name = match out.stats.fast_path {
                FastPath::ExtdDirInfo => "Directory walker",
                _ => "Directory walker (per-entry)",
            };
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

/// The directory walk, at Turbo: a person is waiting on this scan.
fn walk(
    root: &Path,
    live: &Live,
    tick: &(dyn Fn() + Send + Sync),
) -> Result<WalkOutput, WalkError> {
    live.set_phase(Phase::Walking);
    let governor = Arc::new(Governor::start(
        Budget {
            preset: Preset::Turbo,
            cpu_percent: None,
            auto_base: Preset::Turbo,
        },
        false,
        platform_sampler(),
        platform_signals(),
    ));
    let handle = tm_walk::start(WalkOptions::new(root), Arc::clone(&governor))?;
    loop {
        let progress = handle.progress();
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
    fn roots_are_normalized() {
        assert_eq!(normalize_root(Path::new("C:")), PathBuf::from("C:\\"));
        assert_eq!(normalize_root(Path::new("C:\\")), PathBuf::from("C:\\"));
        assert_eq!(
            normalize_root(Path::new("C:\\Users\\me\\")),
            PathBuf::from("C:\\Users\\me")
        );
    }
}
