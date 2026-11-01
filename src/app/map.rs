//! The map on screen: a worker that lays out and paints off the UI thread, and
//! the texture, labels and hit-testing on it.

use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, RwLock};

use eframe::egui::{self, Color32, FontFamily, FontId, Pos2, Vec2};
use eframe::egui::text::{LayoutJob, TextFormat, TextWrapping};
use sizequilt::category::CATEGORIES;
use sizequilt::format;
use sizequilt::layout::{self, Metrics, Rect, Tile, TileKind};
use sizequilt::raster::{self, Canvas};
use sizequilt::search::SearchResult;
use sizequilt::tree::Tree;

/// The most labels drawn over one map.
const MAX_LABELS: usize = 1800;
const HEADER_POINTS: f32 = 19.0;
const PAD_POINTS: f32 = 2.0;
const LABEL_SIZE: f32 = 11.5;

/// Everything that changes what the map shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Key {
    root: u32,
    width: usize,
    height: usize,
    /// Bumped whenever the tree, the metric or the search changes.
    version: u64,
}

struct Request {
    key: Key,
    pixels_per_point: f32,
    search: Option<Arc<SearchResult>>,
}

struct Painted {
    key: Key,
    tiles: Vec<Tile>,
    canvas: Canvas,
}

/// A label, in points relative to the map's top left.
struct Label {
    pos: Vec2,
    galley: Arc<egui::Galley>,
}

pub struct Map {
    requests: Sender<Request>,
    painted: Arc<Mutex<Option<Painted>>>,
    asked: Option<Key>,
    shown: Option<Key>,
    pub version: u64,
    texture: Option<egui::TextureHandle>,
    pub tiles: Vec<Tile>,
    labels: Vec<Label>,
    /// Pixels of the texture per point of the screen.
    canvas_size: [usize; 2],
}

impl Map {
    pub fn new(tree: Arc<RwLock<Tree>>, ctx: &egui::Context) -> Self {
        let (requests, receiver) = mpsc::channel::<Request>();
        let painted = Arc::new(Mutex::new(None));
        let slot = Arc::clone(&painted);
        let waker = ctx.clone();
        std::thread::Builder::new()
            .name("map".to_owned())
            .spawn(move || {
                // Ends when the view drops its sender.
                while let Ok(mut request) = receiver.recv() {
                    // Only the newest request matters: a resize sends dozens.
                    while let Ok(newer) = receiver.try_recv() {
                        request = newer;
                    }
                    let Ok(tree) = tree.read() else { return };
                    let result = paint(&tree, &request);
                    drop(tree);
                    if let Ok(mut slot) = slot.lock() {
                        *slot = Some(result);
                    }
                    waker.request_repaint();
                }
            })
            .expect("the map thread could not start");
        Self {
            requests,
            painted,
            asked: None,
            shown: None,
            version: 0,
            texture: None,
            tiles: Vec::new(),
            labels: Vec::new(),
            canvas_size: [0, 0],
        }
    }

    /// Whether what is on screen is the newest there will be.
    pub fn is_current(&self) -> bool {
        self.asked.is_some() && self.asked == self.shown
    }

    /// Asks for the map of `root` at this size, and takes up anything painted.
    pub fn update(
        &mut self,
        ctx: &egui::Context,
        tree: &Tree,
        root: u32,
        size: Vec2,
        search: Option<&Arc<SearchResult>>,
    ) {
        let ppp = ctx.pixels_per_point();
        let key = Key {
            root,
            width: (size.x * ppp).round().max(1.0) as usize,
            height: (size.y * ppp).round().max(1.0) as usize,
            version: self.version,
        };
        if self.asked != Some(key) {
            self.asked = Some(key);
            let _ = self.requests.send(Request {
                key,
                pixels_per_point: ppp,
                search: search.cloned(),
            });
        }
        let done = self.painted.lock().ok().and_then(|mut p| p.take());
        if let Some(painted) = done {
            self.take(ctx, tree, painted, search);
        }
    }

    fn take(
        &mut self,
        ctx: &egui::Context,
        tree: &Tree,
        painted: Painted,
        search: Option<&Arc<SearchResult>>,
    ) {
        let Painted { key, tiles, canvas } = painted;
        let pixels = canvas
            .pixels
            .iter()
            .map(|p| Color32::from_rgb(p[0], p[1], p[2]))
            .collect();
        let image = egui::ColorImage::new([canvas.width, canvas.height], pixels);
        let options = egui::TextureOptions::NEAREST;
        match &mut self.texture {
            Some(texture) => texture.set(image, options),
            None => self.texture = Some(ctx.load_texture("map", image, options)),
        }
        self.canvas_size = [canvas.width, canvas.height];
        self.tiles = tiles;
        self.labels = labels(ctx, tree, &self.tiles, search);
        self.shown = Some(key);
    }

    /// Draws the map and its labels into `rect`.
    pub fn draw(&self, painter: &egui::Painter, rect: egui::Rect) {
        let Some(texture) = &self.texture else {
            return;
        };
        let uv = egui::Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
        painter.image(texture.id(), rect, uv, Color32::WHITE);
        // Labels only when the texture fits: mid-resize they would drift.
        let scale = self.scale(rect);
        let settled = (scale - painter.ctx().pixels_per_point()).abs() < 0.01;
        if settled {
            for label in &self.labels {
                painter.galley(rect.min + label.pos, Arc::clone(&label.galley), Color32::WHITE);
            }
        }
    }

    fn scale(&self, rect: egui::Rect) -> f32 {
        self.canvas_size[0] as f32 / rect.width().max(1.0)
    }

