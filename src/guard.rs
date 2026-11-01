//! What may leave the disk, and the one way it can.
//!
//! Every move to the Recycle Bin passes through here, and the compiler holds
//! that: `shell::recycle` takes an [`Approved`], and only [`Guard::approve`] can
//! make one. There is no batch path, no rule engine and no way to act without
//! the person's own confirmation of one named item.
//!
//! A path gets one of three verdicts:
//!
//! * **Blocked**: the system, programs, boot files and the folders that hold a
//!   profile together. Nothing in the app overrides this. Anything reached
//!   through a link, or whose name Windows would read as something else, is
//!   blocked too, because the path may not mean what the screen says.
//! * **Caution**: things that can be moved but are easy to regret. The person
//!   must acknowledge the listed reasons first.
//! * **Allowed**: ordinary files, still confirmed one at a time.
//!
//! The rules fail closed: a path this module cannot read is blocked.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::format;
use crate::shell::{self, Recycled};
use crate::tree::Tree;

/// The folder names that stay untouched, with everything in them, wherever
/// they sit at the top of any drive: an old install on a second drive counts.
const SYSTEM_TREES: [&str; 13] = [
    "windows",
    "windows.old",
    "program files",
    "program files (x86)",
    "program files (arm)",
    "boot",
    "efi",
    "recovery",
    "system volume information",
    "documents and settings",
    "windowsapps",
    "wpsystem",
    "msocache",
];
/// Profiles Windows itself owns.
const SYSTEM_PROFILES: [&str; 4] = ["public", "default", "default user", "all users"];
/// A profile's standard folders: their contents are yours, the folder is not.
const PROFILE_FOLDERS: [&str; 12] = [
    "desktop",
    "documents",
    "downloads",
    "pictures",
    "music",
    "videos",
    "saved games",
    "contacts",
    "favorites",
    "links",
    "searches",
    "onedrive",
];
/// Files that stop a machine booting or working, anywhere they appear.
const CRITICAL_FILES: [&str; 8] = [
    "bootmgr",
    "bootmgfw.efi",
    "bootnxt",
    "bcd",
    "ntldr",
    "ntdetect.com",
    "boot.ini",
    "usrclass.dat",
];
/// Driver and firmware types, anywhere they appear.
const CRITICAL_TYPES: [&str; 4] = ["sys", "efi", "drv", "cpl"];
/// Folders only Windows manages, however deep they are found.
const CRITICAL_FOLDERS: [&str; 2] = ["$recycle.bin", "system volume information"];
const PROGRAM_TYPES: [&str; 4] = ["exe", "dll", "msi", "msix"];
const KEY_TYPES: [&str; 7] = ["kdbx", "pfx", "p12", "pem", "key", "ppk", "wallet"];
const KEY_FILES: [&str; 4] = ["id_rsa", "id_ed25519", "id_ecdsa", "wallet.dat"];
/// Names Windows treats as devices, in any folder and with any extension.
const DEVICE_NAMES: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];
/// The shell's move cannot address a longer path.
const MAX_SHELL_PATH: usize = 259;
const LARGE_BYTES: u64 = 10 * 1024 * 1024 * 1024;
const LARGE_FILES: u32 = 10_000;
/// How far a file's time may drift from the scan's before it counts as changed.
const MTIME_TOLERANCE_MS: f64 = 2000.0;

/// What may happen to a path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Allowed,
    /// May be moved once the person has acknowledged these reasons.
    Caution(Vec<String>),
    /// May not be moved from this app. The text says why.
    Blocked(String),
}

impl Verdict {
    pub fn is_blocked(&self) -> bool {
        matches!(self, Self::Blocked(_))
    }
}

/// A folder of this machine that the fixed names would not catch: Windows
/// installed elsewhere, or a profile folder moved to another drive.
struct Rule {
    path: String,
    /// Whether everything inside is protected as well as the folder.
    inside: bool,
    why: String,
}

/// The protections of this machine.
pub struct Guard {
    rules: Vec<Rule>,
    /// The drive Windows runs from, lower-case with its colon.
    system_drive: Option<String>,
    /// This person's profile, lower-case; other profiles draw a caution.
    profile: Option<String>,
    /// Synced folders: removing from them removes from the cloud as well.
    cloud: Vec<String>,
    elevated: bool,
    log: Option<PathBuf>,
}

