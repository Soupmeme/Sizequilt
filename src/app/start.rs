//! The two screens around a scan: choosing what to scan, and watching it run.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use eframe::egui::{self, Align, Align2, Color32, CornerRadius, FontId, Layout, Sense, Stroke, StrokeKind, Vec2};
use sizequilt::format;
use sizequilt::scan::{self, Engine, Phase, Scan};
use sizequilt::shell::{self, Drive};

use super::theme::{ACCENT, BG, DANGER, HOVER, LINE, MUTED, RAISED, TEXT, WARNING};

/// What the person chose on the start screen.
pub enum Choice {
    Scan(PathBuf),
    Refresh,
}

const CARD: Vec2 = Vec2::new(286.0, 104.0);

/// The start screen.
pub fn start_screen(
    ui: &mut egui::Ui,
    drives: &Arc<Mutex<Option<Vec<Drive>>>>,
    picked: &Arc<Mutex<Option<PathBuf>>>,
    engine: &mut Engine,
    error: Option<&str>,
) -> Option<Choice> {
    let mut choice = None;
    let frame = egui::Frame::new().fill(BG).inner_margin(egui::Margin::same(36));
    egui::CentralPanel::default().frame(frame).show(ui, |ui| {
        ui.heading("Sizequilt");
        ui.colored_label(MUTED, "See exactly what is filling a drive, then take the space back.");
        ui.add_space(22.0);

        if let Some(error) = error {
            ui.colored_label(DANGER, error);
            ui.add_space(10.0);
        }

        let list = drives.lock().ok().and_then(|d| d.clone());
        match list {
            None => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.colored_label(MUTED, "Looking for drives…");
                });
            }
            Some(list) if list.is_empty() => {
                ui.colored_label(MUTED, "No drives answered. Scan a folder instead.");
            }
            Some(list) => {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::splat(12.0);
                    for drive in &list {
                        if drive_card(ui, drive, *engine) {
                            choice = Some(Choice::Scan(drive.root.clone()));
                        }
                    }
                });
            }
        }

        ui.add_space(20.0);
        ui.horizontal(|ui| {
            if ui.button("Scan a folder…").clicked() {
                let slot = Arc::clone(picked);
                let ctx = ui.ctx().clone();
                // The dialog blocks until it closes, so it gets its own thread.
                std::thread::spawn(move || {
                    let folder = rfd::FileDialog::new().pick_folder();
                    if let (Some(folder), Ok(mut slot)) = (folder, slot.lock()) {
                        *slot = Some(folder);
                        ctx.request_repaint();
                    }
                });
            }
            if ui.button("Refresh drives").clicked() {
                choice = Some(Choice::Refresh);
            }
        });

        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.label("Scan method");
            ui.add_space(6.0);
            for choice in [Engine::Auto, Engine::Walker, Engine::Mft] {
                ui.selectable_value(engine, choice, choice.label());
            }
        });
        ui.add_space(2.0);
        let approval = if shell::is_elevated() {
            "Already running as administrator, so there is no prompt."
        } else {
            "Windows asks for administrator approval once per scan; the access is read-only, \
             and the folder walker takes over if you decline."
        };
        let note = match *engine {
            Engine::Auto => format!(
                "SSDs use the folder walker, which is faster there. Hard drives use the NTFS \
                 file table, which is much faster there. {approval}"
            ),
            Engine::Walker => "Lists folders one by one with no prompt. Folders Windows refuses \
                               to list are not counted."
                .to_owned(),
            Engine::Mft => format!(
                "Reads the NTFS file table on every drive, so it also counts folders Windows \
                 refuses to list. {approval}"
            ),
        };
        ui.colored_label(MUTED, egui::RichText::new(note).small());
    });
    choice
}

/// What a scan of `drive` will use, in words.
fn planned_method(drive: &Drive, engine: Engine) -> &'static str {
    let resolved = match engine {
        Engine::Auto => scan::choose(drive.table_readable(), drive.seek_penalty),
        Engine::Mft if !drive.table_readable() => Engine::Walker,
        chosen => chosen,
    };
    if resolved != Engine::Mft {
        "folder walker"
    } else if shell::is_elevated() {
        "file table"
    } else {
        "file table, admin prompt"
    }
}

