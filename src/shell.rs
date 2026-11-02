//! The Windows shell: drives, opening and revealing, the Recycle Bin, and the
//! elevation of the helper. Everything `unsafe` in the app lives here.

use std::ffi::{OsStr, c_void};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_CANCELLED, GetLastError, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
};
use windows_sys::Win32::Storage::FileSystem::{
    BusTypeNvme, BusTypeSata, CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, GetDiskFreeSpaceExW,
    GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW, GetVolumeNameForVolumeMountPointW,
    GetVolumePathNameW, IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, OPEN_EXISTING, STORAGE_BUS_TYPE,
};
use windows_sys::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows_sys::Win32::System::SystemInformation::GetSystemWindowsDirectoryW;
use windows_sys::Win32::System::Ioctl::{
    DEVICE_SEEK_PENALTY_DESCRIPTOR, DEVICE_TRIM_DESCRIPTOR, IOCTL_STORAGE_QUERY_PROPERTY,
    PropertyStandardQuery, STORAGE_DEVICE_DESCRIPTOR, STORAGE_PROPERTY_ID, STORAGE_PROPERTY_QUERY,
    StorageDeviceProperty, StorageDeviceSeekPenaltyProperty, StorageDeviceTrimProperty,
    VOLUME_DISK_EXTENTS,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcessToken, TerminateProcess, WaitForSingleObject,
};
use windows_sys::Win32::UI::Shell::{
    FO_DELETE, FOF_ALLOWUNDO, FOF_NO_CONNECTED_ELEMENTS, FOF_NOCONFIRMATION, FOF_NOERRORUI,
    FOF_NORECURSEREPARSE, FOF_SILENT, FOF_WANTNUKEWARNING, FOLDERID_Desktop, FOLDERID_Documents,
    FOLDERID_Downloads, FOLDERID_LocalAppData, FOLDERID_Music, FOLDERID_Pictures,
    FOLDERID_Profile, FOLDERID_ProgramFiles, FOLDERID_ProgramFilesX86, FOLDERID_RoamingAppData,
    FOLDERID_SkyDrive, FOLDERID_Videos, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS,
    SHELLEXECUTEINFOW, SHFILEOPSTRUCTW, SHFileOperationW, SHGetKnownFolderPath, SHQUERYRBINFO,
    SHQueryRecycleBinW, ShellExecuteExW, ShellExecuteW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{SW_HIDE, SW_SHOWNORMAL};

/// `DRIVE_FIXED`, `DRIVE_REMOVABLE`, `DRIVE_REMOTE`.
const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_FIXED: u32 = 3;
const DRIVE_REMOTE: u32 = 4;

fn wide(text: &OsStr) -> Vec<u16> {
    text.encode_wide().chain(std::iter::once(0)).collect()
}

fn wide_str(text: &str) -> Vec<u16> {
    wide(OsStr::new(text))
}

/// A mounted drive, as the start screen shows it.
#[derive(Clone, Debug)]
pub struct Drive {
    pub root: PathBuf,
    pub letter: char,
    pub label: String,
    pub file_system: String,
    pub total: u64,
    pub free: u64,
    pub removable: bool,
    pub network: bool,
    /// True on a spinning disk, false on an SSD, `None` when it will not say.
    pub seek_penalty: Option<bool>,
}

impl Drive {
    /// Whether the master file table of this drive can be read at all.
    pub fn table_readable(&self) -> bool {
        self.file_system.eq_ignore_ascii_case("NTFS") && !self.network
    }

    /// The kind of disk, as the start screen names it.
    pub fn media(&self) -> Option<&'static str> {
        self.seek_penalty.map(|p| if p { "HDD" } else { "SSD" })
    }
}

/// Whether the disk under `root` pays to seek: true on a spinning disk, false
/// on an SSD, `None` for anything that will not say (a network or virtual
/// drive, a folder that is not on a drive letter). Needs no administrator: the
/// volume is opened with no access at all, only to ask.
pub fn seek_penalty(root: &Path) -> Option<bool> {
    let letter = drive_letter(&root.to_string_lossy())?;
    let volume = Device::open(&format!("\\\\.\\{letter}:"))?;
    let answer: DEVICE_SEEK_PENALTY_DESCRIPTOR = volume.property(StorageDeviceSeekPenaltyProperty)?;
    Some(answer.IncursSeekPenalty)
}