/// The form rules compare: lower-case, backslashes, no trailing separator.
fn key(path: &str) -> String {
    path.replace('/', "\\")
        .to_lowercase()
        .trim_end_matches('\\')
        .to_owned()
}

/// Splits a path into lower-case components, refusing anything that is not a
/// plain, absolute drive path whose every name means what it says.
fn parts(path: &str) -> Result<Vec<String>, String> {
    if path.chars().count() > MAX_SHELL_PATH {
        return Err("the path is too long for the Recycle Bin to handle safely".to_owned());
    }
    let unusual = |c: char| {
        c.is_control() || matches!(c, '*' | '?' | '"' | '<' | '>' | '|' | '\u{FFFD}')
    };
    if path.chars().any(unusual) {
        return Err("its name holds characters Windows could read as something else".to_owned());
    }
    let key = key(path);
    let mut components: Vec<String> = key.split('\\').map(str::to_owned).collect();
    let drive = components.first().map(String::as_bytes);
    let is_drive = matches!(drive, Some([letter, b':']) if letter.is_ascii_alphabetic());
    if !is_drive {
        return Err("only paths on a drive letter can be moved".to_owned());
    }
    for name in components.iter().skip(1) {
        let odd = name.is_empty()
            || name == "."
            || name == ".."
            || name.ends_with('.')
            || name.ends_with(' ')
            || name.contains(':');
        if odd {
            return Err("its name is one Windows would read as a different file".to_owned());
        }
        let stem = name.split('.').next().unwrap_or(name);
        if DEVICE_NAMES.contains(&stem) {
            return Err("its name is reserved by Windows for a device".to_owned());
        }
    }
    components.shrink_to_fit();
    Ok(components)
}

/// Why the location of a path is off limits, whatever is in it.
fn system_location(c: &[String]) -> Option<String> {
    let top = c.get(1)?.as_str();
    if SYSTEM_TREES.contains(&top) || top.starts_with('$') {
        return Some(format!("{top} belongs to Windows or to installed programs"));
    }
    match top {
        "programdata" if c.len() == 2 => {
            Some("ProgramData is where installed programs keep what they need".to_owned())
        }
        "programdata" if c[2] == "microsoft" => {
            Some("this is Windows' own data, including Defender and encryption keys".to_owned())
        }
        "users" => profile_location(c),
        _ => None,
    }
}

fn profile_location(c: &[String]) -> Option<String> {
    let Some(user) = c.get(2) else {
        return Some("this folder holds every profile on the machine".to_owned());
    };
    if SYSTEM_PROFILES.contains(&user.as_str()) {
        return Some("this profile belongs to Windows".to_owned());
    }
    let Some(inside) = c.get(3) else {
        return Some("this is a whole user profile".to_owned());
    };
    if matches!(inside.as_str(), ".ssh" | ".gnupg") {
        return Some("this holds login and encryption keys".to_owned());
    }
    if inside == "appdata" {
        return match (c.get(4), c.get(5), c.get(6)) {
            (None | Some(_), None, _) => {
                Some("this holds the settings of every app you use".to_owned())
            }
            (Some(_), Some(app), _) if app == "microsoft" => {
                Some("this holds Windows' per-user data, including saved credentials".to_owned())
            }
            (Some(_), Some(app), None) if app == "packages" => {
                Some("this holds every Store app's data".to_owned())
            }
            _ => None,
        };
    }
    (c.len() == 4 && PROFILE_FOLDERS.contains(&inside.as_str()))
        .then(|| "this is a standard folder of your profile; its contents can be moved".to_owned())
}

/// Why a name is off limits wherever it appears, given in lower case.
fn critical_name(name: &str) -> Option<String> {
    if CRITICAL_FILES.contains(&name) || name.starts_with("ntuser.") {
        return Some(format!("{name} is a file Windows needs to start or to sign you in"));
    }
    if CRITICAL_FOLDERS.contains(&name) {
        return Some(format!("{name} is managed by Windows"));
    }
    let extension = name.rsplit_once('.').map(|(_, e)| e)?;
    CRITICAL_TYPES
        .contains(&extension)
        .then(|| format!("{name} is a driver or system file"))
}

fn is_program(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, e)| PROGRAM_TYPES.contains(&e))
}

