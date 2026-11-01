//! Paints a layout into a pixel buffer, away from the UI thread.
//!
//! The map is drawn once per layout and shown as one texture, so hovering over
//! a map of 400,000 tiles costs one quad, not 400,000 rectangles a frame.
//! Tiles are shaded per pixel: each gets a soft cushion, lit from the top left,
//! which is what lets the eye tell thousands of neighbouring files apart.

use crate::category::CATEGORIES;
use crate::layout::{Rect, Tile, TileKind};
use crate::search::SearchResult;
use crate::tree::Tree;

/// RGBA pixels, row by row. Always opaque.
pub struct Canvas {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<[u8; 4]>,
}

const BACKGROUND: [u8; 3] = [14, 17, 22];
/// Behind a folder's children, and the dark line between sibling folders.
const FOLDER_BODY: [u8; 3] = [18, 21, 27];
const GUTTER: [u8; 3] = [6, 7, 10];
/// A folder or rest tile with no file to take a colour from.
const EMPTY: [u8; 3] = [64, 72, 84];
/// How far a dimmed tile falls toward the background.
const DIM: f32 = 0.82;
/// Folder colours stay muted, so they never read as a file category.
const FRAME_SATURATION: f32 = 0.34;
/// The golden angle: consecutive branches land far apart on the colour wheel.
const BRANCH_HUE_STEP: f32 = 137.508;
/// How much of its file colour a closed folder or rest tile keeps.
const SUMMARY_COLOUR: f32 = 0.42;
/// A file's brightness varies this far either way, by file and by folder.
const FILE_SPECKLE: f32 = 0.10;
const FOLDER_SPECKLE: f32 = 0.08;
/// The width of a rest tile's stripes, in pixels.
const HATCH: usize = 4;
const HATCH_MIN_SIDE: usize = 18;

/// Paints `tiles`. `search`, when given, dims everything it did not light.
pub fn paint(
    tree: &Tree,
    tiles: &[Tile],
    width: usize,
    height: usize,
    search: Option<&SearchResult>,
) -> Canvas {
    let fill = [BACKGROUND[0], BACKGROUND[1], BACKGROUND[2], 255];
    let mut canvas = Canvas {
        width,
        height,
        pixels: vec![fill; width * height],
    };
    let mut rows = Vec::new();
    let mut cols = Vec::new();
    for tile in tiles {
        let lit = search.is_none_or(|s| s.is_lit(tile.node));
        match tile.kind {
            TileKind::Folder => folder(&mut canvas, tile, lit),
            TileKind::ClosedFolder => {
                let rgb = dimmed(summary(tree.dominant_category(tile.node)), lit);
                cushion(&mut canvas, &tile.rect, rgb, &mut rows, &mut cols);
            }
            TileKind::Rest => {
                let rgb = dimmed(summary(rest_category(tree, tile)), lit);
                hatched(&mut canvas, &tile.rect, rgb);
            }
            TileKind::File => {
                let base = CATEGORIES[tree.category(tile.node) as usize].rgb;
                let rgb = dimmed(speckled(base, tile.node, tree.parent(tile.node)), lit);
                cushion(&mut canvas, &tile.rect, rgb, &mut rows, &mut cols);
            }
        }
    }
    canvas
}

/// A folder with its children inside: a dark line against its neighbours, a
/// frame and header in its branch's colour, and a dark body. Tiles nested in it
/// sit inside the frame, so every level of a deep tree shows its own edge.
fn folder(canvas: &mut Canvas, tile: &Tile, lit: bool) {
    let r = tile.rect;
    if tile.depth == 0 {
        flat(canvas, &r, dimmed(FOLDER_BODY, lit));
        return;
    }
    flat(canvas, &r, GUTTER);
    let frame = dimmed(frame_colour(tile.branch, tile.depth), lit);
    flat(canvas, &Rect::new(r.x0, r.y0, r.x1 - 1.0, r.y1 - 1.0), frame);
    let side = if r.width() > 8.0 && r.height() > 8.0 { 2.0 } else { 1.0 };
    let top = r.y0 + tile.header.max(side);
    let body = Rect::new(r.x0 + side, top, r.x1 - side - 1.0, r.y1 - side - 1.0);
    flat(canvas, &body, dimmed(FOLDER_BODY, lit));
}

