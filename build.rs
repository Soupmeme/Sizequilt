//! Gives the Windows executable its manifest, icon and version details.
//!
//! These normally go through a resource compiler, which the GNU Rust toolchain
//! doesn't ship, so this writes the COFF object itself: one `.rsrc` section
//! holding the resource tree Windows reads.

use std::path::Path;

use embed_manifest::manifest::{ActiveCodePage, DpiAwareness, Setting};
use embed_manifest::new_manifest;

mod icon {
    include!("src/icon.rs");
}

const RT_ICON: u32 = 3;
const RT_GROUP_ICON: u32 = 14;
const RT_VERSION: u32 = 16;
const RT_MANIFEST: u32 = 24;
const ENGLISH_US: u32 = 0x0409;
const ICON_SIZES: [usize; 6] = [16, 24, 32, 48, 64, 256];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/icon.rs");
    if std::env::var_os("CARGO_CFG_WINDOWS").is_none() {
        return;
    }
    let manifest = new_manifest("Sizequilt")
        .active_code_page(ActiveCodePage::Utf8)
        .dpi_awareness(DpiAwareness::PerMonitorV2)
        .long_path_aware(Setting::Enabled)
        .to_string();

    let mut resources: Vec<(u32, u32, Vec<u8>)> = Vec::new();
    let mut group = Vec::new();
    group.extend_from_slice(&[0, 0, 1, 0]);
    group.extend_from_slice(&(ICON_SIZES.len() as u16).to_le_bytes());
    for (index, &size) in ICON_SIZES.iter().enumerate() {
        let image = icon_image(size);
        let id = index as u16 + 1;
        let side = if size == 256 { 0 } else { size as u8 };
        group.extend_from_slice(&[side, side, 0, 0, 1, 0, 32, 0]);
        group.extend_from_slice(&(image.len() as u32).to_le_bytes());
        group.extend_from_slice(&id.to_le_bytes());
        resources.push((RT_ICON, u32::from(id), image));
    }
    resources.push((RT_GROUP_ICON, 1, group));
    resources.push((RT_VERSION, 1, version_info()));
    resources.push((RT_MANIFEST, 1, manifest.into_bytes()));

    let out = std::env::var("OUT_DIR").expect("cargo sets OUT_DIR");
    let path = Path::new(&out).join("resources.o");
    std::fs::write(&path, coff_object(&resources)).expect("the resources could not be written");
    println!("cargo:rustc-link-arg-bins={}", path.display());

    // What the patched windows-link leaves to the final link.
    for lib in [
        "kernel32", "ntdll", "user32", "gdi32", "shell32", "ole32", "oleaut32", "advapi32",
        "comdlg32", "opengl32", "synchronization", "ws2_32", "bcrypt", "userenv",
    ] {
        println!("cargo:rustc-link-lib={lib}");
    }
}

/// One icon size as Windows stores it: a bitmap header, bottom-up BGRA pixels,
/// and an AND mask that's unused beside the alpha channel.
fn icon_image(size: usize) -> Vec<u8> {
    let rgba = icon::pixels(size);
    let mask_row = size.div_ceil(32) * 4;
    let mut image = Vec::with_capacity(40 + rgba.len() + mask_row * size);
    let dword = |image: &mut Vec<u8>, value: u32| image.extend_from_slice(&value.to_le_bytes());
    dword(&mut image, 40);
    dword(&mut image, size as u32);
    dword(&mut image, size as u32 * 2);
    image.extend_from_slice(&[1, 0, 32, 0]);
    for _ in 0..6 {
        dword(&mut image, 0);
    }
    for y in (0..size).rev() {
        for x in 0..size {
            let at = (y * size + x) * 4;
            image.extend_from_slice(&[rgba[at + 2], rgba[at + 1], rgba[at], rgba[at + 3]]);
        }
    }
    image.resize(image.len() + mask_row * size, 0);
    image
}

fn pad4(bytes: &mut Vec<u8>) {
    while bytes.len() % 4 != 0 {
        bytes.push(0);
    }
}