/// The drive letter a path starts with.
fn drive_letter(path: &str) -> Option<char> {
    let mut chars = path.chars();
    let (letter, ':') = (chars.next()?, chars.next()?) else {
        return None;
    };
    letter.is_ascii_alphabetic().then_some(letter)
}

/// A volume or disk opened with no access at all: it needs no administrator
/// and can only answer questions.
struct Device(HANDLE);

impl Device {
    fn open(path: &str) -> Option<Self> {
        let path = wide_str(path);
        // SAFETY: `path` is NUL-terminated and outlives the call; the null
        // security attributes and template are allowed.
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        (handle != INVALID_HANDLE_VALUE).then_some(Self(handle))
    }

    /// The answer to `code`, if the device gives a whole `T`.
    fn ask<T: Default>(&self, code: u32, input: *const c_void, input_size: u32) -> Option<T> {
        let mut answer = T::default();
        let mut returned = 0_u32;
        // SAFETY: the caller's input is valid for `input_size`, `answer` is
        // writable for its whole size, and the handle is open.
        let ok = unsafe {
            DeviceIoControl(
                self.0,
                code,
                input,
                input_size,
                std::ptr::from_mut(&mut answer).cast::<c_void>(),
                size_of::<T>() as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        (ok != 0 && returned as usize >= size_of::<T>()).then_some(answer)
    }

    /// The storage `property`, if the device gives a whole `T`.
    fn property<T: Default>(&self, property: STORAGE_PROPERTY_ID) -> Option<T> {
        let query = STORAGE_PROPERTY_QUERY {
            PropertyId: property,
            QueryType: PropertyStandardQuery,
            AdditionalParameters: [0],
        };
        self.ask(
            IOCTL_STORAGE_QUERY_PROPERTY,
            std::ptr::from_ref(&query).cast::<c_void>(),
            size_of::<STORAGE_PROPERTY_QUERY>() as u32,
        )
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        // SAFETY: the handle is ours and closed once.
        unsafe { CloseHandle(self.0) };
    }
}

/// What a disk says about itself, for deciding how hard a walk may press it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DiskReport {
    /// The real path sits on its drive letter's own volume, not on a volume
    /// mounted in a folder.
    pub on_letter_volume: bool,
    /// Windows calls the drive fixed, not removable or remote.
    pub fixed: bool,
    /// The physical disk number, when the volume lives on exactly one disk.
    pub disk: Option<u32>,
    pub seek_penalty: Option<bool>,
    pub bus: Option<STORAGE_BUS_TYPE>,
    pub removable_media: Option<bool>,
    pub trim: Option<bool>,
}

impl DiskReport {
    /// True only for one internal SATA or NVMe disk that has no seek penalty
    /// and supports TRIM. A signal missing is a no. USB, RAID, Storage Spaces
    /// and virtual disks are all out, because any of them can hide a hard
    /// drive behind a good answer.
    pub fn is_ssd(&self) -> bool {
        self.on_letter_volume
            && self.fixed
            && self.disk.is_some()
            && self.seek_penalty == Some(false)
            && self.bus.is_some_and(|bus| bus == BusTypeNvme || bus == BusTypeSata)
            && self.removable_media == Some(false)
            && self.trim == Some(true)
    }
}

/// Asks the physical disk behind `root` about itself. The path is resolved
/// first, so a link, or a drive made with `subst`, is judged by the disk it
/// really leads to. What can't be read stays unknown.
pub fn disk_report(root: &Path) -> DiskReport {
    let mut report = DiskReport::default();
    let Ok(real) = std::fs::canonicalize(root) else {
        return report;
    };
    let real = real.to_string_lossy();
    let real = real.strip_prefix(r"\\?\").unwrap_or(&real);
    let Some(letter) = drive_letter(real) else {
        return report;
    };
    let letter_root = format!("{letter}:\\");
    report.on_letter_volume =
        volume_path(real).is_some_and(|path| path.eq_ignore_ascii_case(&letter_root));
    let path = wide_str(&letter_root);
    // SAFETY: `path` is NUL-terminated and outlives the call.
    report.fixed = unsafe { GetDriveTypeW(path.as_ptr()) } == DRIVE_FIXED;
    report.disk = Device::open(&format!("\\\\.\\{letter}:"))
        .and_then(|volume| {
            volume.ask::<VOLUME_DISK_EXTENTS>(
                IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
                std::ptr::null(),
                0,
            )
        })
        .filter(|extents| extents.NumberOfDiskExtents == 1)
        .map(|extents| extents.Extents[0].DiskNumber);
    let Some(disk) = report
        .disk
        .and_then(|number| Device::open(&format!("\\\\.\\PhysicalDrive{number}")))
    else {
        return report;
    };
    let penalty: Option<DEVICE_SEEK_PENALTY_DESCRIPTOR> =
        disk.property(StorageDeviceSeekPenaltyProperty);
    report.seek_penalty = penalty.map(|answer| answer.IncursSeekPenalty);
    let device: Option<STORAGE_DEVICE_DESCRIPTOR> = disk.property(StorageDeviceProperty);
    report.bus = device.map(|answer| answer.BusType);
    report.removable_media = device.map(|answer| answer.RemovableMedia);
    let trim: Option<DEVICE_TRIM_DESCRIPTOR> = disk.property(StorageDeviceTrimProperty);
    report.trim = trim.map(|answer| answer.TrimEnabled);
    report
}

/// The mount point of the volume holding `path`, such as `C:\` or
/// `C:\Mounted\`.
fn volume_path(path: &str) -> Option<String> {
    let path = wide_str(path);
    let mut volume = [0_u16; 1024];
    // SAFETY: `path` is NUL-terminated and `volume` is writable for the length given.
    let ok = unsafe { GetVolumePathNameW(path.as_ptr(), volume.as_mut_ptr(), volume.len() as u32) };
    let end = volume.iter().position(|unit| *unit == 0)?;
    (ok != 0).then(|| String::from_utf16_lossy(&volume[..end]))
}

/// Every fixed, removable and network drive that answers.
pub fn drives() -> Vec<Drive> {
    // SAFETY: takes nothing, returns a bit mask.
    let mask = unsafe { GetLogicalDrives() };
    (0..26)
        .filter(|i| mask >> i & 1 == 1)
        .filter_map(|i| drive(char::from(b'A' + i as u8)))
        .collect()
}

fn drive(letter: char) -> Option<Drive> {
    let root = format!("{letter}:\\");
    let path = wide_str(&root);
    // SAFETY: `path` is NUL-terminated and outlives the call.
    let kind = unsafe { GetDriveTypeW(path.as_ptr()) };
    if !matches!(kind, DRIVE_FIXED | DRIVE_REMOVABLE | DRIVE_REMOTE) {
        return None;
    }
    let (mut free, mut total) = (0_u64, 0_u64);
    // SAFETY: `path` is NUL-terminated; the out-pointers are valid u64s; a null
    // third out-pointer is allowed.
    let ok = unsafe { GetDiskFreeSpaceExW(path.as_ptr(), &mut free, &mut total, std::ptr::null_mut()) };
    if ok == 0 {
        return None;
    }
    let mut label = [0_u16; 261];
    let mut file_system = [0_u16; 64];
    // SAFETY: both buffers are writable for the lengths given; the other
    // out-pointers may be null.
    let named = unsafe {
        GetVolumeInformationW(
            path.as_ptr(),
            label.as_mut_ptr(),
            label.len() as u32,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            file_system.as_mut_ptr(),
            file_system.len() as u32,
        )
    };
    let text = |units: &[u16]| {
        let end = units.iter().position(|u| *u == 0).unwrap_or(units.len());
        String::from_utf16_lossy(&units[..end])
    };
    Some(Drive {
        root: PathBuf::from(&root),
        letter,
        label: if named != 0 { text(&label) } else { String::new() },
        file_system: if named != 0 { text(&file_system) } else { String::new() },
        total,
        free,
        removable: kind == DRIVE_REMOVABLE,
        network: kind == DRIVE_REMOTE,
        seek_penalty: seek_penalty(Path::new(&root)),
    })
}

/// Whether this process runs elevated.
pub fn is_elevated() -> bool {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: the pseudo-handle is always valid; `token` is a valid out-pointer.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return false;
    }
    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut returned = 0_u32;
    // SAFETY: `elevation` is writable for the size passed; `token` is open.
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            std::ptr::from_mut(&mut elevation).cast::<c_void>(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        )
    };
    // SAFETY: `token` is ours and closed once.
    unsafe { CloseHandle(token) };
    ok != 0 && elevation.TokenIsElevated != 0
}

