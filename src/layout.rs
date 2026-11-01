//! The squarified treemap: nested rectangles for one folder of a [`Tree`].
//!
//! Folders are expanded breadth first, so the tile budget goes to the large
//! rectangles, and a folder's children come biggest first, so laying one out
//! stops at the first child too small to see. What is left over becomes a
//! single "rest" tile: the cost of a folder is what is visible, never its
//! length.

use std::collections::VecDeque;

use crate::tree::Tree;

/// The most tiles one layout makes. Anything past it stays a plain folder tile.
pub const MAX_TILES: usize = 400_000;
/// A child smaller than this many square pixels joins its folder's rest tile.
const MIN_AREA: f32 = 12.0;
/// A folder needs this much room, in pixels, to show its children at all.
const MIN_FOLDER_SIDE: f32 = 4.0;
/// A folder needs this much room to show a header strip.
const HEADER_MIN_WIDTH: f32 = 48.0;
const HEADER_MIN_HEIGHT: f32 = 36.0;
/// A folder filling this much of its parent is the map, not a part of it.
const DOMINANT_SHARE: f32 = 0.6;

/// Sizes in pixels, scaled by the display's pixels-per-point.
#[derive(Clone, Copy, Debug)]
pub struct Metrics {
    pub header: f32,
    pub pad: f32,
}

/// An axis-aligned pixel rectangle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl Rect {
    pub fn new(x0: f32, y0: f32, x1: f32, y1: f32) -> Self {
        Self { x0, y0, x1, y1 }
    }

    #[inline]
    pub fn width(&self) -> f32 {
        self.x1 - self.x0
    }

    #[inline]
    pub fn height(&self) -> f32 {
        self.y1 - self.y0
    }

    #[inline]
    pub fn area(&self) -> f32 {
        self.width() * self.height()
    }

    #[inline]
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x0 && x < self.x1 && y >= self.y0 && y < self.y1
    }
}

/// What a tile stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TileKind {
    /// A file, or a link.
    File,
    /// A folder whose children are drawn inside it.
    Folder,
    /// A folder too small, or past the budget, to open up.
    ClosedFolder,
    /// A folder's remaining children, too small to draw one by one.
    Rest,
}

/// One rectangle of the map. Parents always precede their children.
#[derive(Clone, Copy, Debug)]
pub struct Tile {
    pub rect: Rect,
    pub node: u32,
    /// Folder depth below the laid-out root.
    pub depth: u16,
    /// Which group of the map this tile belongs to: it picks the folder hue.
    pub branch: u16,
    pub kind: TileKind,
    /// For a [`TileKind::Rest`]: how many children and what they hold.
    pub rest_count: u32,
    pub rest_bytes: u64,
    /// The node whose header strip this tile carries, as its height in pixels.
    pub header: f32,
}

/// A child or the rest, on its way into a row.
#[derive(Clone, Copy)]
struct Item {
    node: u32,
    area: f32,
    rest_count: u32,
    rest_bytes: u64,
}

