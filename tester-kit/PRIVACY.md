# Sizequilt: privacy and how to check it

You shouldn't have to take our word for any of this, so this page also shows how
to check it yourself. The strongest check takes about a minute and needs no
technical knowledge.

## What the app does

**It sees:** file and folder names, sizes, dates, and where they are. Names can
be sensitive, so that is still worth protecting.

**It never opens your files to read them.** Finding sizes only needs the disk's
index. The only files it reads are its own temporary scan file and two fonts from
the Windows fonts folder.

One detail matters if you want to be exact. A **File table** scan loads the
drive's whole index into memory, and Windows stores very small files, up to
roughly a few hundred bytes, inside that index. The app doesn't use those bytes:
it takes only names, sizes and dates, and they're gone when the scan finishes. A
**Folder walker** scan never loads them at all.

**It keeps nothing about your scan.** Results stay in memory and are gone when
you close the app.

It has no account, no sign-in, no analytics, no crash reporting and no automatic
updates. Nothing is designed to leave your computer.

## What it writes to your disk

- **A log of files you moved to the Recycle Bin**, at
  `%LOCALAPPDATA%\Sizequilt\actions.log`. It's a plain text file with the
  full path of each item. It stays on your computer and you can delete it.
- **A temporary file during a file table scan**, in `%TEMP%\TreeMap-mft`. It
  holds names and sizes and is removed when the scan finishes.

## When it asks for administrator rights

Only for a **File table** scan, which reads the drive's index directly. Automatic
mode uses this on hard drives and USB drives.

Choose **Folder walker** on the start screen and the app never asks. It runs
with your normal rights, so it can only list folders you can already open.

## Check it yourself

### 1. Scan with your network off (strongest, and the easiest)

This works even if everything else on this page were untrue, because nothing can
leave a computer that isn't connected.

1. Turn on airplane mode, or unplug your network cable.
2. Open Sizequilt, scan, and look around.
3. Close it, then check Task Manager to make sure it's gone.
4. Turn your network back on.

For a complete check, use a computer you can leave offline, and make sure the
app is closed before you reconnect.

### 2. Check that your copy is the real one

Ask for the SHA-256 hash, and get it **separately from the file**, for example by
text message if the app came by email. Someone who could swap the file could
swap a hash sent with it.

Open PowerShell in this folder and run:

```bash
powershell -ExecutionPolicy Bypass -File .\check-app.ps1 -ExpectedHash PASTE-THE-HASH-HERE
```

This reads the program without running it. It also looks for the Windows
networking libraries a program would normally use.

### 3. Block it with Windows Firewall

Windows enforces this rule, so it doesn't rely on the app behaving. Read
`block-network.ps1` first, then run it from a PowerShell window opened as
administrator:

```bash
powershell -ExecutionPolicy Bypass -File .\block-network.ps1
```

Use **Folder walker** with this, so the app never has the rights to change the
rule. The rule covers only this program, not other programs it might start,
which is why step 1 is the stronger check.

### 4. Watch it run

Open **Resource Monitor** from the Start menu, go to the **Network** tab, and tick
`Sizequilt.exe` while it scans. It should show no network activity.

## The limits of these checks

A program can't prove its own innocence, and no check of the file itself is
complete, because a program can hide what it does. That's why step 1 matters: it
doesn't depend on trusting the app or these scripts.

Windows components loaded into the app, such as your graphics driver and the
folder picker, are not part of our code. Network-off scanning covers them too.