/// Why the helper did not start.
#[derive(Debug)]
pub enum ElevateError {
    /// The person said no at the UAC prompt.
    Declined,
    Failed(String),
}

/// The elevated helper, running.
pub struct Helper {
    process: HANDLE,
}

// SAFETY: a process handle may be waited on and closed from any thread.
unsafe impl Send for Helper {}

impl Helper {
    /// Waits up to `limit`; true once the helper has exited.
    pub fn wait(&self, limit: Duration) -> bool {
        // SAFETY: `process` stays open until drop.
        unsafe { WaitForSingleObject(self.process, limit.as_millis() as u32) == WAIT_OBJECT_0 }
    }

    pub fn terminate(&self) {
        // SAFETY: `process` stays open until drop; failure leaves it running,
        // and it exits by itself when it has read the table.
        unsafe { TerminateProcess(self.process, 1) };
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        // SAFETY: `process` is ours and closed once.
        unsafe { CloseHandle(self.process) };
    }
}

/// The argument that puts this executable in helper mode.
pub const HELPER_FLAG: &str = "--mft-helper";

/// Quotes one argument the way the C runtime parses it back.
fn quote_arg(arg: &str) -> String {
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(backslashes));
                backslashes = 0;
                out.push(c);
            }
        }
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}

/// Starts this executable elevated, hidden, in helper mode. The UAC prompt
/// names this executable; the helper reads the volume and writes `output`.
pub fn start_elevated_helper(
    volume: &str,
    root: &Path,
    output: &Path,
) -> Result<Helper, ElevateError> {
    let exe = std::env::current_exe().map_err(|e| ElevateError::Failed(e.to_string()))?;
    let args = format!(
        "{HELPER_FLAG} {} {} {}",
        quote_arg(volume),
        quote_arg(&root.to_string_lossy()),
        quote_arg(&output.to_string_lossy()),
    );
    let verb = wide_str("runas");
    let file = wide(exe.as_os_str());
    let parameters = wide_str(&args);
    // SAFETY: zero is valid for this plain C struct; every field read is set below.
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = parameters.as_ptr();
    info.nShow = SW_HIDE;
    // SAFETY: every string in `info` is NUL-terminated and outlives the call.
    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        // SAFETY: reads this thread's last error.
        let code = unsafe { GetLastError() };
        return Err(if code == ERROR_CANCELLED {
            ElevateError::Declined
        } else {
            ElevateError::Failed(format!("Windows error {code}"))
        });
    }
    if info.hProcess.is_null() {
        return Err(ElevateError::Failed("no process handle came back".to_owned()));
    }
    Ok(Helper {
        process: info.hProcess,
    })
}