/// Lays out `root` in `bounds`.
pub fn layout(tree: &Tree, root: u32, bounds: Rect, metrics: Metrics) -> Vec<Tile> {
    let mut tiles = Vec::with_capacity(4096);
    if bounds.width() < 1.0 || bounds.height() < 1.0 || tree.is_empty() {
        return tiles;
    }
    let root_kind = if tree.is_dir(root) {
        TileKind::Folder
    } else {
        TileKind::File
    };
    tiles.push(Tile {
        rect: bounds,
        node: root,
        depth: 0,
        branch: 0,
        kind: root_kind,
        rest_count: 0,
        rest_bytes: 0,
        header: 0.0,
    });
    if root_kind != TileKind::Folder {
        return tiles;
    }

    // Hues go to the first level where a folder is one part among several: a
    // folder that fills its parent passes the choice down to its own children.
    let mut next_branch = 1_u16;
    let mut queue = VecDeque::from([(0_usize, 0_u16, true)]);
    let mut items: Vec<Item> = Vec::new();
    while let Some((at, depth, hands_out_hues)) = queue.pop_front() {
        let folder = tiles[at];
        let inner = open_folder(&mut tiles, at, metrics);
        let total = tree.size(folder.node);
        if total == 0 || inner.width() < 1.0 || inner.height() < 1.0 {
            tiles[at].kind = TileKind::ClosedFolder;
            continue;
        }
        collect_items(tree, folder.node, inner.area() / total as f32, &mut items);
        if items.is_empty() {
            tiles[at].kind = TileKind::ClosedFolder;
            continue;
        }
        let first_new = tiles.len();
        squarify(&items, inner, |item, rect| {
            let node = item.node;
            let branch = if hands_out_hues {
                next_branch = next_branch.wrapping_add(1);
                next_branch
            } else {
                folder.branch
            };
            let kind = if item.rest_count > 0 {
                TileKind::Rest
            } else if tree.is_dir(node) {
                TileKind::Folder
            } else {
                TileKind::File
            };
            tiles.push(Tile {
                rect,
                node,
                depth: depth + 1,
                branch,
                kind,
                rest_count: item.rest_count,
                rest_bytes: item.rest_bytes,
                header: 0.0,
            });
        });
        for i in first_new..tiles.len() {
            if tiles[i].kind != TileKind::Folder {
                continue;
            }
            let rect = tiles[i].rect;
            if tiles.len() + queue.len() >= MAX_TILES
                || rect.width() < MIN_FOLDER_SIDE
                || rect.height() < MIN_FOLDER_SIDE
            {
                tiles[i].kind = TileKind::ClosedFolder;
            } else {
                let dominant = rect.area() >= inner.area() * DOMINANT_SHARE;
                queue.push_back((i, depth + 1, hands_out_hues && dominant));
            }
        }
    }
    tiles
}

/// Gives a folder its header strip and padding; returns where its children go.
fn open_folder(tiles: &mut [Tile], at: usize, m: Metrics) -> Rect {
    let tile = &mut tiles[at];
    let r = tile.rect;
    if tile.depth == 0 {
        return r;
    }
    let pad = if r.width() > 16.0 && r.height() > 16.0 {
        m.pad
    } else {
        0.0
    };
    let header = if r.width() >= HEADER_MIN_WIDTH && r.height() >= HEADER_MIN_HEIGHT {
        m.header
    } else {
        0.0
    };
    tile.header = header;
    Rect::new(r.x0 + pad, r.y0 + header.max(pad), r.x1 - pad, r.y1 - pad)
}

/// A folder's children as areas, big ones one by one and the tail as one rest.
fn collect_items(tree: &Tree, folder: u32, px_per_byte: f32, items: &mut Vec<Item>) {
    items.clear();
    let children = tree.children(folder);
    let mut visible = 0;
    for &child in children {
        let area = tree.size(child) as f32 * px_per_byte;
        if area < MIN_AREA {
            break;
        }
        items.push(Item {
            node: child,
            area,
            rest_count: 0,
            rest_bytes: 0,
        });
        visible += 1;
    }
    let rest = &children[visible..];
    if !rest.is_empty() {
        let bytes: u64 = rest.iter().map(|c| tree.size(*c)).sum();
        if bytes > 0 {
            items.push(Item {
                node: folder,
                area: bytes as f32 * px_per_byte,
                rest_count: rest.len() as u32,
                rest_bytes: bytes,
            });
        }
    }
}

/// The worst aspect ratio of a row of `sum` total area with these extremes
/// along a side of length `side`.
#[inline]
fn worst(max: f32, min: f32, sum: f32, side: f32) -> f32 {
    let s2 = sum * sum;
    let w2 = side * side;
    (w2 * max / s2).max(s2 / (w2 * min))
}

