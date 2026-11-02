//! The view of a finished scan: navigation, the lists, the map and the actions
//! on what they show.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Key, Layout, PointerButton, Sense, Stroke,
    StrokeKind, Vec2,
};
use sizequilt::category::CATEGORIES;
use sizequilt::format;
use sizequilt::guard::{Guard, Request, Verdict};
use sizequilt::layout::TileKind;
use sizequilt::scan::Engine;
use sizequilt::search::{self, SearchResult};
use sizequilt::shell::{self, Recycled};
use sizequilt::tree::{Metric, Tree};

use super::map::Map;
use super::theme::{
    ACCENT, ACCENT_DIM, BG, DANGER, HOVER, LINE, MUTED, PANEL, RAISED, TEXT, WARNING,
    category_color,
};

const ROW_HEIGHT: f32 = 24.0;
const LARGEST_LISTED: usize = 1000;
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(140);
/// One wheel notch at a time, however fast the wheel spins.
const WHEEL_COOLDOWN: Duration = Duration::from_millis(220);

/// How a view hands back to the app.
pub enum ViewExit {
    Rescan(PathBuf, Engine),
    Home,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Folders,
    Largest,
    Types,
}

/// What a click asked for, applied once the frame's borrows are done.
enum Action {
    Select(u32),
    /// Select, then bring it into view on the map and in the folder list.
    Show(u32),
    Zoom(u32),
    Open(u32),
    Reveal(u32),
    CopyPath(u32),
    AskRecycle(u32),
    Toggle(u32),
    Query(String),
    Metric(Metric),
    Up,
    Back,
}

struct Row {
    node: u32,
    depth: u16,
}

struct Recycling {
    node: u32,
    name: String,
    result: Arc<Mutex<Option<Recycled>>>,
}

/// The confirmation on screen: one item, its verdict, and what the person ticked.
struct Confirm {
    node: u32,
    request: Request,
    acknowledged: bool,
}

/// What the pointer is over on the map.
#[derive(Clone, Copy)]
struct Hover {
    node: u32,
    kind: TileKind,
    rest_count: u32,
    rest_bytes: u64,
}

pub struct View {
    tree: Arc<RwLock<Tree>>,
    engine: Engine,
    /// The folder the map shows.
    focus: u32,
    back: Vec<u32>,
    selected: Option<u32>,
    hover: Option<Hover>,
    map_menu_node: Option<u32>,
    tab: Tab,
    expanded: HashSet<u32>,
    rows: Vec<Row>,
    rows_dirty: bool,
    scroll_to_row: Option<usize>,
    largest: Vec<u32>,
    query: String,
    query_edited: Option<Instant>,
    focus_search: bool,
    search: Option<Arc<SearchResult>>,
    search_generation: u64,
    search_done: Arc<Mutex<Option<(u64, SearchResult)>>>,
    map: Map,
    actions: Vec<Action>,
    guard: Arc<Guard>,
    confirm: Option<Confirm>,
    recycling: Option<Recycling>,
    notice: Option<(String, Color32)>,
    last_wheel: Instant,
}

impl View {
    pub fn new(tree: Tree, engine: Engine, ctx: &egui::Context) -> Self {
        let largest = tree.largest_files(LARGEST_LISTED);
        let notice = tree.facts.note.as_ref().map(|why| {
            (format!("Used the folder walker: {why}."), WARNING)
        });
        let tree = Arc::new(RwLock::new(tree));
        let map = Map::new(Arc::clone(&tree), ctx);
        Self {
            tree,
            engine,
            focus: 0,
            back: Vec::new(),
            selected: None,
            hover: None,
            map_menu_node: None,
            tab: Tab::Folders,
            expanded: HashSet::from([0]),
            rows: Vec::new(),
            rows_dirty: true,
            scroll_to_row: None,
            largest,
            query: String::new(),
            query_edited: None,
            focus_search: false,
            search: None,
            search_generation: 0,
            search_done: Arc::new(Mutex::new(None)),
            map,
            actions: Vec::new(),
            guard: Arc::new(Guard::system()),
            confirm: None,
            recycling: None,
            notice,
            last_wheel: Instant::now(),
        }
    }

    pub fn metric(&self) -> Metric {
        self.tree.read().map_or(Metric::Allocated, |t| t.metric())
    }