/// A folder's frame colour: the hue names the branch, and the lightness steps
/// with depth, so a folder never matches the folder around it.
fn frame_colour(branch: u16, depth: u16) -> [u8; 3] {
    let hue = (f32::from(branch) * BRANCH_HUE_STEP + 200.0) % 360.0;
    let lightness = if depth % 2 == 1 { 0.36 } else { 0.23 };
    hsl(hue, FRAME_SATURATION, lightness)
}

fn hsl(hue: f32, saturation: f32, lightness: f32) -> [u8; 3] {
    let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let sector = hue / 60.0;
    let second = chroma * (1.0 - (sector % 2.0 - 1.0).abs());
    let (r, g, b) = match sector as u32 {
        0 => (chroma, second, 0.0),
        1 => (second, chroma, 0.0),
        2 => (0.0, chroma, second),
        3 => (0.0, second, chroma),
        4 => (second, 0.0, chroma),
        _ => (chroma, 0.0, second),
    };
    let base = lightness - chroma / 2.0;
    [r, g, b].map(|c| ((c + base) * 255.0).round() as u8)
}

/// The muted colour of a tile standing for many files.
fn summary(category: Option<u8>) -> [u8; 3] {
    let Some(category) = category else {
        return EMPTY;
    };
    let rgb = CATEGORIES[category as usize].rgb;
    std::array::from_fn(|i| {
        let keep = f32::from(rgb[i]) * SUMMARY_COLOUR;
        (keep + f32::from(EMPTY[i]) * (1.0 - SUMMARY_COLOUR) * 0.8) as u8
    })
}

/// What a rest tile holds most of: the largest child it stands for.
fn rest_category(tree: &Tree, tile: &Tile) -> Option<u8> {
    let children = tree.children(tile.node);
    let first = children.len().checked_sub(tile.rest_count as usize)?;
    tree.dominant_category(*children.get(first)?)
}

/// Spreads a number's bits across `-1.0..1.0`.
fn scatter(n: u32) -> f32 {
    let hashed = n.wrapping_add(1).wrapping_mul(2_654_435_761);
    f32::from((hashed >> 24) as u8) / 127.5 - 1.0
}

/// A file's colour, nudged by file and by folder: neighbours of one type stay
/// tellable apart, and two folders of the same type read as two patches.
fn speckled(rgb: [u8; 3], node: u32, parent: Option<u32>) -> [u8; 3] {
    let folder = parent.map_or(0.0, scatter);
    let light = 1.0 + scatter(node) * FILE_SPECKLE + folder * FOLDER_SPECKLE;
    rgb.map(|c| (f32::from(c) * light).min(255.0) as u8)
}

fn dimmed(rgb: [u8; 3], lit: bool) -> [u8; 3] {
    if lit {
        return rgb;
    }
    std::array::from_fn(|i| {
        (f32::from(rgb[i]) * (1.0 - DIM) + f32::from(BACKGROUND[i]) * DIM) as u8
    })
}

/// A tile's pixel bounds, clamped to the canvas. Edges round the same way for
/// every tile, so neighbours meet with neither a gap nor an overlap.
fn bounds(canvas: &Canvas, r: &Rect) -> (usize, usize, usize, usize) {
    let clamp = |v: f32, max: usize| (v.round().max(0.0) as usize).min(max);
    (
        clamp(r.x0, canvas.width),
        clamp(r.y0, canvas.height),
        clamp(r.x1, canvas.width),
        clamp(r.y1, canvas.height),
    )
}