/// Types that run code when opened, including the shortcuts that can point at it.
const RUNNABLE_TYPES: [&str; 22] = [
    "exe", "com", "scr", "pif", "bat", "cmd", "ps1", "psm1", "vbs", "vbe", "js", "jse", "wsf",
    "wsh", "hta", "msi", "msp", "msix", "cpl", "lnk", "reg", "jar",
];

/// Whether opening `path` would run something rather than show it.
pub fn is_runnable(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| RUNNABLE_TYPES.contains(&e.to_string_lossy().to_lowercase().as_str()))
}

/// Opens a file or folder as a double-click in Explorer would. Callers keep
/// [`is_runnable`] paths away from here: a disk viewer never launches programs.
pub fn open(path: &Path) {
    let file = wide(path.as_os_str());
    let verb = wide_str("open");
    // SAFETY: both strings are NUL-terminated and outlive the call.
    unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        );
    }
}

/// Shows a file or folder selected in its Explorer window.
pub fn reveal(path: &Path) {
    use std::os::windows::process::CommandExt;
    let _ = std::process::Command::new("explorer.exe")
        .raw_arg(format!("/select,\"{}\"", path.display()))
        .spawn();
}

/// Folders of this machine, as Windows reports them rather than as the
/// environment says: any program can change an environment variable.
#[derive(Default)]
pub struct SystemFolders {
    pub system: Vec<String>,
    pub personal: Vec<String>,
    pub profile: Option<String>,
    pub cloud: Option<String>,
    pub local_data: Option<String>,
}

