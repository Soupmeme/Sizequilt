# Sizequilt

A fast native disk-space treemap for Windows: one 6 MB `.exe`, with no installer,
Node or Electron.

![Sizequilt showing a demo folder as a treemap](docs/screenshot.png)

It is a native rebuild of the core of
[TreeMap](https://github.com/Prithvi-Web/TreeMap-Disk-Visualizer). TreeMap's scan
engine was already Rust, so this project reuses those crates and replaces what
surrounds them: Express, Electron and the browser UI.

## Getting it

Download the latest build from
[Releases](https://github.com/Soupmeme/Sizequilt/releases), or
[build it from source](#building), which takes a few minutes and means you can
read everything that goes into it. Each release lists the SHA-256 hash of its
`.exe`.

Builds aren't code-signed. Because a new unsigned program has no reputation,
Windows SmartScreen may warn about it, and Microsoft Defender has flagged test
builds with generic machine-learning detections (names ending in `!ml`). We
believe those are false positives, but Microsoft hasn't confirmed that. Don't
turn your antivirus off for it. [PRIVACY.md](tester-kit/PRIVACY.md) lists checks
you can run yourself, including scanning with your network off.

## Using it

Run `dist\Sizequilt.exe`, then click a drive or choose a folder. You can
also start a scan directly:

```bash
Sizequilt.exe C:\
Sizequilt.exe D:\Games --walker
Sizequilt.exe H:\ --mft
```

With no flag, the method is chosen from the disk, as described under
[Scan speed](#scan-speed).

| Action | How |
|---|---|
| Go straight to a folder | Double-click it, or anything inside it |
| Zoom in one level | Scroll up |
| Zoom out | Scroll down, `Backspace`, the up button, or the breadcrumb |
| Back | The back button, or the mouse's back button |
| Select | Click a tile; the folder list follows |
| Actions | Right-click, or use the buttons under the list |
| Search | `Ctrl+F`, then a name or `*.ext` |
| Light up a file type | Click it under **File types** |
| Move to Recycle Bin | Select it and press `Delete`, then confirm |

### Features

- Treemap of a whole drive or a folder, with zoom, labels and tooltips
- Folder tree, largest files and a file-type breakdown
- Search across every name, with matches lit on the map
- Sizes by space on disk or by file size, with hard links counted once
- Open, show in Explorer, copy path, and move to Recycle Bin, under the rules in
  [Safety](#safety)

### Safety

Scanning never changes anything on disk. The administrator step only reads the
drive; that process writes one file, in your temp folder, and exits.

Nothing is ever removed without your confirmation of that one named item. There
is no batch delete, no automatic cleanup and no way to empty the Recycle Bin.
Every move passes through one gatekeeper, `src/guard.rs`, and the code that
calls Windows' delete cannot compile without its approval.

**Blocked, with no override in the app:**

- Whole drives, and the folder you scanned
- `Windows`, `Windows.old`, `Program Files`, `Program Files (x86)`, `Boot`, `EFI`,
  `Recovery`, `WindowsApps`, `System Volume Information` and every `$` folder
  (`$Recycle.Bin`, `$WinREAgent`) at the top of any drive, with everything in them
- `Users`, any whole profile, the Public and Default profiles, `AppData` and its
  `Local` and `Roaming` folders, `AppData\*\Microsoft`, `.ssh` and `.gnupg`
- `ProgramData` itself and `ProgramData\Microsoft`
- Your Desktop, Documents, Downloads, Pictures, Music and Videos folders
  themselves, wherever they live; their contents are yours to manage
- Drivers and boot files anywhere: `.sys`, `.efi`, `.drv`, `.cpl`, `bootmgr`,
  `BCD`, and profile hives such as `NTUSER.DAT`
- Any folder containing one of those, or containing a protected folder
- Links and junctions, and anything reached through one
- Names Windows could read as something else: wildcards, trailing dots or
  spaces, device names such as `CON`, and paths too long for the shell

**Caution, where you must tick an acknowledgement first:** program files and
folders holding them, AppData contents, ProgramData contents, another user's
files, the top of the Windows drive, key and password files, git repositories,
synced OneDrive folders, moves over 10 GB or 10,000 files, and anything while
running as administrator.

**Right before anything moves**, the item is re-checked against the disk. The
move is refused if it has gone, changed since the scan, or now leads somewhere
else. It's also refused if the drive has no Recycle Bin, the bin is turned off,
or the item is larger than the bin, because Windows would delete it for good in
those cases.

Only the selected item moves. Windows' habit of also taking a page's connected
`_files` folder is turned off, and programs and scripts are never started from
the app.

Every attempt is recorded in `%LOCALAPPDATA%\Sizequilt\actions.log`, and
nothing moves if that record can't be written.

These protections cover this app only. They don't stop File Explorer or other
programs.

To check the rules against a real drive, run
`target\release\sq-bench.exe --policy C:\`. It asks the guard about every item
and prints what would get through.

## Scan speed

Measured on this machine on real drives. F: is a 932 GB NTFS drive holding about
400,000 items:

| Scan of F: | Time | Items |
|---|---|---|
| Directory walker, cold (first scan after boot) | 170 s | 392,318 |
| Master file table | 5.5 s | 416,480 |
| Directory walker, warm (folder data in memory) | 1.4 s | 392,318 |

The MFT read costs about the same whenever it runs, while the walker depends
entirely on whether Windows has the folder data cached and how fast the drive
seeks. On D:, an NVMe drive with 880,000 items, the walker took 3 s when cached,
against roughly 40 s for the MFT.

So **Automatic**, the default, asks each disk whether it pays a seek penalty,
which needs no administrator rights:

| Disk | Method used |
|---|---|
| SSD | Folder walker |
| Hard drive | File table, with an administrator prompt |
| Won't say | File table, with an administrator prompt |
| Not NTFS, or a network drive | Folder walker |

USB enclosures often don't pass the question through, so a USB drive usually
counts as "won't say". That suits a USB hard drive; for a USB SSD, choose Folder
walker.

Choose **Folder walker** or **File table** on the start screen to override it.
File table is also the one to pick when you want folders Windows refuses to
list counted.

The MFT also counted more on F: (446 GB against 443 GB), most likely because it
is not stopped by the folders Windows refuses to list.

Once a scan is loaded, a 2560×1440 map takes about 15 ms to lay out and paint,
and searching 575,000 names takes about 5 ms. Memory is about 75–90 bytes per
item, on top of about 100 MB for the app.

## Building

Install [Rust](https://rustup.rs) with the `stable-x86_64-pc-windows-gnu`
toolchain, then:

```bash
.\build.ps1
.\build.ps1 test -p sizequilt --lib
```

The MSVC toolchain is not needed. `src/bin/sq-bench.rs` times one scan with no
UI: `target\release\sq-bench.exe F:\`.

### Sharing with testers

`.\release.ps1` builds `dist\Sizequilt-tester-kit.zip` with the app,
[PRIVACY.md](tester-kit/PRIVACY.md) and the check scripts. It also writes
`dist\SHA256-send-separately.txt`. Send that hash through a different channel
from the zip.

A release stops if the tests fail, if code uses a socket type, if a networking
crate is in the build, if the built file contains network libraries, or if it
holds your username or project path. It also confirms that those checks can
fail, by giving them a known bad input first.

## Layout

| Path | What it holds |
|---|---|
| `src/tree.rs` | The scan as flat columns: sizes, sorting, removal |
| `src/scan.rs` | MFT first, walker as the fallback |
| `src/layout.rs`, `src/raster.rs` | Squarified layout, painted off the UI thread |
| `src/search.rs`, `src/category.rs` | Name search and file types |
| `src/guard.rs` | What may be moved: the only way to the Recycle Bin |
| `src/shell.rs` | Every Windows call: drives, elevation, Recycle Bin |
| `src/app/` | The interface (egui) |
| `vendor/` | Third-party crates, kept separate from the app |

## Licences

Sizequilt is released under the MIT License. See [LICENSE](LICENSE).

`vendor/tm-*` are copied unchanged from TreeMap, MIT, © 2026 Prithvi Vinay
(`vendor/LICENSE-TreeMap.txt`).

`vendor/windows-link` is windows-link 0.2.1 (MIT or Apache-2.0) with one change:
it links through ordinary import libraries instead of `raw-dylib`, because
`raw-dylib` on the GNU target needs an assembler that the Rust toolchain does
not ship.