fn flat(canvas: &mut Canvas, rect: &Rect, rgb: [u8; 3]) {
    let (x0, y0, x1, y1) = bounds(canvas, rect);
    let pixel = [rgb[0], rgb[1], rgb[2], 255];
    for y in y0..y1 {
        let row = y * canvas.width;
        canvas.pixels[row + x0..row + x1].fill(pixel);
    }
}

/// Diagonal stripes: this tile is many things, not one.
fn hatched(canvas: &mut Canvas, rect: &Rect, rgb: [u8; 3]) {
    let (x0, y0, x1, y1) = bounds(canvas, rect);
    // On a tiny tile stripes are only noise.
    if x1 - x0 < HATCH_MIN_SIDE || y1 - y0 < HATCH_MIN_SIDE {
        flat(canvas, rect, rgb);
        return;
    }
    let dark = rgb.map(|c| (f32::from(c) * 0.84) as u8);
    let solid = [rgb[0], rgb[1], rgb[2], 255];
    let stripe = [dark[0], dark[1], dark[2], 255];
    for y in y0..y1 {
        let row = y * canvas.width;
        for x in x0..x1 {
            let on = (x + y) / HATCH % 2 == 0;
            canvas.pixels[row + x] = if on { stripe } else { solid };
        }
    }
}

/// The light along one axis of a tile: a soft bulge, brighter toward the start.
fn axis_light(len: usize, out: &mut Vec<f32>) {
    out.clear();
    let n = len as f32;
    out.extend((0..len).map(|i| {
        let t = (i as f32 + 0.5) / n;
        let bulge = 1.0 - (2.0 * t - 1.0).powi(2);
        0.80 + 0.22 * bulge.sqrt() + 0.10 * (0.5 - t)
    }));
}