fn utf16z(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect()
}

/// One block of a version resource: length, value length, type, key, value and
/// children, with the padding the format wants between them.
fn block(key: &str, value: &[u8], value_units: usize, text: bool, children: &[u8]) -> Vec<u8> {
    let mut out = vec![0; 6];
    out.extend_from_slice(&utf16z(key));
    pad4(&mut out);
    out.extend_from_slice(value);
    if !children.is_empty() {
        pad4(&mut out);
        out.extend_from_slice(children);
    }
    let length = out.len() as u16;
    out[0..2].copy_from_slice(&length.to_le_bytes());
    out[2..4].copy_from_slice(&(value_units as u16).to_le_bytes());
    out[4..6].copy_from_slice(&u16::from(text).to_le_bytes());
    pad4(&mut out);
    out
}

/// `VS_VERSIONINFO`: what Explorer shows under Properties, Details.
fn version_info() -> Vec<u8> {
    let number = |name: &str| -> u32 {
        std::env::var(name)
            .expect("cargo sets the package version")
            .parse()
            .expect("the package version is numeric")
    };
    let (major, minor, patch) = (
        number("CARGO_PKG_VERSION_MAJOR"),
        number("CARGO_PKG_VERSION_MINOR"),
        number("CARGO_PKG_VERSION_PATCH"),
    );
    let version = format!("{major}.{minor}.{patch}.0");
    let (high, low) = (major << 16 | minor, patch << 16);

    let mut fixed = Vec::with_capacity(52);
    for value in [
        0xFEEF_04BD_u32,
        0x0001_0000,
        high,
        low,
        high,
        low,
        0x3F,
        0,
        0x0004_0004,
        1,
        0,
        0,
        0,
    ] {
        fixed.extend_from_slice(&value.to_le_bytes());
    }

    let mut strings = Vec::new();
    for (key, value) in [
        ("CompanyName", "Soupmeme"),
        ("FileDescription", "Sizequilt disk space visualizer"),
        ("FileVersion", version.as_str()),
        ("InternalName", "Sizequilt"),
        ("LegalCopyright", "Copyright (c) 2026 Soupmeme. MIT License."),
        ("OriginalFilename", "Sizequilt.exe"),
        ("ProductName", "Sizequilt"),
        ("ProductVersion", version.as_str()),
    ] {
        let units = value.encode_utf16().count() + 1;
        strings.extend_from_slice(&block(key, &utf16z(value), units, true, &[]));
    }
    let table = block("040904B0", &[], 0, true, &strings);
    let string_info = block("StringFileInfo", &[], 0, true, &table);
    let translation = [0x09, 0x04, 0xB0, 0x04];
    let var = block("Translation", &translation, 4, false, &[]);
    let var_info = block("VarFileInfo", &[], 0, true, &var);

    let mut children = string_info;
    children.extend_from_slice(&var_info);
    block("VS_VERSION_INFO", &fixed, 52, false, &children)
}

fn directory(entries: u16) -> [u8; 16] {
    let mut table = [0; 16];
    table[14..16].copy_from_slice(&entries.to_le_bytes());
    table
}

fn entry(id: u32, offset: u32, subdirectory: bool) -> [u8; 8] {
    let mut bytes = [0; 8];
    bytes[0..4].copy_from_slice(&id.to_le_bytes());
    let flag = if subdirectory { 0x8000_0000 } else { 0 };
    bytes[4..8].copy_from_slice(&(offset | flag).to_le_bytes());
    bytes
}