fn is_key(name: &str) -> bool {
    KEY_FILES.contains(&name)
        || name.rsplit_once('.').is_some_and(|(_, e)| KEY_TYPES.contains(&e))
}

impl Guard {
    /// The protections of this machine, from what Windows itself reports.
    pub fn system() -> Self {
        let folders = shell::system_folders();
        let mut rules = Vec::new();
        for path in &folders.system {
            rules.push(Rule {
                path: key(path),
                inside: true,
                why: "this belongs to Windows or to installed programs".to_owned(),
            });
        }
        for path in &folders.personal {
            rules.push(Rule {
                path: key(path),
                inside: false,
                why: "this is a standard folder of your profile; its contents can be moved"
                    .to_owned(),
            });
        }
        if let Ok(exe) = std::env::current_exe() {
            rules.push(Rule {
                path: key(&exe.to_string_lossy()),
                inside: false,
                why: "this is Sizequilt itself".to_owned(),
            });
        }
        Self {
            rules,
            system_drive: folders.system.first().and_then(|w| key(w).get(..2).map(str::to_owned)),
            profile: folders.profile.as_deref().map(key),
            cloud: folders.cloud.iter().map(|p| key(p)).collect(),
            elevated: shell::is_elevated(),
            log: folders.local_data.map(|d| {
                PathBuf::from(d).join("Sizequilt").join("actions.log")
            }),
        }
    }

    /// Where the record of every move is kept.
    pub fn log_path(&self) -> Option<&Path> {
        self.log.as_deref()
    }

    /// The verdict on a path alone. Reads nothing from disk.
    pub fn check_path(&self, path: &str) -> Verdict {
        let c = match parts(path) {
            Ok(c) => c,
            Err(why) => return Verdict::Blocked(why),
        };
        if c.len() < 2 {
            return Verdict::Blocked("this is a whole drive".to_owned());
        }
        if let Some(why) = system_location(&c) {
            return Verdict::Blocked(why);
        }
        let name = c.last().map_or("", String::as_str);
        if let Some(why) = critical_name(name) {
            return Verdict::Blocked(why);
        }
        let joined = c.join("\\");
        for rule in &self.rules {
            let below = rule.inside && joined.starts_with(&format!("{}\\", rule.path));
            if joined == rule.path || below {
                return Verdict::Blocked(rule.why.clone());
            }
            if rule.path.starts_with(&format!("{joined}\\")) {
                return Verdict::Blocked(format!("it contains {}", rule.path));
            }
        }

        let mut reasons = Vec::new();
        if is_program(name) {
            reasons.push("This is a program file. Removing it can break the app it belongs to; uninstalling is safer.".to_owned());
        }
        if is_key(name) {
            reasons.push("This looks like a key or password file, which may not be replaceable.".to_owned());
        }
        if c[1] == "programdata" {
            reasons.push("Installed programs keep what they need in ProgramData.".to_owned());
        }
        if c.len() == 2 && self.system_drive.as_deref() == Some(c[0].as_str()) {
            reasons.push("This sits at the top of the Windows drive, where installers and drivers put things.".to_owned());
        }
        if c[1] == "users" {
            if c.get(3).is_some_and(|f| f == "appdata") {
                reasons.push("This is in AppData: an app may lose its settings or saved data.".to_owned());
            }
            let own = self.profile.as_ref().is_some_and(|p| joined.starts_with(&format!("{p}\\")));
            if self.profile.is_some() && !own {
                reasons.push("This belongs to another user on this machine.".to_owned());
            }
        }
        if self.cloud.iter().any(|f| joined == *f || joined.starts_with(&format!("{f}\\"))) {
            reasons.push("This is in a synced folder, so it will also be removed from the cloud and your other devices.".to_owned());
        }
        if self.elevated {
            reasons.push("Sizequilt is running as administrator, so Windows' own permissions will not stop this.".to_owned());
        }
        if reasons.is_empty() {
            Verdict::Allowed
        } else {
            Verdict::Caution(reasons)
        }
    }

    /// The verdict on a node from what the scan knows, without looking inside
    /// a folder: quick enough for every frame.
    pub fn check_node(&self, tree: &Tree, node: u32) -> Verdict {
        if node == 0 {
            return Verdict::Blocked("this is the folder that was scanned".to_owned());
        }
        if tree.is_link(node) {
            return Verdict::Blocked(
                "this is a link to somewhere else; removing it frees nothing".to_owned(),
            );
        }
        self.check_path(&tree.path(node).to_string_lossy())
    }