    /// One frame. Everything reads the tree through one shared lock; what
    /// changes it happens after, in [`View::apply`] and [`View::poll`].
    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<ViewExit> {
        let ctx = ui.ctx().clone();
        self.poll(&ctx);
        self.keyboard(&ctx);
        let mut exit = None;
        {
            let shared = Arc::clone(&self.tree);
            let Ok(tree) = shared.read() else {
                return Some(ViewExit::Home);
            };
            self.rebuild_rows(&tree);
            self.top_bar(ui, &tree, &mut exit);
            self.status_bar(ui, &tree);
            self.side_panel(ui, &tree);
            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(BG))
                .show(ui, |ui| self.map_panel(ui, &tree));
            self.confirm_dialog(&ctx);
        }
        self.apply(&ctx);
        exit
    }

    /// Takes up what background work finished.
    fn poll(&mut self, ctx: &egui::Context) {
        // A pause in typing starts the search.
        if let Some(edited) = self.query_edited {
            let waited = edited.elapsed();
            if waited >= SEARCH_DEBOUNCE {
                self.query_edited = None;
                self.start_search(ctx);
            } else {
                ctx.request_repaint_after(SEARCH_DEBOUNCE - waited);
            }
        }
        let done = self.search_done.lock().ok().and_then(|mut s| s.take());
        if let Some((generation, result)) = done {
            if generation == self.search_generation {
                self.search = Some(Arc::new(result));
                self.map.version += 1;
            }
        }

        let finished = self.recycling.as_ref().and_then(|r| {
            r.result.lock().ok().and_then(|mut s| s.take())
        });
        if let Some(result) = finished {
            let job = self.recycling.take().expect("checked just above");
            match result {
                Recycled::Gone => self.removed(ctx, job.node, &job.name),
                Recycled::Aborted => {
                    self.notice = Some((format!("{} was left where it is.", job.name), MUTED));
                }
                Recycled::Refused(why) => {
                    self.notice = Some((format!("{} was not moved: {why}.", job.name), DANGER));
                }
                Recycled::Failed(why) => {
                    self.notice = Some((
                        format!("{} was not fully moved: {why}. Rescan to see what changed.", job.name),
                        DANGER,
                    ));
                }
            }
        }
    }

    /// `node` left the disk: takes it out of the tree and of everything showing it.
    fn removed(&mut self, ctx: &egui::Context, node: u32, name: &str) {
        let Ok(mut tree) = self.tree.write() else {
            return;
        };
        let parent = tree.parent(node).unwrap_or(0);
        if tree.is_within(self.focus, node) {
            self.focus = parent;
        }
        self.back.retain(|b| !tree.is_within(*b, node));
        if self.selected.is_some_and(|s| tree.is_within(s, node)) {
            self.selected = None;
        }
        tree.remove(node);
        self.largest = tree.largest_files(LARGEST_LISTED);
        drop(tree);
        self.hover = None;
        self.map_menu_node = None;
        self.rows_dirty = true;
        self.map.version += 1;
        if !self.query.trim().is_empty() {
            self.start_search(ctx);
        }
        self.notice = Some((
            format!("{name} moved to the Recycle Bin. Restore it from there if you change your mind."),
            ACCENT,
        ));
    }

    fn start_search(&mut self, ctx: &egui::Context) {
        self.search_generation += 1;
        let query = self.query.trim().to_owned();
        if query.is_empty() {
            if self.search.take().is_some() {
                self.map.version += 1;
            }
            return;
        }
        let generation = self.search_generation;
        let tree = Arc::clone(&self.tree);
        let slot = Arc::clone(&self.search_done);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let Ok(tree) = tree.read() else { return };
            let result = search::run(&tree, &query);
            drop(tree);
            if let Ok(mut slot) = slot.lock() {
                *slot = Some((generation, result));
            }
            ctx.request_repaint();
        });
    }

    fn keyboard(&mut self, ctx: &egui::Context) {
        let back_button = ctx.input(|i| i.pointer.button_pressed(PointerButton::Extra1));
        if back_button {
            self.actions.push(Action::Back);
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, Key::F)) {
            self.focus_search = true;
        }
        if ctx.egui_wants_keyboard_input() || self.confirm.is_some() {
            return;
        }
        let pressed = |key| ctx.input(|i| i.key_pressed(key));
        if pressed(Key::Backspace) {
            self.actions.push(Action::Up);
        }
        if pressed(Key::Escape) {
            self.selected = None;
        }
        if let Some(node) = self.selected {
            if pressed(Key::Enter) {
                self.actions.push(Action::Zoom(node));
            }
            if pressed(Key::Delete) {
                self.actions.push(Action::AskRecycle(node));
            }
        }
    }

    /// Applies what the frame's clicks asked for.
    fn apply(&mut self, ctx: &egui::Context) {
        for action in std::mem::take(&mut self.actions) {
            // This one writes to the tree, so it cannot share the lock below.
            if let Action::Metric(metric) = action {
                self.set_metric(metric);
                continue;
            }
            let shared = Arc::clone(&self.tree);
            let Ok(tree) = shared.read() else { return };
            match action {
                Action::Select(node) => self.selected = Some(node),
                Action::Show(node) => {
                    self.selected = Some(node);
                    if !tree.is_within(node, self.focus) {
                        self.navigate(0);
                    }
                    self.reveal_row(&tree, node);
                }
                Action::Zoom(node) => {
                    let target = if tree.is_dir(node) { Some(node) } else { tree.parent(node) };
                    if let Some(target) = target {
                        self.navigate(target);
                    }
                }
                Action::Open(node) => {
                    let path = tree.path(node);
                    if shell::is_runnable(&path) {
                        self.notice = Some((
                            "Sizequilt doesn't start programs. Use Show in Explorer.".to_owned(),
                            MUTED,
                        ));
                    } else {
                        shell::open(&path);
                    }
                }
                Action::Reveal(node) => shell::reveal(&tree.path(node)),
                Action::CopyPath(node) => {
                    ctx.copy_text(tree.path(node).to_string_lossy().into_owned());
                    self.notice = Some(("Path copied.".to_owned(), MUTED));
                }
                Action::AskRecycle(node) => {
                    self.confirm = Some(Confirm {
                        node,
                        request: Request::new(&self.guard, &tree, node),
                        acknowledged: false,
                    });
                }
                Action::Toggle(node) => {
                    if !self.expanded.remove(&node) {
                        self.expanded.insert(node);
                    }
                    self.rows_dirty = true;
                }
                Action::Query(query) => {
                    self.query = query;
                    self.query_edited = Some(Instant::now() - SEARCH_DEBOUNCE);
                    ctx.request_repaint();
                }
                Action::Metric(_) => {}
                Action::Up => {
                    if let Some(parent) = tree.parent(self.focus) {
                        self.navigate(parent);
                    }
                }
                Action::Back => {
                    if let Some(previous) = self.back.pop() {
                        self.focus = previous;
                    }
                }
            }
        }
    }

    fn navigate(&mut self, to: u32) {
        if to != self.focus {
            self.back.push(self.focus);
            self.focus = to;
            self.hover = None;
        }
    }

    /// Opens the folder list down to `node` and scrolls it there.
    fn reveal_row(&mut self, tree: &Tree, node: u32) {
        for link in tree.chain(node) {
            if link != node {
                self.expanded.insert(link);
            }
        }
        self.rows_dirty = true;
        self.rebuild_rows(tree);
        self.scroll_to_row = self.rows.iter().position(|r| r.node == node);
    }

    fn rebuild_rows(&mut self, tree: &Tree) {
        if !self.rows_dirty {
            return;
        }
        self.rows_dirty = false;
        self.rows.clear();
        let mut stack = vec![(0_u32, 0_u16)];
        while let Some((node, depth)) = stack.pop() {
            self.rows.push(Row { node, depth });
            if self.expanded.contains(&node) {
                stack.extend(tree.children(node).iter().rev().map(|c| (*c, depth + 1)));
            }
        }
    }

    fn set_metric(&mut self, metric: Metric) {
        if let Ok(mut tree) = self.tree.write() {
            tree.set_metric(metric);
            tree.sort_extensions();
            self.largest = tree.largest_files(LARGEST_LISTED);
        }
        self.rows_dirty = true;
        self.map.version += 1;
    }

    fn top_bar(&mut self, ui: &mut egui::Ui, tree: &Tree, exit: &mut Option<ViewExit>) {
        let frame = egui::Frame::new()
            .fill(PANEL)
            .inner_margin(egui::Margin::symmetric(10, 8));
        egui::Panel::top("top").frame(frame).show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.add_enabled(!self.back.is_empty(), egui::Button::new("←")).on_hover_text("Back").clicked() {
                    self.actions.push(Action::Back);
                }
                if ui.add_enabled(self.focus != 0, egui::Button::new("↑")).on_hover_text("Up one folder (Backspace)").clicked() {
                    self.actions.push(Action::Up);
                }
                ui.add_space(4.0);

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("New scan").clicked() {
                        *exit = Some(ViewExit::Home);
                    }
                    if ui.button("Rescan").clicked() {
                        *exit = Some(ViewExit::Rescan(tree.root_path.clone(), self.engine));
                    }

                    let metric = tree.metric();
                    let mut chosen = metric;
                    ui.selectable_value(&mut chosen, Metric::Logical, "File size")
                        .on_hover_text("Rank by each file's length, as Explorer's Size column does");
                    ui.selectable_value(&mut chosen, Metric::Allocated, "On disk")
                        .on_hover_text("Rank by the space files really take: what deleting them frees");
                    if chosen != metric {
                        self.actions.push(Action::Metric(chosen));
                    }

                    let search = ui.add(
                        egui::TextEdit::singleline(&mut self.query)
                            .hint_text("Search names, or *.ext   (Ctrl+F)")
                            .desired_width(240.0),
                    );
                    if std::mem::take(&mut self.focus_search) {
                        search.request_focus();
                    }
                    if search.changed() {
                        self.query_edited = Some(Instant::now());
                    }

                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        self.breadcrumb(ui, tree);
                    });
                });
            });
        });
    }

    fn breadcrumb(&mut self, ui: &mut egui::Ui, tree: &Tree) {
        let chain = tree.chain(self.focus);
        // The end of a long path matters most.
        let skip = chain.len().saturating_sub(5);
        ui.spacing_mut().item_spacing.x = 2.0;
        ui.spacing_mut().button_padding = Vec2::new(5.0, 3.0);
        if skip > 0 {
            ui.colored_label(MUTED, "…");
        }
        for (i, &link) in chain.iter().enumerate().skip(skip) {
            if i > skip || skip > 0 {
                ui.colored_label(MUTED, "›");
            }
            let last = i + 1 == chain.len();
            let text = egui::RichText::new(tree.name(link)).color(if last { TEXT } else { MUTED });
            if ui.add(egui::Button::new(text).frame(false)).clicked() && !last {
                self.actions.push(Action::Zoom(link));
            }
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui, tree: &Tree) {
        let frame = egui::Frame::new()
            .fill(PANEL)
            .inner_margin(egui::Margin::symmetric(10, 5));
        egui::Panel::bottom("status").frame(frame).show(ui, |ui| {
            ui.horizontal(|ui| {
                let f = &tree.facts;
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let summary = format!(
                        "{} items in {}  ·  {}  ·  {}",
                        format::count(f.entries),
                        format::seconds((f.wall_ms + f.build_ms) / 1000.0),
                        f.engine,
                        format::bytes(tree.size(0)),
                    );
                    ui.colored_label(MUTED, egui::RichText::new(summary).small());
                    if f.refused_dirs + f.denied_entries > 0 {
                        let text = format!("{} folders unreadable", format::count(f.refused_dirs));
                        ui.colored_label(WARNING, egui::RichText::new(text).small())
                            .on_hover_text("Windows refused to list these. Their contents are not counted.");
                    }
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        if self.recycling.is_some() {
                            ui.spinner();
                            ui.colored_label(MUTED, egui::RichText::new("Moving to the Recycle Bin…").small());
                        } else if let Some((text, color)) = &self.notice {
                            ui.add(egui::Label::new(egui::RichText::new(text.as_str()).small().color(*color)).truncate());
                        } else {
                            ui.colored_label(
                                MUTED,
                                egui::RichText::new("Double-click or scroll to zoom. Right-click for actions.").small(),
                            );
                        }
                    });
                });
            });
        });
    }

    fn side_panel(&mut self, ui: &mut egui::Ui, tree: &Tree) {
        let frame = egui::Frame::new()
            .fill(PANEL)
            .inner_margin(egui::Margin::same(8));
        egui::Panel::left("side")
            .frame(frame)
            .resizable(true)
            .default_size(360.0)
            .size_range(260.0..=720.0)
            .show(ui, |ui| {
                // A resizable panel keeps the width of its content, so a
                // moment of short content must not shrink it.
                ui.set_min_width(ui.available_width());
                let searching = !self.query.trim().is_empty();
                if self.selected.is_some() {
                    egui::Panel::bottom("details")
                        .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(0, 8)))
                        .show(ui, |ui| self.details(ui, tree));
                }
                if searching {
                    self.results(ui, tree);
                    return;
                }
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.tab, Tab::Folders, "Folders");
                    ui.selectable_value(&mut self.tab, Tab::Largest, "Largest files");
                    ui.selectable_value(&mut self.tab, Tab::Types, "File types");
                });
                ui.add_space(4.0);
                match self.tab {
                    Tab::Folders => self.folders(ui, tree),
                    Tab::Largest => self.largest_list(ui, tree),
                    Tab::Types => self.types(ui, tree),
                }
            });
    }

    fn folders(&mut self, ui: &mut egui::Ui, tree: &Tree) {
        let mut scroll = egui::ScrollArea::vertical()
            .id_salt("folders")
            .auto_shrink([false, false]);
        if let Some(row) = self.scroll_to_row.take() {
            let wanted = row as f32 * row_step(ui);
            scroll = scroll.vertical_scroll_offset((wanted - 120.0).max(0.0));
        }
        let count = self.rows.len();
        scroll.show_rows(ui, ROW_HEIGHT, count, |ui, range| {
            for i in range {
                let Row { node, depth } = self.rows[i];
                self.folder_row(ui, tree, node, depth);
            }
        });
    }

    fn folder_row(&mut self, ui: &mut egui::Ui, tree: &Tree, node: u32, depth: u16) {
        let width = ui.available_width();
        let (rect, response) = ui.allocate_exact_size(Vec2::new(width, ROW_HEIGHT), Sense::click());
        if !ui.is_rect_visible(rect) {
            return;
        }
        let painter = ui.painter_at(rect);
        self.row_background(&painter, rect, node, response.hovered());

        let indent = 12.0 * f32::from(depth.min(24));
        let is_dir = tree.is_dir(node);
        let has_children = is_dir && !tree.children(node).is_empty();
        let arrow_x = rect.left() + 12.0 + indent;
        let mid = rect.center().y;
        if has_children {
            // Drawn, not typed: Segoe UI has no arrow glyphs.
            let c = egui::pos2(arrow_x, mid);
            let points = if self.expanded.contains(&node) {
                vec![c + Vec2::new(-4.0, -2.0), c + Vec2::new(4.0, -2.0), c + Vec2::new(0.0, 3.0)]
            } else {
                vec![c + Vec2::new(-2.0, -4.0), c + Vec2::new(3.0, 0.0), c + Vec2::new(-2.0, 4.0)]
            };
            painter.add(egui::Shape::convex_polygon(points, MUTED, Stroke::NONE));
        } else if !is_dir {
            let chip = egui::Rect::from_center_size(egui::pos2(arrow_x, mid), Vec2::splat(7.0));
            painter.rect_filled(chip, CornerRadius::same(2), category_color(tree.category(node) as usize));
        }

        let size = tree.size(node);
        let size_text = format::bytes(size);
        let right = rect.right() - 8.0;
        let size_galley = painter.layout_no_wrap(size_text, FontId::proportional(12.0), MUTED);
        let size_left = right - size_galley.size().x;
        painter.galley(
            egui::pos2(size_left, mid - size_galley.size().y / 2.0),
            size_galley,
            MUTED,
        );

        // The share of the parent, as a bar along the bottom of the row.
        if let Some(parent) = tree.parent(node) {
            let whole = tree.size(parent).max(1);
            let share = size as f32 / whole as f32;
            let bar = egui::Rect::from_min_size(
                egui::pos2(arrow_x + 10.0, rect.bottom() - 3.0),
                Vec2::new((size_left - arrow_x - 18.0).max(0.0) * share, 2.0),
            );
            painter.rect_filled(bar, CornerRadius::same(1), ACCENT.gamma_multiply(0.55));
        }

        let name = egui::text::LayoutJob::simple_singleline(
            tree.name(node),
            FontId::proportional(13.0),
            if tree.is_refused(node) { WARNING } else { TEXT },
        );
        let name_rect = egui::Rect::from_min_max(
            egui::pos2(arrow_x + 10.0, rect.top()),
            egui::pos2(size_left - 8.0, rect.bottom()),
        );
        let galley = ui.fonts_mut(|f| f.layout_job(name));
        ui.painter_at(name_rect).galley(
            egui::pos2(name_rect.left(), mid - galley.size().y / 2.0 - 1.0),
            galley,
            TEXT,
        );

        let on_arrow = has_children
            && response
                .interact_pointer_pos()
                .is_some_and(|p| (p.x - arrow_x).abs() <= 11.0);
        if response.clicked() {
            if on_arrow {
                self.actions.push(Action::Toggle(node));
            } else {
                self.actions.push(Action::Select(node));
            }
        }
        if response.double_clicked() {
            if is_dir {
                self.actions.push(Action::Zoom(node));
            } else {
                self.actions.push(Action::Open(node));
            }
        }
        self.node_menu(&response, tree, node);
    }

    fn row_background(&self, painter: &egui::Painter, rect: egui::Rect, node: u32, hovered: bool) {
        if self.selected == Some(node) {
            painter.rect_filled(rect, CornerRadius::same(4), ACCENT_DIM);
        } else if hovered || self.hover.is_some_and(|h| h.node == node) {
            painter.rect_filled(rect, CornerRadius::same(4), HOVER);
        }
    }

    fn largest_list(&mut self, ui: &mut egui::Ui, tree: &Tree) {
        let list = std::mem::take(&mut self.largest);
        self.node_list(ui, tree, &list, "largest");
        self.largest = list;
    }

    fn results(&mut self, ui: &mut egui::Ui, tree: &Tree) {
        let Some(search) = self.search.clone() else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.colored_label(MUTED, "Searching…");
            });
            return;
        };
        let summary = format!(
            "{} {} holding {}",
            format::count(search.total),
            if search.total == 1 { "match" } else { "matches" },
            format::bytes(search.bytes)
        );
        ui.horizontal(|ui| {
            ui.colored_label(MUTED, egui::RichText::new(summary).small());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().button_padding = Vec2::new(8.0, 2.0);
                if ui.button("Clear").clicked() {
                    self.actions.push(Action::Query(String::new()));
                }
            });
        });
        if search.total as usize > search.listed.len() {
            ui.colored_label(
                MUTED,
                egui::RichText::new(format!("Showing the largest {}.", search.listed.len())).small(),
            );
        }
        ui.add_space(4.0);
        self.node_list(ui, tree, &search.listed, "results");
    }

    /// A flat list of nodes: name, folder and size.
    fn node_list(&mut self, ui: &mut egui::Ui, tree: &Tree, nodes: &[u32], id: &str) {
        egui::ScrollArea::vertical()
            .id_salt(id)
            .auto_shrink([false, false])
            .show_rows(ui, ROW_HEIGHT + 12.0, nodes.len(), |ui, range| {
                for i in range {
                    self.node_row(ui, tree, nodes[i]);
                }
            });
    }

    fn node_row(&mut self, ui: &mut egui::Ui, tree: &Tree, node: u32) {
        if tree.is_removed(node) {
            return;
        }
        let width = ui.available_width();
        let (rect, response) = ui.allocate_exact_size(Vec2::new(width, ROW_HEIGHT + 12.0), Sense::click());
        let painter = ui.painter_at(rect);
        self.row_background(&painter, rect, node, response.hovered());
        let left = rect.left() + 8.0;

        let color = if tree.is_dir(node) {
            MUTED
        } else {
            category_color(tree.category(node) as usize)
        };
        let chip = egui::Rect::from_center_size(egui::pos2(left + 3.0, rect.top() + 12.0), Vec2::splat(7.0));
        painter.rect_filled(chip, CornerRadius::same(2), color);

        let size = painter.layout_no_wrap(format::bytes(tree.size(node)), FontId::proportional(12.0), MUTED);
        let size_left = rect.right() - 8.0 - size.size().x;
        painter.galley(egui::pos2(size_left, rect.top() + 4.0), size, MUTED);

        let name_clip = egui::Rect::from_min_max(
            egui::pos2(left + 14.0, rect.top()),
            egui::pos2(size_left - 8.0, rect.bottom()),
        );
        ui.painter_at(name_clip).text(
            egui::pos2(name_clip.left(), rect.top() + 3.0),
            Align2::LEFT_TOP,
            tree.name(node),
            FontId::proportional(13.0),
            TEXT,
        );
        let folder = tree
            .parent(node)
            .map(|p| tree.path(p).to_string_lossy().into_owned())
            .unwrap_or_default();
        ui.painter_at(rect.shrink2(Vec2::new(8.0, 0.0))).text(
            egui::pos2(left + 14.0, rect.top() + 20.0),
            Align2::LEFT_TOP,
            folder,
            FontId::proportional(11.0),
            MUTED,
        );

        if response.clicked() {
            self.actions.push(Action::Show(node));
        }
        if response.double_clicked() {
            self.actions.push(Action::Reveal(node));
        }
        self.node_menu(&response, tree, node);
    }

    fn types(&mut self, ui: &mut egui::Ui, tree: &Tree) {
        let metric = tree.metric();
        let size_of = |e: &sizequilt::tree::ExtStat| match metric {
            Metric::Allocated => e.allocated,
            Metric::Logical => e.logical,
        };
        let total = tree.size(0).max(1);
        egui::ScrollArea::vertical()
            .id_salt("types")
            .auto_shrink([false, false])
            .show_rows(ui, ROW_HEIGHT, tree.extensions.len(), |ui, range| {
                for stat in &tree.extensions[range] {
                    let width = ui.available_width();
                    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, ROW_HEIGHT), Sense::click());
                    let painter = ui.painter_at(rect);
                    if response.hovered() {
                        painter.rect_filled(rect, CornerRadius::same(4), HOVER);
                    }
                    let mid = rect.center().y;
                    let color = category_color(stat.category as usize);
                    let chip = egui::Rect::from_center_size(egui::pos2(rect.left() + 11.0, mid), Vec2::splat(7.0));
                    painter.rect_filled(chip, CornerRadius::same(2), color);
                    let label = if stat.ext.is_empty() { "(no extension)".to_owned() } else { format!(".{}", stat.ext) };
                    painter.text(egui::pos2(rect.left() + 22.0, mid - 1.0), Align2::LEFT_CENTER, label, FontId::proportional(13.0), TEXT);
                    let size = size_of(stat);
                    painter.text(
                        egui::pos2(rect.right() - 8.0, mid - 1.0),
                        Align2::RIGHT_CENTER,
                        format!("{}  ·  {}", format::count(stat.files), format::bytes(size)),
                        FontId::proportional(12.0),
                        MUTED,
                    );
                    let bar = egui::Rect::from_min_size(
                        egui::pos2(rect.left() + 22.0, rect.bottom() - 3.0),
                        Vec2::new((width - 30.0) * (size as f32 / total as f32), 2.0),
                    );
                    painter.rect_filled(bar, CornerRadius::same(1), color.gamma_multiply(0.7));
                    if response.clicked() && !stat.ext.is_empty() {
                        self.actions.push(Action::Query(format!("*.{}", stat.ext)));
                    }
                    response.on_hover_text("Click to light these files up on the map");
                }
            });
    }

    fn details(&mut self, ui: &mut egui::Ui, tree: &Tree) {
        let Some(node) = self.selected else { return };
        let verdict = self.guard.check_node(tree, node);
        ui.add(egui::Label::new(egui::RichText::new(tree.name(node)).strong()).truncate());
        let path = tree.path(node).to_string_lossy().into_owned();
        ui.add(egui::Label::new(egui::RichText::new(&path).small().color(MUTED)).truncate())
            .on_hover_text(&path);
        ui.add_space(2.0);
        let mut line = format!(
            "{} on disk  ·  {} file size",
            format::bytes(tree.allocated(node)),
            format::bytes(tree.logical(node)),
        );
        if tree.is_dir(node) {
            line.push_str(&format!("  ·  {} files", format::count(u64::from(tree.files_under(node)))));
        }
        ui.colored_label(MUTED, egui::RichText::new(line).small());
        if let Some(date) = format::date(tree.mtime_ms(node)) {
            ui.colored_label(MUTED, egui::RichText::new(format!("Modified {date}")).small());
        }
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().button_padding = Vec2::new(8.0, 3.0);
            if ui.button("Open").clicked() {
                self.actions.push(Action::Open(node));
            }
            if ui.button("Show in Explorer").clicked() {
                self.actions.push(Action::Reveal(node));
            }
            if ui.button("Copy path").clicked() {
                self.actions.push(Action::CopyPath(node));
            }
            let protected = protection(&verdict);
            if protected.is_none() {
                let free = self.recycling.is_none();
                if ui.add_enabled(free, egui::Button::new("Recycle…")).clicked() {
                    self.actions.push(Action::AskRecycle(node));
                }
            }
        });
        if let Some(why) = protection(&verdict) {
            let line = egui::RichText::new(format!("Protected: {why}.")).small().color(WARNING);
            ui.add(egui::Label::new(line).wrap());
        }
    }

    fn node_menu(&mut self, response: &egui::Response, tree: &Tree, node: u32) {
        let mut chosen = None;
        response.context_menu(|ui| {
            ui.set_min_width(190.0);
            if tree.is_dir(node) && ui.button("Zoom in").clicked() {
                chosen = Some(Action::Zoom(node));
            }
            if ui.button("Open").clicked() {
                chosen = Some(Action::Open(node));
            }
            if ui.button("Show in Explorer").clicked() {
                chosen = Some(Action::Reveal(node));
            }
            if ui.button("Copy path").clicked() {
                chosen = Some(Action::CopyPath(node));
            }
            ui.separator();
            let verdict = self.guard.check_node(tree, node);
            if let Some(why) = protection(&verdict) {
                ui.add_enabled(false, egui::Button::new("Protected"))
                    .on_disabled_hover_text(why);
            } else if ui
                .add_enabled(self.recycling.is_none(), egui::Button::new("Move to Recycle Bin…"))
                .clicked()
            {
                chosen = Some(Action::AskRecycle(node));
            }
        });
        if let Some(action) = chosen {
            self.actions.push(action);
        }
    }

    fn map_panel(&mut self, ui: &mut egui::Ui, tree: &Tree) {
        let ctx = ui.ctx().clone();
        let full = ui.available_rect_before_wrap();
        let legend_height = 26.0;
        let rect = egui::Rect::from_min_max(full.min, egui::pos2(full.right(), full.bottom() - legend_height));
        let response = ui.allocate_rect(rect, Sense::click());

        self.map.update(&ctx, tree, self.focus, rect.size(), self.search.as_ref());
        let painter = ui.painter_at(rect);
        self.map.draw(&painter, rect);
        if !self.map.is_current() {
            ctx.request_repaint_after(Duration::from_millis(30));
        }

        self.hover = None;
        if let Some(pos) = response.hover_pos() {
            if let Some(tile) = self.map.tile_at(rect, pos) {
                self.hover = Some(Hover {
                    node: tile.node,
                    kind: tile.kind,
                    rest_count: tile.rest_count,
                    rest_bytes: tile.rest_bytes,
                });
                let outline = self.map.screen_rect(rect, &tile.rect);
                painter.rect_stroke(outline, CornerRadius::ZERO, Stroke::new(1.5, Color32::WHITE), StrokeKind::Inside);
                // A faint line around the folder it sits in, so a dense area
                // still shows where this file's group ends.
                let group = match tile.kind {
                    TileKind::Rest => Some(tile.node),
                    _ => tree.parent(tile.node),
                };
                let around = group
                    .filter(|g| *g != self.focus)
                    .and_then(|g| self.map.tiles.iter().find(|t| t.node == g && t.kind != TileKind::Rest));
                if let Some(around) = around {
                    let outline = self.map.screen_rect(rect, &around.rect);
                    let faint = Color32::from_rgba_unmultiplied(255, 255, 255, 110);
                    painter.rect_stroke(outline, CornerRadius::ZERO, Stroke::new(1.0, faint), StrokeKind::Inside);
                }
            }
        }
        if let Some(selected) = self.selected {
            if let Some(tile) = self.map.tile_of(tree, selected) {
                let outline = self.map.screen_rect(rect, &tile.rect);
                painter.rect_stroke(outline, CornerRadius::ZERO, Stroke::new(2.0, ACCENT), StrokeKind::Inside);
            }
        }

        if let Some(hover) = self.hover {
            let node = hover.node;
            if response.clicked() {
                self.actions.push(Action::Show(node));
            }
            // A double-click goes straight to the folder under the pointer.
            if response.double_clicked() {
                let target = tree.folder_of(node);
                if target != self.focus {
                    self.actions.push(Action::Zoom(target));
                }
            }
            // The wheel walks there: one notch in zooms a level, one out goes up.
            let wheel = ctx.input(|i| i.smooth_scroll_delta.y);
            if wheel.abs() > 0.5 && self.last_wheel.elapsed() >= WHEEL_COOLDOWN {
                self.last_wheel = Instant::now();
                if wheel > 0.0 {
                    if let Some(target) = tree.step_toward(self.focus, node) {
                        self.actions.push(Action::Zoom(target));
                    }
                } else {
                    self.actions.push(Action::Up);
                }
            }
            if response.secondary_clicked() {
                self.map_menu_node = Some(node);
            }
            let focus_size = tree.size(self.focus);
            response.clone().on_hover_ui_at_pointer(|ui| {
                ui.set_max_width(420.0);
                if hover.kind == TileKind::Rest {
                    ui.label(format!("{} smaller items", format::count(u64::from(hover.rest_count))));
                    ui.colored_label(MUTED, format!(
                        "{}  ·  {} of this view",
                        format::bytes(hover.rest_bytes),
                        format::percent(hover.rest_bytes, focus_size),
                    ));
                } else {
                    ui.label(egui::RichText::new(tree.name(node)).strong());
                    ui.colored_label(MUTED, format!(
                        "{}  ·  {} of this view",
                        format::bytes(tree.size(node)),
                        format::percent(tree.size(node), focus_size),
                    ));
                    ui.colored_label(MUTED, egui::RichText::new(tree.path(node).to_string_lossy()).small());
                }
            });
        }

        // An open menu covers the map and takes the hover away, so it keeps
        // the node it was opened on.
        if let Some(node) = self.map_menu_node {
            self.node_menu(&response, tree, node);
        }

        // The legend, biggest category first.
        let legend = egui::Rect::from_min_max(egui::pos2(full.left(), rect.bottom()), full.max);
        let painter = ui.painter_at(legend);
        painter.rect_filled(legend, CornerRadius::ZERO, PANEL);
        painter.line_segment([legend.left_top(), legend.right_top()], Stroke::new(1.0, LINE));
        let mut order: Vec<usize> = (0..CATEGORIES.len()).collect();
        let category_size = |i: usize| {
            let c = &tree.categories[i];
            if tree.metric() == Metric::Allocated { c.allocated } else { c.logical }
        };
        order.sort_unstable_by_key(|i| std::cmp::Reverse(category_size(*i)));
        let mut x = legend.left() + 10.0;
        let mid = legend.center().y;
        for i in order {
            let size = category_size(i);
            if size == 0 {
                continue;
            }
            let chip = egui::Rect::from_center_size(egui::pos2(x + 4.0, mid), Vec2::splat(9.0));
            painter.rect_filled(chip, CornerRadius::same(2), category_color(i));
            let label = format!("{} {}", CATEGORIES[i].name, format::bytes(size));
            let drawn = painter.text(egui::pos2(x + 13.0, mid - 1.0), Align2::LEFT_CENTER, label, FontId::proportional(11.5), MUTED);
            x = drawn.right() + 14.0;
            if x > legend.right() - 60.0 {
                break;
            }
        }
    }

    /// The one place a move can start: it names one item, shows every reason
    /// for care, and a blocked item has no button to press at all.
    fn confirm_dialog(&mut self, ctx: &egui::Context) {
        let Some(confirm) = &mut self.confirm else { return };
        let request = &confirm.request;
        let mut close = false;
        let mut go = false;
        let frame = egui::Frame::new()
            .fill(RAISED)
            .stroke(Stroke::new(1.0, LINE))
            .corner_radius(CornerRadius::same(8))
            .inner_margin(egui::Margin::same(18));
        let modal = egui::Modal::new(egui::Id::unique("confirm")).frame(frame).show(ctx, |ui| {
            ui.set_width(480.0);
            let blocked = request.verdict.is_blocked();
            ui.heading(if blocked { "This is protected" } else { "Move to the Recycle Bin?" });
            ui.add_space(6.0);
            ui.label(egui::RichText::new(&request.name).strong());
            ui.add(
                egui::Label::new(egui::RichText::new(request.path.to_string_lossy()).small().color(MUTED))
                    .wrap(),
            );
            ui.add_space(6.0);
            let mut what = format!("{} on disk", format::bytes(request.allocated));
            if request.is_dir {
                what.push_str(&format!(", {} files", format::count(u64::from(request.files))));
            }
            ui.colored_label(MUTED, what);
            ui.add_space(6.0);
            let mut needs_tick = false;
            match &request.verdict {
                Verdict::Blocked(why) => {
                    ui.colored_label(DANGER, format!("Sizequilt will not move this: {why}."));
                    ui.colored_label(
                        MUTED,
                        egui::RichText::new("This can't be overridden here, to keep it from happening by accident.").small(),
                    );
                }
                Verdict::Caution(reasons) => {
                    needs_tick = true;
                    for reason in reasons {
                        ui.colored_label(WARNING, reason);
                    }
                    ui.add_space(6.0);
                    ui.scope(|ui| {
                        // A checkbox fills with the same colour as this dialog,
                        // and the warning's whole point is that it gets seen.
                        let widgets = &mut ui.visuals_mut().widgets;
                        widgets.inactive.bg_fill = BG;
                        widgets.inactive.bg_stroke = Stroke::new(1.5, MUTED);
                        widgets.inactive.fg_stroke = Stroke::new(2.0, ACCENT);
                        widgets.hovered.bg_fill = BG;
                        widgets.hovered.bg_stroke = Stroke::new(1.5, ACCENT);
                        widgets.hovered.fg_stroke = Stroke::new(2.0, ACCENT);
                        widgets.active.bg_fill = BG;
                        widgets.active.bg_stroke = Stroke::new(1.5, ACCENT);
                        widgets.active.fg_stroke = Stroke::new(2.0, ACCENT);
                        ui.checkbox(&mut confirm.acknowledged, "I've read this and still want to move it");
                    });
                }
                Verdict::Allowed => {
                    ui.colored_label(MUTED, "You can restore it from the Recycle Bin.");
                }
            }
            ui.add_space(12.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if !blocked {
                    let ready = !needs_tick || confirm.acknowledged;
                    let label = egui::RichText::new("Move to Recycle Bin").color(BG);
                    let button = egui::Button::new(label).fill(ACCENT);
                    if ui.add_enabled(ready, button).clicked() {
                        go = true;
                    }
                }
                if ui.button(if blocked { "Close" } else { "Cancel" }).clicked() {
                    close = true;
                }
            });
        });
        if close || modal.should_close() {
            self.confirm = None;
        }
        if go {
            let Some(Confirm { node, request, acknowledged }) = self.confirm.take() else {
                return;
            };
            let result = Arc::new(Mutex::new(None));
            let slot = Arc::clone(&result);
            let waker = ctx.clone();
            let guard = Arc::clone(&self.guard);
            let name = request.name.clone();
            std::thread::spawn(move || {
                let outcome = guard.move_to_recycle_bin(&request, acknowledged);
                if let Ok(mut slot) = slot.lock() {
                    *slot = Some(outcome);
                }
                waker.request_repaint();
            });
            self.recycling = Some(Recycling { node, name, result });
        }
    }
}

/// Why a verdict rules out moving, if it does.
fn protection(verdict: &Verdict) -> Option<&str> {
    match verdict {
        Verdict::Blocked(why) => Some(why),
        _ => None,
    }
}

/// The height of a list row with its spacing.
fn row_step(ui: &egui::Ui) -> f32 {
    ROW_HEIGHT + ui.spacing().item_spacing.y
}