fn known_folder(id: &windows_sys::core::GUID) -> Option<String> {
    let mut path: windows_sys::core::PWSTR = std::ptr::null_mut();
    // SAFETY: `id` is a valid GUID, `path` a valid out-pointer, and a null
    // token means this user.
    let result = unsafe { SHGetKnownFolderPath(id, 0, std::ptr::null_mut(), &mut path) };
    let text = (result >= 0 && !path.is_null()).then(|| {
        // SAFETY: on success `path` is a NUL-terminated string from the shell.
        let len = (0..).take_while(|i| unsafe { *path.add(*i) } != 0).count();
        // SAFETY: `len` units were just read from `path`.
        String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(path, len) })
    });
    // SAFETY: the shell allocated `path`, and null is accepted.
    unsafe { CoTaskMemFree(path.cast()) };
    text
}

/// Where Windows and this person's folders really are.
pub fn system_folders() -> SystemFolders {
    let mut units = [0_u16; 512];
    // SAFETY: `units` is writable for the length passed.
    let len = unsafe { GetSystemWindowsDirectoryW(units.as_mut_ptr(), units.len() as u32) };
    let windows = units.get(..len as usize).map(String::from_utf16_lossy);
    let mut system: Vec<String> = windows.into_iter().collect();
    system.extend(known_folder(&FOLDERID_ProgramFiles));
    system.extend(known_folder(&FOLDERID_ProgramFilesX86));
    let personal = [
        FOLDERID_Desktop,
        FOLDERID_Documents,
        FOLDERID_Downloads,
        FOLDERID_Music,
        FOLDERID_Pictures,
        FOLDERID_Videos,
        FOLDERID_LocalAppData,
        FOLDERID_RoamingAppData,
        FOLDERID_SkyDrive,
    ]
    .iter()
    .filter_map(known_folder)
    .collect();
    SystemFolders {
        system,
        personal,
        profile: known_folder(&FOLDERID_Profile),
        cloud: known_folder(&FOLDERID_SkyDrive),
        local_data: known_folder(&FOLDERID_LocalAppData),
    }
}

/// Whether file metadata (not followed) is a link, a junction or any other
/// reparse point.
pub fn is_reparse_point(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    /// `FILE_ATTRIBUTE_REPARSE_POINT`.
    const REPARSE_POINT: u32 = 0x400;
    meta.file_attributes() & REPARSE_POINT != 0 || meta.file_type().is_symlink()
}

/// One DWORD of this volume's Recycle Bin settings, if it has any set.
fn recycle_setting(guid: &str, value: &str) -> Option<u32> {
    let key = wide_str(&format!(
        r"Software\Microsoft\Windows\CurrentVersion\Explorer\BitBucket\Volume\{guid}"
    ));
    let value = wide_str(value);
    let mut data = 0_u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: both strings are NUL-terminated, and `data` is writable for `size`.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            std::ptr::from_mut(&mut data).cast::<c_void>(),
            &mut size,
        )
    };
    (status == 0).then_some(data)
}