fn cushion(
    canvas: &mut Canvas,
    rect: &Rect,
    rgb: [u8; 3],
    rows: &mut Vec<f32>,
    cols: &mut Vec<f32>,
) {
    let (x0, y0, x1, y1) = bounds(canvas, rect);
    let (w, h) = (x1 - x0, y1 - y0);
    if w == 0 || h == 0 {
        return;
    }
    // Under a few pixels a cushion is noise; the tile just takes its colour.
    if w < 3 || h < 3 {
        flat(canvas, rect, rgb);
        return;
    }
    axis_light(w, cols);
    axis_light(h, rows);
    let [r, g, b] = rgb.map(f32::from);
    // Each tile keeps a thin dark edge along its right and bottom.
    let edge = 0.62;
    for (dy, light_y) in rows.iter().enumerate() {
        let row = (y0 + dy) * canvas.width + x0;
        let line = &mut canvas.pixels[row..row + w];
        let last_row = dy + 1 == h;
        for (dx, (pixel, light_x)) in line.iter_mut().zip(cols.iter()).enumerate() {
            let mut light = light_y * light_x;
            if last_row || dx + 1 == w {
                light *= edge;
            }
            *pixel = [
                (r * light).min(255.0) as u8,
                (g * light).min(255.0) as u8,
                (b * light).min(255.0) as u8,
                255,
            ];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{Metrics, layout};
    use crate::tree::test_tree::sample;

    #[test]
    fn every_pixel_of_a_full_map_is_painted() {
        let t = sample();
        let m = Metrics { header: 18.0, pad: 2.0 };
        let tiles = layout(&t, 0, Rect::new(0.0, 0.0, 320.0, 200.0), m);
        let canvas = paint(&t, &tiles, 320, 200, None);
        assert_eq!(canvas.pixels.len(), 320 * 200);
        let untouched = canvas
            .pixels
            .iter()
            .filter(|p| p[..3] == BACKGROUND)
            .count();
        assert_eq!(untouched, 0);
    }

    #[test]
    fn a_tile_past_the_canvas_is_clipped_not_a_panic() {
        let t = sample();
        let tile = Tile {
            rect: Rect::new(-50.0, -50.0, 5000.0, 5000.0),
            node: 3,
            depth: 0,
            branch: 0,
            kind: TileKind::File,
            rest_count: 0,
            rest_bytes: 0,
            header: 0.0,
        };
        let canvas = paint(&t, &[tile], 8, 8, None);
        assert_eq!(canvas.pixels.len(), 64);
    }

    fn contrast(a: [u8; 3], b: [u8; 3]) -> i32 {
        (0..3).map(|i| (i32::from(a[i]) - i32::from(b[i])).abs()).sum()
    }

    #[test]
    fn a_folder_never_matches_the_folder_around_it() {
        for branch in 0..24 {
            for depth in 1..12 {
                let outer = frame_colour(branch, depth);
                let inner = frame_colour(branch, depth + 1);
                assert!(contrast(outer, inner) > 60, "{branch}/{depth}: {outer:?} {inner:?}");
            }
        }
    }

    #[test]
    fn neighbouring_branches_differ_at_the_same_depth() {
        for branch in 0..24 {
            let a = frame_colour(branch, 1);
            let b = frame_colour(branch + 1, 1);
            assert!(contrast(a, b) > 35, "{branch}: {a:?} {b:?}");
        }
    }

    #[test]
    fn colours_convert_exactly_at_the_primaries() {
        assert_eq!(hsl(0.0, 1.0, 0.5), [255, 0, 0]);
        assert_eq!(hsl(120.0, 1.0, 0.5), [0, 255, 0]);
        assert_eq!(hsl(240.0, 1.0, 0.5), [0, 0, 255]);
        assert_eq!(hsl(359.9, 0.0, 0.5), [128, 128, 128]);
    }

    #[test]
    fn a_closed_folder_shows_what_it_holds() {
        let video = summary(Some(0));
        let archive = summary(Some(3));
        assert!(contrast(video, archive) > 40);
        assert!(contrast(video, EMPTY) > 40);
        assert_eq!(summary(None), EMPTY);
    }

    #[test]
    fn neighbouring_files_of_one_type_differ() {
        let rgb = CATEGORIES[3].rgb;
        let differing = (1..200)
            .filter(|n| speckled(rgb, *n, Some(9)) != speckled(rgb, n + 1, Some(9)))
            .count();
        assert!(differing > 190, "{differing}");
    }

    #[test]
    fn speckle_keeps_a_file_near_its_category_colour() {
        let rgb = CATEGORIES[6].rgb;
        for node in 0..500 {
            let shown = speckled(rgb, node, Some(node / 7));
            for i in 0..3 {
                let ratio = f32::from(shown[i]) / f32::from(rgb[i]);
                assert!((0.8..=1.2).contains(&ratio), "{ratio}");
            }
        }
    }

    #[test]
    fn a_rest_tile_is_striped() {
        let t = sample();
        let tile = Tile {
            rect: Rect::new(0.0, 0.0, 32.0, 32.0),
            node: 0,
            depth: 1,
            branch: 0,
            kind: TileKind::Rest,
            rest_count: 1,
            rest_bytes: 5,
            header: 0.0,
        };
        let canvas = paint(&t, &[tile], 32, 32, None);
        let distinct: std::collections::HashSet<_> = canvas.pixels.iter().collect();
        assert_eq!(distinct.len(), 2);
    }

    #[test]
    fn search_dims_what_it_did_not_light() {
        let t = sample();
        let m = Metrics { header: 18.0, pad: 2.0 };
        let tiles = layout(&t, 0, Rect::new(0.0, 0.0, 320.0, 200.0), m);
        let plain = paint(&t, &tiles, 320, 200, None);
        let found = crate::search::run(&t, "no such file");
        let dark = paint(&t, &tiles, 320, 200, Some(&found));
        let sum = |c: &Canvas| c.pixels.iter().map(|p| u64::from(p[0])).sum::<u64>();
        assert!(sum(&dark) < sum(&plain) / 2);
    }
}
