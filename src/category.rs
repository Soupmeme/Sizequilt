//! File categories: what colours the map.

/// A file's kind, from its extension. The discriminant indexes [`CATEGORIES`].
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Category {
    Video = 0,
    Audio,
    Image,
    Archive,
    Disk,
    Document,
    Code,
    Program,
    Data,
    System,
    Other,
}

/// One row of the legend.
pub struct CategoryInfo {
    pub name: &'static str,
    /// Linear-ish sRGB base colour; the rasteriser shades it per pixel.
    pub rgb: [u8; 3],
}

/// Every category, in discriminant order.
pub const CATEGORIES: [CategoryInfo; 11] = [
    CategoryInfo { name: "Video", rgb: [229, 72, 96] },
    CategoryInfo { name: "Audio", rgb: [236, 132, 62] },
    CategoryInfo { name: "Images", rgb: [240, 190, 66] },
    CategoryInfo { name: "Archives", rgb: [150, 200, 72] },
    CategoryInfo { name: "Disk images", rgb: [52, 182, 122] },
    CategoryInfo { name: "Documents", rgb: [44, 190, 190] },
    CategoryInfo { name: "Code", rgb: [64, 150, 240] },
    CategoryInfo { name: "Programs", rgb: [118, 110, 240] },
    CategoryInfo { name: "Data", rgb: [176, 96, 226] },
    CategoryInfo { name: "System", rgb: [224, 96, 180] },
    CategoryInfo { name: "Other", rgb: [120, 132, 148] },
];

/// The extension of `name`, without its dot; `None` for no extension or a
/// leading-dot name such as `.gitignore`.
pub fn extension(name: &[u8]) -> Option<&[u8]> {
    let dot = name.iter().rposition(|b| *b == b'.')?;
    if dot == 0 || dot + 1 == name.len() {
        return None;
    }
    let ext = &name[dot + 1..];
    // A long tail after the last dot is a name with dots in it, not a type.
    (ext.len() <= 16).then_some(ext)
}

/// The category of a file called `name`.
pub fn of_name(name: &[u8]) -> Category {
    let Some(ext) = extension(name) else {
        return Category::Other;
    };
    let mut lower = [0_u8; 16];
    for (out, b) in lower.iter_mut().zip(ext) {
        *out = b.to_ascii_lowercase();
    }
    of_extension(&lower[..ext.len()])
}

fn of_extension(ext: &[u8]) -> Category {
    use Category::*;
    match ext {
        b"mp4" | b"mkv" | b"mov" | b"avi" | b"wmv" | b"webm" | b"m4v" | b"mpg" | b"mpeg"
        | b"ts" | b"m2ts" | b"flv" | b"vob" | b"3gp" | b"mts" | b"prproj" => Video,
        b"mp3" | b"flac" | b"wav" | b"aac" | b"ogg" | b"opus" | b"m4a" | b"wma" | b"aiff"
        | b"aif" | b"mid" | b"midi" | b"als" | b"flp" => Audio,
        b"jpg" | b"jpeg" | b"png" | b"gif" | b"bmp" | b"webp" | b"tif" | b"tiff" | b"heic"
        | b"heif" | b"avif" | b"raw" | b"cr2" | b"cr3" | b"nef" | b"arw" | b"dng" | b"psd"
        | b"psb" | b"ai" | b"svg" | b"ico" | b"exr" | b"hdr" | b"kra" | b"tga" | b"dds" => Image,
        b"zip" | b"rar" | b"7z" | b"tar" | b"gz" | b"bz2" | b"xz" | b"zst" | b"tgz" | b"cab"
        | b"lz4" | b"lzma" | b"pak" | b"wim" | b"esd" => Archive,
        b"iso" | b"img" | b"vhd" | b"vhdx" | b"vmdk" | b"vdi" | b"qcow2" | b"bin" | b"nrg"
        | b"dmg" | b"ova" | b"gho" => Disk,
        b"pdf" | b"doc" | b"docx" | b"xls" | b"xlsx" | b"ppt" | b"pptx" | b"odt" | b"ods"
        | b"odp" | b"rtf" | b"txt" | b"md" | b"epub" | b"mobi" | b"csv" | b"tsv" | b"tex" => {
            Document
        }
        b"rs" | b"c" | b"h" | b"cpp" | b"hpp" | b"cc" | b"cs" | b"java" | b"kt" | b"py"
        | b"js" | b"jsx" | b"tsx" | b"mjs" | b"cjs" | b"go" | b"rb" | b"php" | b"swift"
        | b"html" | b"css" | b"scss" | b"vue" | b"sh" | b"ps1" | b"bat" | b"cmd" | b"lua"
        | b"ipynb" | b"map" | b"pyc" | b"pdb" | b"o" | b"obj" | b"lib" | b"a" | b"rlib"
        | b"rmeta" | b"d" | b"class" | b"wasm" => Code,
        b"exe" | b"dll" | b"msi" | b"msix" | b"appx" | b"sys" | b"ocx" | b"com" | b"scr"
        | b"apk" | b"jar" | b"node" | b"so" | b"mui" => Program,
        b"db" | b"sqlite" | b"sqlite3" | b"mdb" | b"accdb" | b"json" | b"xml" | b"yaml"
        | b"yml" | b"toml" | b"parquet" | b"arrow" | b"h5" | b"npy" | b"npz" | b"pkl" | b"pt"
        | b"pth" | b"ckpt" | b"safetensors" | b"gguf" | b"onnx" | b"tfrecord" | b"dat"
        | b"asset" | b"assets" | b"bundle" | b"uasset" | b"umap" | b"ucas" | b"utoc" | b"vpk"
        | b"blend" | b"fbx" | b"glb" | b"gltf" | b"usd" | b"usdz" | b"abc" | b"vdb" => Data,
        b"log" | b"etl" | b"evtx" | b"tmp" | b"temp" | b"cache" | b"bak" | b"old" | b"dmp"
        | b"mdmp" | b"lock" | b"manifest" | b"cat" | b"mum" | b"msp" | b"nupkg" | b"ldb"
        | b"regtrans-ms" | b"blf" | b"pf" | b"ini" | b"cfg" | b"reg" | b"edb" | b"jfm" => System,
        _ => Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_ignore_case_and_leading_dots() {
        assert_eq!(of_name(b"Movie.MKV"), Category::Video);
        assert_eq!(of_name(b".gitignore"), Category::Other);
        assert_eq!(of_name(b"no_extension"), Category::Other);
        assert_eq!(of_name(b"trailing."), Category::Other);
        assert_eq!(of_name(b"a.tar.gz"), Category::Archive);
    }

    #[test]
    fn a_long_tail_after_a_dot_is_not_a_type() {
        assert_eq!(extension(b"v1.2-a-very-long-release-name"), None);
    }
}