/// Why Windows would not put `bytes` of `path` in a Recycle Bin. Asked first,
/// because the shell's answer to any of these is to delete permanently.
pub fn recycle_refusal(path: &Path, bytes: u64) -> Option<String> {
    let text = path.to_string_lossy();
    let root = wide_str(&format!("{}\\", text.get(..2)?));
    // SAFETY: `root` is NUL-terminated and outlives the call.
    if unsafe { GetDriveTypeW(root.as_ptr()) } != DRIVE_FIXED {
        return Some("this drive has no Recycle Bin, so moving it would delete it for good".to_owned());
    }
    let mut info = SHQUERYRBINFO {
        cbSize: size_of::<SHQUERYRBINFO>() as u32,
        i64Size: 0,
        i64NumItems: 0,
    };
    // SAFETY: `root` is NUL-terminated, and `info` carries its own size.
    if unsafe { SHQueryRecycleBinW(root.as_ptr(), &mut info) } < 0 {
        return Some("Windows reports no Recycle Bin on this drive".to_owned());
    }
    let mut volume = [0_u16; 64];
    // SAFETY: `root` is NUL-terminated; `volume` is writable for the length passed.
    let named = unsafe {
        GetVolumeNameForVolumeMountPointW(root.as_ptr(), volume.as_mut_ptr(), volume.len() as u32)
    };
    if named == 0 {
        return None;
    }
    let name = String::from_utf16_lossy(&volume);
    let guid = name.split_once('{').and_then(|(_, g)| g.split_once('}'))?.0;
    let guid = format!("{{{guid}}}");
    if recycle_setting(&guid, "NukeOnDelete").is_some_and(|nuke| nuke != 0) {
        return Some("the Recycle Bin is turned off for this drive, so moving it would delete it for good".to_owned());
    }
    let capacity = recycle_setting(&guid, "MaxCapacity")?;
    (bytes > u64::from(capacity) * 1024 * 1024).then(|| {
        format!(
            "it is larger than this drive's Recycle Bin ({} MB), so it would be deleted for good",
            crate::format::count(u64::from(capacity))
        )
    })
}

/// How a move to the Recycle Bin ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Recycled {
    /// The path has left the disk.
    Gone,
    /// The person stopped it at a shell prompt.
    Aborted,
    /// Nothing was attempted: a check refused it.
    Refused(String),
    Failed(String),
}

impl Recycled {
    /// The word the action log records.
    pub fn word(&self) -> &'static str {
        match self {
            Self::Gone => "recycled",
            Self::Aborted => "stopped",
            Self::Refused(_) => "refused",
            Self::Failed(_) => "failed",
        }
    }
}