    /// The full verdict on a node: [`Guard::check_node`], then everything
    /// inside it. A folder is only as movable as what it holds.
    pub fn check_tree(&self, tree: &Tree, node: u32) -> Verdict {
        let verdict = self.check_node(tree, node);
        let mut reasons = match verdict {
            Verdict::Blocked(_) => return verdict,
            Verdict::Caution(reasons) => reasons,
            Verdict::Allowed => Vec::new(),
        };
        if tree.is_dir(node) {
            let mut stack = tree.children(node).to_vec();
            let (mut programs, mut keys, mut repositories) = (0_u64, 0_u64, 0_u64);
            while let Some(at) = stack.pop() {
                let name = String::from_utf8_lossy(tree.name_bytes(at)).to_lowercase();
                if let Some(why) = critical_name(&name) {
                    return Verdict::Blocked(format!("it contains {}", why));
                }
                if tree.is_dir(at) {
                    repositories += u64::from(name == ".git");
                    stack.extend_from_slice(tree.children(at));
                } else {
                    programs += u64::from(is_program(&name));
                    keys += u64::from(is_key(&name));
                }
            }
            if programs > 0 {
                reasons.push(format!(
                    "This holds {} program files, so it may be an installed app. Uninstalling is safer.",
                    format::count(programs)
                ));
            }
            if keys > 0 {
                reasons.push(format!(
                    "This holds {} files that look like keys or passwords.",
                    format::count(keys)
                ));
            }
            if repositories > 0 {
                reasons.push(format!(
                    "This holds {} git repositories, which may have unpushed work.",
                    format::count(repositories)
                ));
            }
            if tree.allocated(node) >= LARGE_BYTES || tree.files_under(node) >= LARGE_FILES {
                reasons.push(format!(
                    "This is a large move: {} in {} files.",
                    format::bytes(tree.allocated(node)),
                    format::count(u64::from(tree.files_under(node)))
                ));
            }
        }
        if reasons.is_empty() {
            Verdict::Allowed
        } else {
            Verdict::Caution(reasons)
        }
    }

    /// Checks a request against the disk as it is now. This is the only
    /// way to make an [`Approved`].
    pub fn approve(&self, request: &Request, acknowledged: bool) -> Result<Approved, String> {
        match &request.verdict {
            Verdict::Blocked(why) => return Err(why.clone()),
            Verdict::Caution(_) if !acknowledged => {
                return Err("the warnings were not acknowledged".to_owned());
            }
            _ => {}
        }
        let shown = request.path.to_string_lossy();
        if let Verdict::Blocked(why) = self.check_path(&shown) {
            return Err(why);
        }
        let meta = std::fs::symlink_metadata(&request.path)
            .map_err(|_| "it is no longer where the scan found it. Rescan first".to_owned())?;
        if shell::is_reparse_point(&meta) {
            return Err("this is a link to somewhere else".to_owned());
        }
        if meta.is_dir() != request.is_dir {
            return Err("it has changed since the scan. Rescan first".to_owned());
        }
        // Resolved, a path through a junction leads somewhere other than
        // where it reads, so the checks above were about the wrong place.
        let real = std::fs::canonicalize(&request.path)
            .map_err(|e| format!("its real location could not be confirmed: {e}"))?;
        let real = real.to_string_lossy();
        let real = real.strip_prefix(r"\\?\").unwrap_or(&real);
        if key(real) != key(&shown) {
            return Err(format!("this path really leads to {real}"));
        }
        if let Verdict::Blocked(why) = self.check_path(real) {
            return Err(why);
        }
        if !request.is_dir {
            let changed_size = request.logical > 0 && meta.len() != request.logical;
            let now_ms = meta.modified().ok().and_then(|t| {
                t.duration_since(std::time::UNIX_EPOCH).ok()
            }).map(|d| d.as_secs_f64() * 1000.0);
            let changed_time = request.mtime_ms.is_finite()
                && now_ms.is_some_and(|t| (t - request.mtime_ms).abs() > MTIME_TOLERANCE_MS);
            if changed_size || changed_time {
                return Err("the file has changed since the scan. Rescan first".to_owned());
            }
        }
        if let Some(why) = shell::recycle_refusal(&request.path, request.allocated) {
            return Err(why);
        }
        Ok(Approved {
            path: request.path.clone(),
        })
    }

    /// Moves one confirmed item to the Recycle Bin, re-checking it first and
    /// recording what happened. Nothing moves unless the record can be written.
    pub fn move_to_recycle_bin(&self, request: &Request, acknowledged: bool) -> Recycled {
        let shown = request.path.display().to_string();
        let approved = match self.approve(request, acknowledged) {
            Ok(approved) => approved,
            Err(why) => {
                // Refused, nothing moved, so a failed record changes nothing.
                let _ = self.record("refused", &format!("{shown}\t{why}"));
                return Recycled::Refused(why);
            }
        };
        let size = request.allocated;
        if let Err(why) = self.record("moving", &format!("{shown}\t{size}")) {
            return Recycled::Refused(format!("the action log could not be written: {why}"));
        }
        let outcome = shell::recycle(&approved);
        let _ = self.record(outcome.word(), &shown);
        outcome
    }

    fn record(&self, what: &str, detail: &str) -> Result<(), String> {
        let path = self.log.as_ref().ok_or("no location for it")?;
        if let Some(folder) = path.parent() {
            std::fs::create_dir_all(folder).map_err(|e| e.to_string())?;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64() * 1000.0);
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| e.to_string())?;
        writeln!(file, "{}\t{what}\t{detail}", format::timestamp(now)).map_err(|e| e.to_string())
    }
}