    /// The tile under a screen position.
    pub fn tile_at(&self, rect: egui::Rect, pos: Pos2) -> Option<&Tile> {
        let scale = self.scale(rect);
        let (x, y) = ((pos.x - rect.left()) * scale, (pos.y - rect.top()) * scale);
        layout::hit(&self.tiles, x, y)
    }

    /// A tile's rectangle on screen.
    pub fn screen_rect(&self, rect: egui::Rect, tile: &Rect) -> egui::Rect {
        let scale = self.scale(rect);
        egui::Rect::from_min_max(
            rect.min + Vec2::new(tile.x0, tile.y0) / scale,
            rect.min + Vec2::new(tile.x1, tile.y1) / scale,
        )
    }

    /// The tile that best shows `node`: its own, or that of the nearest folder
    /// above it that made it onto the map.
    pub fn tile_of(&self, tree: &Tree, node: u32) -> Option<&Tile> {
        let chain = tree.chain(node);
        for link in chain.iter().rev() {
            let found = self
                .tiles
                .iter()
                .find(|t| t.node == *link && t.kind != TileKind::Rest);
            if found.is_some() {
                return found;
            }
        }
        None
    }
}

fn paint(tree: &Tree, request: &Request) -> Painted {
    let Key { root, width, height, .. } = request.key;
    let ppp = request.pixels_per_point;
    let metrics = Metrics {
        header: HEADER_POINTS * ppp,
        pad: PAD_POINTS * ppp,
    };
    let bounds = Rect::new(0.0, 0.0, width as f32, height as f32);
    let tiles = layout::layout(tree, root, bounds, metrics);
    let canvas = raster::paint(tree, &tiles, width, height, request.search.as_deref());
    Painted {
        key: request.key,
        tiles,
        canvas,
    }
}

/// Dark text on the bright categories, light text on the dark ones.
fn text_on(rgb: [u8; 3]) -> Color32 {
    let [r, g, b] = rgb.map(f32::from);
    if 0.299 * r + 0.587 * g + 0.114 * b > 135.0 {
        Color32::from_rgba_unmultiplied(8, 10, 14, 235)
    } else {
        Color32::from_rgba_unmultiplied(255, 255, 255, 240)
    }
}

fn one_line(text: String, color: Color32, width: f32) -> LayoutJob {
    let mut job = LayoutJob::single_section(
        text,
        TextFormat {
            font_id: FontId::new(LABEL_SIZE, FontFamily::Proportional),
            color,
            ..Default::default()
        },
    );
    job.wrap = TextWrapping {
        max_width: width,
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    job
}

/// Lays out the labels of the largest tiles that have room for one.
fn labels(
    ctx: &egui::Context,
    tree: &Tree,
    tiles: &[Tile],
    search: Option<&Arc<SearchResult>>,
) -> Vec<Label> {
    let ppp = ctx.pixels_per_point();
    let mut candidates: Vec<&Tile> = tiles
        .iter()
        .filter(|t| {
            let (w, h) = (t.rect.width() / ppp, t.rect.height() / ppp);
            let room = match t.kind {
                TileKind::Folder => t.header > 0.0 && w >= 44.0,
                TileKind::ClosedFolder | TileKind::File | TileKind::Rest => w >= 44.0 && h >= 17.0,
            };
            room && search.is_none_or(|s| s.is_lit(t.node))
        })
        .collect();
    candidates.sort_unstable_by(|a, b| b.rect.area().total_cmp(&a.rect.area()));
    candidates.truncate(MAX_LABELS);

    let mut out = Vec::with_capacity(candidates.len());
    ctx.fonts_mut(|fonts| {
        for tile in candidates {
            let (x, y) = (tile.rect.x0 / ppp, tile.rect.y0 / ppp);
            let (w, h) = (tile.rect.width() / ppp, tile.rect.height() / ppp);
            let width = (w - 10.0).max(8.0);
            let size = tree.size(tile.node);
            match tile.kind {
                TileKind::Folder => {
                    let text = format!("{}  {}", tree.name(tile.node), format::bytes(size));
                    let job = one_line(text, Color32::from_rgb(214, 222, 232), width);
                    out.push(Label {
                        pos: Vec2::new(x + 5.0, y + 2.5),
                        galley: fonts.layout_job(job),
                    });
                }
                TileKind::Rest => {
                    let text = format!(
                        "{} smaller items  {}",
                        format::count(u64::from(tile.rest_count)),
                        format::bytes(tile.rest_bytes)
                    );
                    let job = one_line(text, Color32::from_rgb(200, 208, 218), width);
                    out.push(Label {
                        pos: Vec2::new(x + 5.0, y + 3.0),
                        galley: fonts.layout_job(job),
                    });
                }
                TileKind::File | TileKind::ClosedFolder => {
                    let color = if tile.kind == TileKind::File {
                        text_on(CATEGORIES[tree.category(tile.node) as usize].rgb)
                    } else {
                        Color32::from_rgb(214, 222, 232)
                    };
                    let job = one_line(tree.name(tile.node), color, width);
                    out.push(Label {
                        pos: Vec2::new(x + 5.0, y + 3.0),
                        galley: fonts.layout_job(job),
                    });
                    if h >= 34.0 {
                        let job = one_line(format::bytes(size), color.gamma_multiply(0.8), width);
                        out.push(Label {
                            pos: Vec2::new(x + 5.0, y + 17.0),
                            galley: fonts.layout_job(job),
                        });
                    }
                }
            }
        }
    });
    out
}