/// Moves one approved path to the Recycle Bin. Never a permanent delete: the
/// guard has already refused what the bin cannot take, and should Windows
/// change its mind anyway its own warning asks first. Only this path moves:
/// no connected files beside it, and nothing through a link inside it.
pub(crate) fn recycle(approved: &crate::guard::Approved) -> Recycled {
    let path = approved.path();
    // SAFETY: initialises COM on this thread; an already-initialised thread is fine.
    unsafe { CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32) };
    // The list form: double-NUL terminated.
    let mut from: Vec<u16> = path.as_os_str().encode_wide().collect();
    from.extend([0, 0]);
    let flags = FOF_ALLOWUNDO
        | FOF_WANTNUKEWARNING
        | FOF_NOCONFIRMATION
        | FOF_SILENT
        | FOF_NOERRORUI
        | FOF_NO_CONNECTED_ELEMENTS
        | FOF_NORECURSEREPARSE;
    let mut op = SHFILEOPSTRUCTW {
        hwnd: std::ptr::null_mut(),
        wFunc: FO_DELETE,
        pFrom: from.as_ptr(),
        pTo: std::ptr::null(),
        fFlags: flags as u16,
        fAnyOperationsAborted: 0,
        hNameMappings: std::ptr::null_mut(),
        lpszProgressTitle: std::ptr::null(),
    };
    // SAFETY: `op` is fully set and `from` outlives the call.
    let code = unsafe { SHFileOperationW(&mut op) };
    if op.fAnyOperationsAborted != 0 {
        return Recycled::Aborted;
    }
    if code != 0 {
        return Recycled::Failed(format!("the shell reported error {code:#x}"));
    }
    if std::fs::symlink_metadata(path).is_ok() {
        return Recycled::Failed("it is still on disk".to_owned());
    }
    Recycled::Gone
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::Storage::FileSystem::{
        BusTypeFileBackedVirtual, BusTypeRAID, BusTypeSas, BusTypeScsi, BusTypeSpaces, BusTypeUsb,
        BusTypeVirtual,
    };

    fn good_ssd() -> DiskReport {
        DiskReport {
            on_letter_volume: true,
            fixed: true,
            disk: Some(0),
            seek_penalty: Some(false),
            bus: Some(BusTypeNvme),
            removable_media: Some(false),
            trim: Some(true),
        }
    }

    #[test]
    fn a_disk_passes_only_with_every_signal() {
        assert!(good_ssd().is_ssd());
        assert!(DiskReport { bus: Some(BusTypeSata), ..good_ssd() }.is_ssd());
        for failing in [
            DiskReport { on_letter_volume: false, ..good_ssd() },
            DiskReport { fixed: false, ..good_ssd() },
            DiskReport { disk: None, ..good_ssd() },
            DiskReport { seek_penalty: Some(true), ..good_ssd() },
            DiskReport { seek_penalty: None, ..good_ssd() },
            DiskReport { bus: None, ..good_ssd() },
            DiskReport { removable_media: Some(true), ..good_ssd() },
            DiskReport { removable_media: None, ..good_ssd() },
            DiskReport { trim: Some(false), ..good_ssd() },
            DiskReport { trim: None, ..good_ssd() },
            DiskReport::default(),
        ] {
            assert!(!failing.is_ssd(), "{failing:?}");
        }
    }

    #[test]
    fn a_bus_that_can_hide_a_hard_drive_never_passes() {
        for bus in [
            BusTypeUsb,
            BusTypeRAID,
            BusTypeSpaces,
            BusTypeFileBackedVirtual,
            BusTypeVirtual,
            BusTypeSas,
            BusTypeScsi,
        ] {
            assert!(!DiskReport { bus: Some(bus), ..good_ssd() }.is_ssd(), "bus {bus}");
        }
    }

    #[test]
    fn a_path_that_does_not_exist_reports_nothing() {
        assert_eq!(disk_report(Path::new(r"C:\no\such\folder\here")), DiskReport::default());
    }

    #[test]
    fn arguments_survive_quoting() {
        assert_eq!(quote_arg("C:"), "\"C:\"");
        assert_eq!(quote_arg("C:\\"), "\"C:\\\\\"");
        assert_eq!(quote_arg("C:\\a b\\c"), "\"C:\\a b\\c\"");
        assert_eq!(quote_arg("say \"hi\""), "\"say \\\"hi\\\"\"");
    }

    #[test]
    fn programs_and_scripts_count_as_runnable() {
        for name in ["a.exe", "b.BAT", "c.ps1", "d.lnk", "e.msi", "f.reg", "g.vbs"] {
            assert!(is_runnable(Path::new(name)), "{name}");
        }
        for name in ["a.txt", "b.mp4", "c.pdf", "folder", "d.dll"] {
            assert!(!is_runnable(Path::new(name)), "{name}");
        }
    }

    #[test]
    fn a_small_item_fits_this_machines_recycle_bin() {
        assert_eq!(recycle_refusal(&std::env::temp_dir(), 1), None);
    }

    #[test]
    fn an_item_larger_than_the_recycle_bin_is_refused() {
        let why = recycle_refusal(&std::env::temp_dir(), u64::MAX);
        assert!(why.is_some_and(|w| w.contains("larger")));
    }

    #[test]
    fn a_path_without_a_drive_letter_has_no_seek_penalty_answer() {
        assert_eq!(seek_penalty(Path::new("\\\\server\\share")), None);
        assert_eq!(seek_penalty(Path::new("")), None);
        assert_eq!(seek_penalty(Path::new("1:\\x")), None);
    }

    #[test]
    fn this_machine_has_a_drive() {
        assert!(!drives().is_empty());
    }
}