/// The resource tree, as a one-section x86-64 COFF object. Resources are sorted
/// by type then id, as the tree requires. The timestamp stays zero so the same
/// source builds the same file.
fn coff_object(resources: &[(u32, u32, Vec<u8>)]) -> Vec<u8> {
    let mut sorted: Vec<&(u32, u32, Vec<u8>)> = resources.iter().collect();
    sorted.sort_by_key(|r| (r.0, r.1));
    let mut types: Vec<u32> = sorted.iter().map(|r| r.0).collect();
    types.dedup();

    // Three levels of directory, then one data entry per resource.
    let size_of = |entries: usize| (16 + entries * 8) as u32;
    let root = size_of(types.len());
    let mut names_at = Vec::new();
    let mut next = root;
    for kind in &types {
        names_at.push(next);
        next += size_of(sorted.iter().filter(|r| r.0 == *kind).count());
    }
    let languages_at = next;
    let entries_at = languages_at + size_of(1) * sorted.len() as u32;
    let blobs_at = entries_at + 16 * sorted.len() as u32;

    let mut section = Vec::new();
    section.extend_from_slice(&directory(types.len() as u16));
    for (kind, at) in types.iter().zip(&names_at) {
        section.extend_from_slice(&entry(*kind, *at, true));
    }
    for kind in &types {
        let count = sorted.iter().filter(|r| r.0 == *kind).count();
        section.extend_from_slice(&directory(count as u16));
        for (index, resource) in sorted.iter().enumerate() {
            if resource.0 == *kind {
                let language = languages_at + size_of(1) * index as u32;
                section.extend_from_slice(&entry(resource.1, language, true));
            }
        }
    }
    for index in 0..sorted.len() as u32 {
        section.extend_from_slice(&directory(1));
        section.extend_from_slice(&entry(ENGLISH_US, entries_at + 16 * index, false));
    }

    let mut blob = blobs_at;
    let mut relocations = Vec::new();
    for resource in &sorted {
        relocations.push(section.len() as u32);
        let mut data_entry = [0; 16];
        data_entry[0..4].copy_from_slice(&blob.to_le_bytes());
        data_entry[4..8].copy_from_slice(&(resource.2.len() as u32).to_le_bytes());
        section.extend_from_slice(&data_entry);
        blob = (blob + resource.2.len() as u32).next_multiple_of(8);
    }
    for resource in &sorted {
        section.extend_from_slice(&resource.2);
        while section.len() % 8 != 0 {
            section.push(0);
        }
    }

    const HEADERS: u32 = 20 + 40;
    let raw_size = section.len() as u32;
    let relocations_at = HEADERS + raw_size;
    let symbols_at = relocations_at + 10 * relocations.len() as u32;
    let count = relocations.len() as u16;

    let mut object = Vec::new();
    object.extend_from_slice(&0x8664_u16.to_le_bytes());
    object.extend_from_slice(&1_u16.to_le_bytes());
    object.extend_from_slice(&0_u32.to_le_bytes());
    object.extend_from_slice(&symbols_at.to_le_bytes());
    object.extend_from_slice(&2_u32.to_le_bytes());
    object.extend_from_slice(&[0; 4]);

    object.extend_from_slice(b".rsrc\0\0\0");
    object.extend_from_slice(&[0; 8]);
    object.extend_from_slice(&raw_size.to_le_bytes());
    object.extend_from_slice(&HEADERS.to_le_bytes());
    object.extend_from_slice(&relocations_at.to_le_bytes());
    object.extend_from_slice(&[0; 4]);
    object.extend_from_slice(&count.to_le_bytes());
    object.extend_from_slice(&[0; 2]);
    object.extend_from_slice(&[0x40, 0, 0x30, 0xC0]);

    object.extend_from_slice(&section);
    for address in relocations {
        object.extend_from_slice(&address.to_le_bytes());
        object.extend_from_slice(&[0; 4]);
        // IMAGE_REL_AMD64_ADDR32NB: the linker turns each offset into an RVA.
        object.extend_from_slice(&3_u16.to_le_bytes());
    }

    object.extend_from_slice(b".rsrc\0\0\0");
    object.extend_from_slice(&[0; 4]);
    object.extend_from_slice(&[1, 0, 0, 0, 3, 1]);
    object.extend_from_slice(&raw_size.to_le_bytes());
    object.extend_from_slice(&count.to_le_bytes());
    object.extend_from_slice(&[0; 12]);
    object.extend_from_slice(&[0; 4]);
    object
}