/// One drive; true when clicked.
fn drive_card(ui: &mut egui::Ui, drive: &Drive, engine: Engine) -> bool {
    let (rect, response) = ui.allocate_exact_size(CARD, Sense::click());
    let painter = ui.painter_at(rect);
    let hovered = response.hovered();
    painter.rect_filled(rect, CornerRadius::same(8), if hovered { HOVER } else { RAISED });
    painter.rect_stroke(
        rect,
        CornerRadius::same(8),
        Stroke::new(1.0, if hovered { ACCENT } else { LINE }),
        StrokeKind::Inside,
    );

    let left = rect.left() + 16.0;
    painter.text(
        egui::pos2(left, rect.top() + 14.0),
        Align2::LEFT_TOP,
        format!("{}:", drive.letter),
        FontId::proportional(22.0),
        TEXT,
    );
    let label = if drive.label.is_empty() {
        if drive.network { "Network drive" } else { "Local disk" }
    } else {
        drive.label.as_str()
    };
    painter.text(
        egui::pos2(left + 38.0, rect.top() + 14.0),
        Align2::LEFT_TOP,
        label,
        FontId::proportional(13.5),
        TEXT,
    );
    let mut facts = drive.file_system.clone();
    if let Some(media) = drive.media() {
        facts.push_str(&format!("  ·  {media}"));
    }
    facts.push_str(&format!("  ·  {}", planned_method(drive, engine)));
    painter.text(
        egui::pos2(left + 38.0, rect.top() + 32.0),
        Align2::LEFT_TOP,
        facts,
        FontId::proportional(11.5),
        MUTED,
    );

    let used = drive.total.saturating_sub(drive.free);
    let share = if drive.total == 0 { 0.0 } else { used as f32 / drive.total as f32 };
    let bar = egui::Rect::from_min_size(
        egui::pos2(left, rect.bottom() - 40.0),
        Vec2::new(rect.width() - 32.0, 7.0),
    );
    painter.rect_filled(bar, CornerRadius::same(3), BG);
    let mut fill = bar;
    fill.set_width(bar.width() * share);
    painter.rect_filled(
        fill,
        CornerRadius::same(3),
        if share > 0.9 { DANGER } else if share > 0.75 { WARNING } else { ACCENT },
    );
    painter.text(
        egui::pos2(left, rect.bottom() - 26.0),
        Align2::LEFT_TOP,
        format!("{} free of {}", format::bytes(drive.free), format::bytes(drive.total)),
        FontId::proportional(11.5),
        MUTED,
    );
    response.clicked()
}

/// The scan in progress.
pub fn scan_screen(ui: &mut egui::Ui, scan: &Scan) {
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(120));
    let frame = egui::Frame::new().fill(BG).inner_margin(egui::Margin::same(36));
    egui::CentralPanel::default().frame(frame).show(ui, |ui| {
        ui.with_layout(Layout::top_down(Align::Center), |ui| {
            ui.add_space(ui.available_height() * 0.26);
            ui.heading(scan.root.to_string_lossy());
            ui.add_space(10.0);

            let live = &scan.live;
            let phase = live.phase();
            let entries = live.entries.load(Ordering::Acquire);
            let elapsed = scan.started.elapsed().as_secs_f64();
            let status = match phase {
                Phase::Starting => "Starting…".to_owned(),
                Phase::Elevating => "Approve the Windows prompt to read the drive directly…".to_owned(),
                Phase::ReadingTable => "Reading the master file table…".to_owned(),
                Phase::Walking => "Walking folders…".to_owned(),
                Phase::Building => "Building the map…".to_owned(),
            };
            ui.colored_label(MUTED, status);
            ui.add_space(16.0);

            if phase == Phase::Walking && entries > 0 {
                ui.label(
                    egui::RichText::new(format!("{} items", format::count(entries)))
                        .size(30.0)
                        .color(ACCENT),
                );
                ui.colored_label(
                    MUTED,
                    format!(
                        "{}  ·  {}/s",
                        format::bytes(live.bytes.load(Ordering::Acquire)),
                        format::count((entries as f64 / elapsed.max(0.01)) as u64),
                    ),
                );
                ui.add_space(6.0);
                if let Ok(current) = live.current.lock() {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(current.as_str()).small().color(Color32::GRAY),
                        )
                        .truncate(),
                    );
                }
            } else {
                ui.add(egui::Spinner::new().size(30.0).color(ACCENT));
            }
            ui.add_space(14.0);
            ui.colored_label(MUTED, format::seconds(elapsed));
            ui.add_space(14.0);
            if ui.button("Cancel").clicked() {
                scan.cancel();
            }
        });
    });
}