/// One item the person has asked to move, as the scan saw it.
#[derive(Clone, Debug)]
pub struct Request {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub logical: u64,
    pub allocated: u64,
    pub files: u32,
    pub mtime_ms: f64,
    pub verdict: Verdict,
}

impl Request {
    /// A request for `node`, with its full verdict.
    pub fn new(guard: &Guard, tree: &Tree, node: u32) -> Self {
        Self {
            path: tree.path(node),
            name: tree.name(node),
            is_dir: tree.is_dir(node),
            logical: tree.logical(node),
            allocated: tree.allocated(node),
            files: tree.files_under(node),
            mtime_ms: tree.mtime_ms(node),
            verdict: guard.check_tree(tree, node),
        }
    }
}

/// Proof that a path passed every check just now. Its field is private, so
/// nothing outside this module can make one.
#[derive(Debug)]
pub struct Approved {
    path: PathBuf,
}

impl Approved {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::test_tree::tree;

    fn guard() -> Guard {
        Guard {
            rules: vec![
                Rule {
                    path: "d:\\windows2".to_owned(),
                    inside: true,
                    why: "system".to_owned(),
                },
                Rule {
                    path: "e:\\stuff\\downloads".to_owned(),
                    inside: false,
                    why: "profile folder".to_owned(),
                },
            ],
            system_drive: Some("c:".to_owned()),
            profile: Some("c:\\users\\me".to_owned()),
            cloud: vec!["c:\\users\\me\\onedrive".to_owned()],
            elevated: false,
            log: None,
        }
    }

    fn blocked(path: &str) -> bool {
        guard().check_path(path).is_blocked()
    }

    fn allowed(path: &str) -> bool {
        guard().check_path(path) == Verdict::Allowed
    }

    fn cautioned(path: &str) -> bool {
        matches!(guard().check_path(path), Verdict::Caution(_))
    }

    #[test]
    fn windows_and_everything_in_it_is_blocked_on_any_drive() {
        for path in [
            r"C:\Windows",
            r"C:\Windows\System32\kernel32.dll",
            r"C:\WINDOWS\explorer.exe",
            r"D:\Windows\notepad.exe",
            r"E:\Windows.old\Users",
            r"C:\Windows\WinSxS\anything\at\all\here.txt",
        ] {
            assert!(blocked(path), "{path}");
        }
    }

    #[test]
    fn program_folders_are_blocked_on_any_drive() {
        for path in [
            r"C:\Program Files",
            r"C:\Program Files\App\data.bin",
            r"C:\Program Files (x86)\Steam\steam.exe",
            r"D:\Program Files\Adobe\file.txt",
            r"D:\WindowsApps\Game\game.pak",
            r"D:\WpSystem\thing",
        ] {
            assert!(blocked(path), "{path}");
        }
    }

