#![windows_subsystem = "windows"]
//! Sizequilt: see where a disk's space went, and take it back.

mod app;

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use sizequilt::scan::Engine;
use sizequilt::shell::HELPER_FLAG;

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if args.first().is_some_and(|a| a == HELPER_FLAG) {
        return helper(&args[1..]);
    }
    match app::run(scan_argument(&args)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

/// `sizequilt <path> [--walker | --mft]`: what to scan on the way in.
/// With neither flag the disk decides.
fn scan_argument(args: &[OsString]) -> Option<(PathBuf, Engine)> {
    let flag = |name: &str| args.iter().any(|a| a == name);
    let engine = if flag("--walker") {
        Engine::Walker
    } else if flag("--mft") {
        Engine::Mft
    } else {
        Engine::Auto
    };
    let root = args.iter().find(|a| !a.to_string_lossy().starts_with("--"))?;
    Some((PathBuf::from(root), engine))
}

/// The elevated, read-only half of a table scan: this same executable, started
/// by the app through the UAC prompt, for one volume, and then gone. The
/// checks are the helper crate's: every argument, and the one file it writes.
fn helper(args: &[OsString]) -> ExitCode {
    let outcome = tm_mft_helper::run(args, &tm_mft_helper::app_temp_folder(), |request| {
        tm_mft::read_volume(&request.root, false).map_err(|e| e.to_string())
    });
    ExitCode::from(outcome.code)
}
