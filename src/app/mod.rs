//! The window: the start screen, the scan in progress, and the view of a
//! finished scan.

mod map;
mod start;
mod theme;
mod view;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use eframe::egui;
use sizequilt::scan::{Engine, Outcome, Scan};
use sizequilt::shell::{self, Drive};
use sizequilt::tree::Metric;

use view::{View, ViewExit};

/// Opens the window and runs until it closes. `scan`, when given, starts right
/// away instead of at the start screen.
pub fn run(scan: Option<(PathBuf, Engine)>) -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Sizequilt")
            .with_inner_size([1360.0, 840.0])
            .with_min_inner_size([820.0, 520.0])
            .with_icon(theme::icon()),
        ..Default::default()
    };
    eframe::run_native(
        "Sizequilt",
        options,
        Box::new(|cc| {
            theme::apply(&cc.egui_ctx);
            let mut app = App::new(&cc.egui_ctx);
            if let Some((root, engine)) = scan {
                app.begin_scan(&cc.egui_ctx, root, engine);
            }
            Ok(Box::new(app))
        }),
    )
}

enum Screen {
    Start,
    Scanning(Scan, Engine),
    View(Box<View>),
}

struct App {
    screen: Screen,
    drives: Arc<Mutex<Option<Vec<Drive>>>>,
    picked_folder: Arc<Mutex<Option<PathBuf>>>,
    /// How the next scan reads the disk.
    engine: Engine,
    error: Option<String>,
    metric: Metric,
}

impl App {
    fn new(ctx: &egui::Context) -> Self {
        let app = Self {
            screen: Screen::Start,
            drives: Arc::new(Mutex::new(None)),
            picked_folder: Arc::new(Mutex::new(None)),
            engine: Engine::Auto,
            error: None,
            metric: Metric::Allocated,
        };
        app.refresh_drives(ctx);
        app
    }

    /// Asks Windows for the drives, off the UI thread: a sleeping network
    /// drive can take seconds to answer.
    fn refresh_drives(&self, ctx: &egui::Context) {
        let slot = Arc::clone(&self.drives);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let found = shell::drives();
            if let Ok(mut slot) = slot.lock() {
                *slot = Some(found);
            }
            ctx.request_repaint();
        });
    }

    fn begin_scan(&mut self, ctx: &egui::Context, root: PathBuf, engine: Engine) {
        self.error = None;
        let waker = ctx.clone();
        let scan = Scan::start(root, engine, self.metric, move || waker.request_repaint());
        self.screen = Screen::Scanning(scan, engine);
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // A folder picked in the dialog starts a scan.
        let picked = self.picked_folder.lock().ok().and_then(|mut s| s.take());
        if let Some(root) = picked {
            self.begin_scan(&ctx, root, self.engine);
        }

        // A finished scan changes the screen before anything is drawn.
        if let Screen::Scanning(scan, engine) = &self.screen {
            if let Some(outcome) = scan.take() {
                let engine = *engine;
                match outcome {
                    Outcome::Done(tree) => {
                        self.screen = Screen::View(Box::new(View::new(*tree, engine, &ctx)));
                    }
                    Outcome::Cancelled => self.screen = Screen::Start,
                    Outcome::Failed(why) => {
                        self.error = Some(why);
                        self.screen = Screen::Start;
                    }
                }
            }
        }

        match &mut self.screen {
            Screen::Start => match start::start_screen(
                ui,
                &self.drives,
                &self.picked_folder,
                &mut self.engine,
                self.error.as_deref(),
            ) {
                Some(start::Choice::Scan(root)) => self.begin_scan(&ctx, root, self.engine),
                Some(start::Choice::Refresh) => self.refresh_drives(&ctx),
                None => {}
            },
            Screen::Scanning(scan, _) => start::scan_screen(ui, scan),
            Screen::View(view) => match view.ui(ui) {
                Some(ViewExit::Rescan(root, engine)) => {
                    self.metric = view.metric();
                    self.begin_scan(&ctx, root, engine);
                }
                Some(ViewExit::Home) => {
                    self.metric = view.metric();
                    self.screen = Screen::Start;
                    self.refresh_drives(&ctx);
                }
                None => {}
            },
        }
    }
}