/// Squarified layout (Bruls, Huizing, van Wijk): lays each row along the short
/// side, closing it when the next item would make the worst tile less square.
fn squarify(items: &[Item], bounds: Rect, mut emit: impl FnMut(&Item, Rect)) {
    let total: f32 = items.iter().map(|i| i.area).sum();
    if total <= 0.0 {
        return;
    }
    // The areas came from a rectangle the padding may since have shrunk.
    let fit = bounds.area() / total;
    let mut area_left = bounds;
    let mut start = 0;
    while start < items.len() {
        let side = area_left.width().min(area_left.height());
        if side <= 0.0 {
            break;
        }
        let first = items[start].area * fit;
        let (mut sum, mut max, mut min) = (first, first, first);
        let mut end = start + 1;
        while end < items.len() {
            let next = items[end].area * fit;
            let grown = sum + next;
            if worst(max.max(next), min.min(next), grown, side) > worst(max, min, sum, side) {
                break;
            }
            sum = grown;
            max = max.max(next);
            min = min.min(next);
            end += 1;
        }
        let row = &items[start..end];
        let last_row = end == items.len();
        if area_left.width() >= area_left.height() {
            // A column on the left edge.
            let width = if last_row {
                area_left.width()
            } else {
                (sum / area_left.height()).min(area_left.width())
            };
            let x1 = area_left.x0 + width;
            let mut y = area_left.y0;
            for (i, item) in row.iter().enumerate() {
                let h = item.area * fit / width;
                let y1 = if i + 1 == row.len() { area_left.y1 } else { y + h };
                emit(item, Rect::new(area_left.x0, y, x1, y1));
                y = y1;
            }
            area_left.x0 = x1;
        } else {
            // A row along the top edge.
            let height = if last_row {
                area_left.height()
            } else {
                (sum / area_left.width()).min(area_left.height())
            };
            let y1 = area_left.y0 + height;
            let mut x = area_left.x0;
            for (i, item) in row.iter().enumerate() {
                let w = item.area * fit / height;
                let x1 = if i + 1 == row.len() { area_left.x1 } else { x + w };
                emit(item, Rect::new(x, area_left.y0, x1, y1));
                x = x1;
            }
            area_left.y0 = y1;
        }
        start = end;
    }
}