    #[test]
    fn boot_and_recovery_locations_are_blocked() {
        for path in [
            r"C:\Boot\BCD",
            r"C:\EFI\Microsoft\Boot\bootmgfw.efi",
            r"C:\Recovery\WindowsRE\winre.wim",
            r"C:\System Volume Information\x",
            r"C:\$Recycle.Bin\S-1-5-21\$R1.txt",
            r"C:\$WinREAgent\Scratch\update.wim",
            r"C:\$Windows.~BT\file",
            r"C:\$SysReset\file",
        ] {
            assert!(blocked(path), "{path}");
        }
    }

    #[test]
    fn whole_drives_are_blocked() {
        assert!(blocked(r"C:\"));
        assert!(blocked("D:"));
    }

    #[test]
    fn critical_files_are_blocked_wherever_they_are() {
        for path in [
            r"C:\pagefile.sys",
            r"D:\hiberfil.sys",
            r"C:\swapfile.sys",
            r"D:\backup\driver.sys",
            r"D:\backup\loader.efi",
            r"D:\backup\bootmgr",
            r"D:\backup\BCD",
            r"C:\Users\me\NTUSER.DAT",
            r"C:\Users\me\ntuser.dat.LOG1",
            r"D:\old\UsrClass.dat",
        ] {
            assert!(blocked(path), "{path}");
        }
    }

    #[test]
    fn the_folders_a_profile_is_built_from_are_blocked() {
        for path in [
            r"C:\Users",
            r"C:\Users\me",
            r"C:\Users\Public\Documents",
            r"C:\Users\Default\NTUSER.DAT",
            r"C:\Users\me\AppData",
            r"C:\Users\me\AppData\Local",
            r"C:\Users\me\AppData\Roaming",
            r"C:\Users\me\AppData\Roaming\Microsoft\Protect\key",
            r"C:\Users\me\AppData\Local\Microsoft\Credentials\x",
            r"C:\Users\me\AppData\Local\Packages",
            r"C:\Users\me\Documents",
            r"C:\Users\me\Desktop",
            r"C:\Users\me\Downloads",
            r"C:\Users\me\.ssh\config",
        ] {
            assert!(blocked(path), "{path}");
        }
    }

    #[test]
    fn programdata_itself_and_microsofts_part_are_blocked() {
        assert!(blocked(r"C:\ProgramData"));
        assert!(blocked(r"C:\ProgramData\Microsoft\Windows Defender\x"));
        assert!(cautioned(r"C:\ProgramData\SomeApp\cache.bin"));
    }

    #[test]
    fn a_folder_holding_a_protected_folder_is_blocked() {
        assert!(blocked(r"E:\stuff"));
        assert!(blocked(r"E:\stuff\downloads"));
        assert!(allowed(r"E:\stuff\downloads\movie.mkv"));
        assert!(blocked(r"D:\windows2\a\b"));
    }

    #[test]
    fn names_windows_would_misread_are_blocked() {
        for path in [
            r"D:\files\report.txt.",
            r"D:\files\report.txt ",
            r"D:\files\*.txt",
            r"D:\files\what?.txt",
            r"D:\files\file.txt:stream",
            r"D:\files\..\..\Windows",
            r"D:\files\.\x",
            r"D:\files\CON",
            r"D:\files\nul.txt",
            r"D:\files\COM1.log",
            "D:\\files\\bad\u{FFFD}name.txt",
            "D:\\files\\new\nline.txt",
        ] {
            assert!(blocked(path), "{path:?}");
        }
    }

    #[test]
    fn only_plain_drive_paths_can_be_moved() {
        for path in [
            r"\\server\share\file.txt",
            r"\\?\C:\file.txt",
            r"\\.\C:\file.txt",
            r"relative\file.txt",
            r"file.txt",
            "",
        ] {
            assert!(blocked(path), "{path:?}");
        }
    }

    #[test]
    fn a_path_too_long_for_the_shell_is_blocked() {
        assert!(blocked(&format!(r"D:\{}", "a".repeat(300))));
    }

    #[test]
    fn ordinary_files_are_allowed() {
        for path in [
            r"D:\modlists\JOJ\mods\texture.dds",
            r"C:\Users\me\Downloads\movie.mkv",
            r"C:\Users\me\Documents\old project",
            r"F:\Torrents\thing.iso",
            r"D:\My Windows Backup\notes.txt",
            r"D:\windows stuff",
        ] {
            assert!(allowed(path), "{path}");
        }
    }

    #[test]
    fn regrettable_things_ask_first() {
        for path in [
            r"D:\Tools\app.exe",
            r"D:\Tools\lib.dll",
            r"C:\Users\me\AppData\Local\SomeApp\cache",
            r"C:\Users\other\Documents\x.txt",
            r"C:\Users\me\OneDrive\photo.jpg",
            r"D:\backup\vault.kdbx",
            r"D:\backup\id_rsa",
        ] {
            assert!(cautioned(path), "{path}");
        }
    }

    #[test]
    fn the_top_of_the_windows_drive_asks_first() {
        assert!(cautioned(r"C:\AMD"));
        assert!(allowed(r"C:\AMD\Packages\readme.txt"));
        assert!(allowed(r"D:\AMD"));
    }

    #[test]
    fn running_as_administrator_always_asks() {
        let mut g = guard();
        g.elevated = true;
        assert!(matches!(g.check_path(r"D:\a.txt"), Verdict::Caution(_)));
    }

    #[test]
    fn the_scanned_root_and_links_are_blocked() {
        let mut out = crate::tree::test_tree::output(&[
            (0, "root", true, 0),
            (0, "link", false, 0),
            (0, "file.txt", false, 5),
        ]);
        out.kind[1] = tm_walk::KIND_SYMLINK;
        let t = Tree::build(
            PathBuf::from(r"D:\data"),
            out,
            "test".to_owned(),
            None,
            crate::tree::Metric::Logical,
        );
        let g = guard();
        assert!(g.check_tree(&t, 0).is_blocked());
        assert!(g.check_tree(&t, 1).is_blocked());
        assert_eq!(g.check_tree(&t, 2), Verdict::Allowed);
    }

    #[test]
    fn a_folder_is_blocked_by_what_it_holds() {
        let t = tree(&[
            (0, "root", true, 0),
            (0, "game", true, 0),
            (1, "deep", true, 0),
            (2, "anticheat.sys", false, 5),
            (0, "clean", true, 0),
            (4, "a.txt", false, 5),
        ]);
        let g = guard();
        let Verdict::Blocked(why) = g.check_tree(&t, 1) else {
            panic!("a folder holding a driver must be blocked");
        };
        assert!(why.contains("anticheat.sys"), "{why}");
        assert_eq!(g.check_tree(&t, 4), Verdict::Allowed);
    }

    #[test]
    fn a_folder_asks_first_about_what_it_holds() {
        let t = tree(&[
            (0, "root", true, 0),
            (0, "app", true, 0),
            (1, "run.exe", false, 5),
            (1, ".git", true, 0),
            (1, "vault.kdbx", false, 5),
        ]);
        let Verdict::Caution(reasons) = guard().check_tree(&t, 1) else {
            panic!("expected a caution");
        };
        assert_eq!(reasons.len(), 3, "{reasons:?}");
    }

    #[test]
    fn a_folder_of_many_files_asks_first() {
        let mut rows = vec![(0_u32, "root", true, 0_u64), (0, "big", true, 0)];
        rows.extend((0..LARGE_FILES).map(|_| (1, "f.txt", false, 1)));
        let t = tree(&rows);
        assert!(matches!(guard().check_tree(&t, 1), Verdict::Caution(_)));
    }

    /// A real folder of its own, by its resolved path.
    fn scratch(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sq-guard-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let real = std::fs::canonicalize(&dir).unwrap();
        PathBuf::from(real.to_string_lossy().trim_start_matches(r"\\?\"))
    }

    fn request(path: &Path, is_dir: bool, logical: u64) -> Request {
        let mtime_ms = std::fs::metadata(path)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64()
            * 1000.0;
        Request {
            path: path.to_owned(),
            name: "x".to_owned(),
            is_dir,
            logical,
            allocated: logical,
            files: 1,
            mtime_ms,
            verdict: Verdict::Allowed,
        }
    }

    #[test]
    fn an_unchanged_file_is_approved() {
        let dir = scratch("same");
        let file = dir.join("a.txt");
        std::fs::write(&file, b"12345").unwrap();
        let approved = guard().approve(&request(&file, false, 5), true).unwrap();
        assert_eq!(approved.path(), file);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_blocked_verdict_stays_blocked_even_when_acknowledged() {
        let dir = scratch("verdict");
        let file = dir.join("a.txt");
        std::fs::write(&file, b"x").unwrap();
        let mut asked = request(&file, false, 1);
        asked.verdict = Verdict::Blocked("no".to_owned());
        assert_eq!(guard().approve(&asked, true).unwrap_err(), "no");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_caution_needs_acknowledging() {
        let dir = scratch("ack");
        let file = dir.join("a.txt");
        std::fs::write(&file, b"x").unwrap();
        let mut asked = request(&file, false, 1);
        asked.verdict = Verdict::Caution(vec!["careful".to_owned()]);
        assert!(guard().approve(&asked, false).is_err());
        assert!(guard().approve(&asked, true).is_ok());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_file_that_changed_since_the_scan_is_refused() {
        let dir = scratch("changed");
        let file = dir.join("a.txt");
        std::fs::write(&file, b"12345").unwrap();
        let asked = request(&file, false, 5);
        std::fs::write(&file, b"now longer").unwrap();
        let why = guard().approve(&asked, true).unwrap_err();
        assert!(why.contains("changed"), "{why}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_file_that_is_gone_is_refused() {
        let dir = scratch("gone");
        let file = dir.join("a.txt");
        std::fs::write(&file, b"x").unwrap();
        let asked = request(&file, false, 1);
        std::fs::remove_file(&file).unwrap();
        assert!(guard().approve(&asked, true).unwrap_err().contains("no longer"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_folder_replaced_by_a_file_is_refused() {
        let dir = scratch("kind");
        let target = dir.join("thing");
        std::fs::create_dir(&target).unwrap();
        let asked = request(&target, true, 0);
        std::fs::remove_dir(&target).unwrap();
        std::fs::write(&target, b"x").unwrap();
        assert!(guard().approve(&asked, true).unwrap_err().contains("changed"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn junction(link: &Path, target: &Path) {
        let done = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .unwrap();
        assert!(done.status.success(), "{done:?}");
    }

    #[test]
    fn a_path_through_a_junction_is_refused() {
        let dir = scratch("through");
        let real = dir.join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::write(real.join("a.txt"), b"x").unwrap();
        let link = dir.join("link");
        junction(&link, &real);
        let asked = request(&link.join("a.txt"), false, 1);
        let why = guard().approve(&asked, true).unwrap_err();
        assert!(why.contains("really leads to"), "{why}");
        std::fs::remove_dir(&link).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_junction_itself_is_refused() {
        let dir = scratch("itself");
        let real = dir.join("real");
        std::fs::create_dir(&real).unwrap();
        let link = dir.join("link");
        junction(&link, &real);
        let mut asked = request(&real, true, 0);
        asked.path = link.clone();
        let why = guard().approve(&asked, true).unwrap_err();
        assert!(why.contains("link"), "{why}");
        std::fs::remove_dir(&link).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn this_machines_folders_are_found_and_protected() {
        let g = Guard::system();
        let windows = std::env::var("SystemRoot").unwrap();
        assert!(g.check_path(&format!(r"{windows}\System32\kernel32.dll")).is_blocked());
        assert!(g.log_path().is_some());
        let exe = std::env::current_exe().unwrap();
        assert!(g.check_path(&exe.to_string_lossy()).is_blocked());
    }

    #[test]
    fn nothing_moves_unless_the_record_can_be_written() {
        let dir = scratch("log");
        let file = dir.join("a.txt");
        std::fs::write(&file, b"12345").unwrap();
        let outcome = guard().move_to_recycle_bin(&request(&file, false, 5), true);
        assert!(matches!(outcome, Recycled::Refused(_)), "{outcome:?}");
        assert!(file.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    #[ignore = "leaves a file in the real Recycle Bin; run with --ignored"]
    fn a_confirmed_file_reaches_the_recycle_bin_and_the_record() {
        let dir = scratch("real");
        let file = dir.join("a.txt");
        std::fs::write(&file, b"to the bin").unwrap();
        let mut g = guard();
        let log = dir.join("actions.log");
        g.log = Some(log.clone());
        let outcome = g.move_to_recycle_bin(&request(&file, false, 10), true);
        assert_eq!(outcome, Recycled::Gone);
        assert!(!file.exists());
        let text = std::fs::read_to_string(log).unwrap();
        assert!(text.contains("moving") && text.contains("recycled"), "{text}");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