/// The topmost tile under a pixel. Children follow parents, so the last hit wins.
pub fn hit(tiles: &[Tile], x: f32, y: f32) -> Option<&Tile> {
    tiles.iter().rev().find(|t| t.rect.contains(x, y))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::test_tree::{sample, tree};

    const M: Metrics = Metrics { header: 18.0, pad: 2.0 };

    fn files(tiles: &[Tile]) -> Vec<&Tile> {
        tiles.iter().filter(|t| t.kind == TileKind::File).collect()
    }

    #[test]
    fn tiles_of_a_folder_cover_it_in_proportion() {
        let t = tree(&[
            (0, "root", true, 0),
            (0, "a", false, 600),
            (0, "b", false, 300),
            (0, "c", false, 100),
        ]);
        let tiles = layout(&t, 0, Rect::new(0.0, 0.0, 1000.0, 500.0), M);
        let leaves = files(&tiles);
        assert_eq!(leaves.len(), 3);
        let area: f32 = leaves.iter().map(|l| l.rect.area()).sum();
        assert!((area - 500_000.0).abs() < 1.0, "{area}");
        let a = leaves.iter().find(|l| l.node == 1).unwrap();
        assert!((a.rect.area() / area - 0.6).abs() < 0.001);
    }

    #[test]
    fn tiles_never_overlap_their_siblings_or_leave_their_folder() {
        let t = sample();
        let bounds = Rect::new(10.0, 20.0, 810.0, 620.0);
        let tiles = layout(&t, 0, bounds, M);
        for a in &tiles {
            assert!(a.rect.x0 >= bounds.x0 - 0.01 && a.rect.x1 <= bounds.x1 + 0.01);
            assert!(a.rect.y0 >= bounds.y0 - 0.01 && a.rect.y1 <= bounds.y1 + 0.01);
        }
        let leaves = files(&tiles);
        for (i, a) in leaves.iter().enumerate() {
            for b in &leaves[i + 1..] {
                let apart = a.rect.x1 <= b.rect.x0 + 0.01
                    || b.rect.x1 <= a.rect.x0 + 0.01
                    || a.rect.y1 <= b.rect.y0 + 0.01
                    || b.rect.y1 <= a.rect.y0 + 0.01;
                assert!(apart, "{:?} {:?}", a.rect, b.rect);
            }
        }
    }

    #[test]
    fn tiles_stay_reasonably_square() {
        let mut rows = vec![(0_u32, "root", true, 0_u64)];
        rows.extend((0..40).map(|i| (0, "f", false, 1000 + i * 37)));
        let t = tree(&rows);
        let tiles = layout(&t, 0, Rect::new(0.0, 0.0, 1600.0, 900.0), M);
        for tile in files(&tiles) {
            let (w, h) = (tile.rect.width(), tile.rect.height());
            assert!(w.max(h) / w.min(h) < 4.0, "{w} x {h}");
        }
    }

    #[test]
    fn the_tail_of_a_long_folder_is_one_rest_tile() {
        let mut rows = vec![(0_u32, "root", true, 0_u64), (0, "huge", false, 100_000_000)];
        rows.extend((0..5000).map(|_| (0, "tiny", false, 1)));
        let t = tree(&rows);
        let tiles = layout(&t, 0, Rect::new(0.0, 0.0, 800.0, 600.0), M);
        let rest: Vec<_> = tiles.iter().filter(|t| t.kind == TileKind::Rest).collect();
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].rest_count, 5000);
        assert_eq!(rest[0].rest_bytes, 5000);
        assert!(tiles.len() < 10);
    }

    #[test]
    fn an_empty_folder_is_closed_not_a_crash() {
        let t = tree(&[(0, "root", true, 0), (0, "empty", true, 0)]);
        let tiles = layout(&t, 0, Rect::new(0.0, 0.0, 100.0, 100.0), M);
        assert_eq!(tiles.len(), 1);
        assert_eq!(tiles[0].kind, TileKind::ClosedFolder);
    }

    #[test]
    fn hit_finds_the_deepest_tile() {
        let t = sample();
        let tiles = layout(&t, 0, Rect::new(0.0, 0.0, 800.0, 600.0), M);
        for leaf in files(&tiles) {
            let (x, y) = (
                (leaf.rect.x0 + leaf.rect.x1) / 2.0,
                (leaf.rect.y0 + leaf.rect.y1) / 2.0,
            );
            assert_eq!(hit(&tiles, x, y).unwrap().node, leaf.node);
        }
    }

    #[test]
    fn a_folder_that_fills_its_parent_hands_hues_to_its_children() {
        let t = tree(&[
            (0, "root", true, 0),
            (0, "huge", true, 0),
            (0, "side", true, 0),
            (1, "one", true, 0),
            (1, "two", true, 0),
            (2, "s", false, 100),
            (3, "a", false, 500),
            (4, "b", false, 400),
        ]);
        let tiles = layout(&t, 0, Rect::new(0.0, 0.0, 800.0, 600.0), M);
        let branch = |node| tiles.iter().find(|t| t.node == node).unwrap().branch;
        assert_ne!(branch(3), branch(4));
        assert_ne!(branch(3), branch(1));
        assert_eq!(branch(5), branch(2));
    }

    #[test]
    fn a_folder_among_others_keeps_one_hue_all_the_way_down() {
        let t = tree(&[
            (0, "root", true, 0),
            (0, "left", true, 0),
            (0, "right", true, 0),
            (1, "inner", true, 0),
            (2, "r", false, 500),
            (3, "l", false, 500),
        ]);
        let tiles = layout(&t, 0, Rect::new(0.0, 0.0, 800.0, 600.0), M);
        let branch = |node| tiles.iter().find(|t| t.node == node).unwrap().branch;
        assert_eq!(branch(3), branch(1));
        assert_ne!(branch(1), branch(2));
    }

    #[test]
    fn a_large_header_folder_gets_its_strip() {
        let t = sample();
        let tiles = layout(&t, 0, Rect::new(0.0, 0.0, 800.0, 600.0), M);
        let big = tiles.iter().find(|t| t.node == 2).unwrap();
        assert_eq!(big.kind, TileKind::Folder);
        assert_eq!(big.header, 18.0);
    }
}
