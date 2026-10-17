//! Launcher self-update: the sidebar row, the update dialog, and the Settings About card.
//!
//! Drawing goes through [`crate::gui::widgets`]. The check itself lives on the backend.

use watt_core::selfupdate::{self, Phase, UpdateStatus};

use super::App;
use super::dialogs::{self, FormAction};
use crate::gui::backend::DemoBackend;
use crate::gui::theme;
use crate::gui::widgets::{self, Button, Icon, Size, Variant};

/// Dialog and banner state that lasts for this process only.
#[derive(Clone, Debug, Default)]
pub(crate) struct UpdateUi {
    /// The update dialog is open.
    pub(crate) open: bool,
    /// "Later" hides the sidebar row until the next launch.
    pub(crate) dismissed: bool,
    /// A new executable is in place and a restart was requested.
    pub(crate) restart_pending: bool,
    /// [`selfupdate::restart`] has been attempted, so a failure does not retry every frame.
    restart_attempted: bool,
    /// The startup toast for `WATT_START_UPDATED_FROM` has been considered.
    noted_update: bool,
}

/// What the dialog body asked for, besides the shared form flags.
struct DialogChoice {
    form: FormAction,
    later: bool,
    retry: bool,
}

impl App {
    /// Version, notes, and install or update. Also the one-shot "Updated to" toast and the
    /// restart that follows a successful replace, both of which have to run even while the
    /// dialog is closed.
    pub(crate) fn launcher_update_modal(&mut self, ctx: &egui::Context) {
        note_just_updated(self);
        handle_restart(self, ctx);
        if matches!(
            self.backend.launcher_update().phase,
            Phase::Downloading | Phase::Checking
        ) {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        if !self.launcher_update.open {
            return;
        }

        let status = self.backend.launcher_update().clone();
        let op_blocked =
            self.startup_check.is_some() || self.ops.values().any(|slot| slot.running());
        let restarting = self.launcher_update.restart_pending;
        let version_line = status
            .latest_version
            .as_ref()
            .map(|version| format!("Version {version}"));
        let instructions = manual_instructions(&status);
        let (dialog, choice) = dialogs::show_dialog(
            ctx,
            egui::Id::unique("launcher-update"),
            "Update available",
            version_line.as_deref(),
            480.0,
            |dialog| {
                let mut choice = DialogChoice {
                    form: FormAction::fresh(),
                    later: false,
                    retry: false,
                };
                dialog.scroll(|ui| {
                    update_body(ui, ctx, self, &status, &instructions, &mut choice);
                });
                let downloading = status.phase == Phase::Downloading;
                let checking = status.phase == Phase::Checking;
                let enabled =
                    status.can_apply && !op_blocked && !restarting && !downloading && !checking;
                let tip = block_tip(restarting, op_blocked);
                choice.form.valid = enabled;
                dialog.footer(|ui| {
                    if status.can_apply {
                        let mut button = Button::new("Update and restart")
                            .variant(Variant::Primary)
                            .enabled(enabled);
                        if !tip.is_empty() {
                            button = button.on_disabled_hover_text(tip);
                        }
                        if button.show(ui).clicked() {
                            choice.form.submit = true;
                        }
                    }
                    if Button::new("Later")
                        .variant(Variant::Ghost)
                        .enabled(!downloading)
                        .show(ui)
                        .clicked()
                    {
                        choice.later = true;
                        choice.form.interacted = true;
                    }
                });
                choice
            },
        );

        if choice.later {
            self.launcher_update.dismissed = true;
            self.launcher_update.open = false;
        } else if dialogs::form_closed(&dialog, &choice.form, true) {
            self.launcher_update.open = false;
        } else if choice.retry {
            if status.apply_failed && status.asset.is_some() {
                self.backend.apply_launcher_update();
            } else {
                self.backend.check_launcher_update(true);
            }
        } else if dialogs::form_submitted(&dialog, &choice.form) {
            self.backend.apply_launcher_update();
        }
    }
}

/// Snapshot helper: an instance list with the update row showing.
#[allow(dead_code)]
impl App {
    pub(crate) fn for_snapshot_tests_with_update() -> Self {
        let mut backend = DemoBackend::new();
        backend.show_update_available();
        let mut app = Self::with_backend(Box::new(backend));
        // The automatic game check would disable "Update and restart" and paint a spinner
        // labelled as a check. These shots are about the launcher update.
        app.startup_check = None;
        app.still_for_snapshots();
        app
    }

    /// [`Self::for_snapshot_tests_with_update`] with the dialog open.
    pub(crate) fn open_launcher_update_for_tests() -> Self {
        let mut app = Self::for_snapshot_tests_with_update();
        app.launcher_update.open = true;
        app
    }
}

/// Height of the pinned sidebar actions, including the update row when it is visible.
pub(crate) fn sidebar_bottom_height(app: &App) -> f32 {
    let mut height = 12.0 + 36.0 + 8.0 + 28.0 + 12.0;
    if row_text(app).is_some() {
        height += 36.0 + 8.0;
    }
    height
}

/// The compact row above "New instance". Nothing is drawn when watt-start is up to date.
pub(crate) fn update_row(ui: &mut egui::Ui, app: &mut App) {
    let Some(text) = row_text(app) else {
        return;
    };
    let restarting = app.launcher_update.restart_pending;
    let p = theme::palette(ui.ctx());
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 36.0), egui::Sense::click());
    let fill = if response.hovered() && !restarting {
        theme::with_alpha(p.volt, 56)
    } else {
        p.volt_tint
    };
    ui.painter().rect_filled(rect, 8.0, fill);
    let icon_rect = egui::Rect::from_center_size(
        egui::pos2(rect.left() + 18.0, rect.center().y),
        egui::vec2(16.0, 16.0),
    );
    widgets::paint_icon(ui.painter(), icon_rect, Icon::Refresh, p.volt, 0.0);
    let galley = ui
        .painter()
        .layout_no_wrap(text, theme::semibold(13.0), p.volt);
    ui.painter().galley(
        egui::pos2(rect.left() + 34.0, rect.center().y - galley.size().y * 0.5),
        galley,
        p.volt,
    );
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    if response.clicked() && !restarting {
        app.launcher_update.open = true;
    }
}

/// Settings → About. Saves the launcher-update switch itself.
pub(crate) fn about_card(ui: &mut egui::Ui, app: &mut App) {
    let status = app.backend.launcher_update().clone();
    widgets::card(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 8.0;
        widgets::section_heading(ui, "About");
        bright(ui, &format!("Version {}", status.current_version));
        muted(ui, status.install.label());
        let (line, failed) = check_line(&status);
        if failed {
            bad(ui, &line);
        } else {
            muted(ui, &line);
        }
        if !status.signed_build {
            muted(ui, "Updates are not signed in this build.");
        }
        if status.phase == Phase::Available {
            let label = format!(
                "Update available · {}",
                status.latest_version.as_deref().unwrap_or("")
            );
            if Button::new(&label)
                .variant(Variant::Primary)
                .size(Size::Small)
                .show(ui)
                .clicked()
            {
                app.launcher_update.dismissed = false;
                app.launcher_update.open = true;
            }
        }
        let busy = matches!(status.phase, Phase::Checking | Phase::Downloading);
        if Button::new(if status.phase == Phase::Checking {
            "Checking…"
        } else {
            "Check now"
        })
        .variant(Variant::Secondary)
        .size(Size::Small)
        .enabled(!busy)
        .show(ui)
        .clicked()
        {
            app.backend.check_launcher_update(true);
        }
        let mut on = app.settings_draft.check_launcher_updates;
        if widgets::switch_row(
            ui,
            &mut on,
            "Check for launcher updates",
            "Once a day, when watt-start opens.",
        )
        .changed()
        {
            app.settings_draft.check_launcher_updates = on;
            let draft = app.settings_draft.clone();
            app.backend.save_settings(draft);
        }
    });
}

fn update_body(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    app: &mut App,
    status: &UpdateStatus,
    instructions: &str,
    choice: &mut DialogChoice,
) {
    ui.spacing_mut().item_spacing.y = 8.0;
    if status.phase == Phase::Checking {
        muted(ui, "Checking…");
    }
    if !status.notes.is_empty() {
        body_text(ui, &status.notes);
    }
    if !status.notes_url.is_empty()
        && Button::new("What's new")
            .variant(Variant::Ghost)
            .size(Size::Small)
            .show(ui)
            .clicked()
    {
        choice.form.interacted = true;
        if !open_url(&status.notes_url) {
            app.toast("Could not open the link");
        }
    }
    if !instructions.is_empty() {
        body_text(ui, instructions);
        if Button::new("Copy")
            .variant(Variant::Ghost)
            .size(Size::Small)
            .show(ui)
            .clicked()
        {
            ctx.copy_text(instructions.to_owned());
            app.toast("Copied");
            choice.form.interacted = true;
        }
    }
    if let Some(error) = &status.error {
        bad(ui, error);
        if Button::new("Retry")
            .variant(Variant::Secondary)
            .size(Size::Small)
            .show(ui)
            .clicked()
        {
            choice.retry = true;
            choice.form.interacted = true;
        }
    }
    if status.phase == Phase::Downloading {
        muted(ui, "Downloading…");
        widgets::progress_bar(ui, status.progress, theme::VOLT, 6.0);
    }
}

fn row_text(app: &App) -> Option<String> {
    if app.launcher_update.restart_pending {
        return Some("Restarting…".to_owned());
    }
    let status = app.backend.launcher_update();
    if status.phase == Phase::Downloading {
        return Some("Updating…".to_owned());
    }
    if status.phase == Phase::Available && !app.launcher_update.dismissed {
        let version = status.latest_version.as_deref().unwrap_or("");
        return Some(format!("Update available · {version}"));
    }
    None
}

/// Instructions for an install we will not replace, or a release with no executable asset.
fn manual_instructions(status: &UpdateStatus) -> String {
    if !status.install.can_replace()
        && matches!(
            status.phase,
            Phase::Available | Phase::Downloading | Phase::Failed
        )
    {
        return status.install.instructions();
    }
    if status.phase == Phase::Available && status.asset.is_none() {
        return "This release has no automatic update for this system. Download it from the release page.".to_owned();
    }
    String::new()
}

fn check_line(status: &UpdateStatus) -> (String, bool) {
    match status.phase {
        Phase::Checking => ("Checking…".to_owned(), false),
        Phase::Failed => {
            let prefix = if status.apply_failed {
                "Could not update"
            } else {
                "Could not check"
            };
            let detail = status.error.as_deref().unwrap_or("unknown error");
            (format!("{prefix}: {detail}"), true)
        }
        Phase::NoInformation => ("No update information".to_owned(), false),
        Phase::UpToDate => match status.last_checked {
            Some(secs) => (
                format!(
                    "Up to date. Last checked {}.",
                    selfupdate::format_unix_utc(secs)
                ),
                false,
            ),
            None => ("Up to date".to_owned(), false),
        },
        Phase::Idle | Phase::Available | Phase::Downloading => match status.last_checked {
            Some(secs) => (
                format!("Last checked {}.", selfupdate::format_unix_utc(secs)),
                false,
            ),
            None => ("Not checked yet".to_owned(), false),
        },
    }
}

fn block_tip(restarting: bool, op_blocked: bool) -> &'static str {
    if restarting {
        "Restarting…"
    } else if op_blocked {
        "Finish or cancel the running operation first"
    } else {
        ""
    }
}

fn note_just_updated(app: &mut App) {
    if app.launcher_update.noted_update {
        return;
    }
    app.launcher_update.noted_update = true;
    if std::env::var_os("WATT_START_UPDATED_FROM").is_some() {
        app.toast_kind(
            super::toasts::ToastKind::Ok,
            format!("Updated to {}", env!("CARGO_PKG_VERSION")),
        );
    }
}

fn handle_restart(app: &mut App, ctx: &egui::Context) {
    if app.launcher_update.restart_attempted {
        return;
    }
    let Some(plan) = app.backend.launcher_update().restart.clone() else {
        return;
    };
    app.launcher_update.restart_attempted = true;
    app.launcher_update.restart_pending = true;
    match selfupdate::restart(&plan.exe, &plan.from_version) {
        Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
        Err(err) => {
            app.launcher_update.restart_pending = false;
            app.toast_kind(
                super::toasts::ToastKind::Error,
                format!("Could not restart: {err:#}"),
            );
        }
    }
}

fn bright(ui: &mut egui::Ui, text: &str) {
    let p = theme::palette(ui.ctx());
    ui.add(
        egui::Label::new(
            egui::RichText::new(text)
                .font(theme::semibold(14.0))
                .color(p.text_bright),
        )
        .truncate(),
    );
}

fn muted(ui: &mut egui::Ui, text: &str) {
    let p = theme::palette(ui.ctx());
    ui.add(
        egui::Label::new(
            egui::RichText::new(text)
                .font(theme::regular(13.0))
                .color(p.text_muted),
        )
        .wrap(),
    );
}

fn body_text(ui: &mut egui::Ui, text: &str) {
    let p = theme::palette(ui.ctx());
    ui.add(
        egui::Label::new(
            egui::RichText::new(text)
                .font(theme::regular(13.0))
                .color(p.text),
        )
        .wrap(),
    );
}

fn bad(ui: &mut egui::Ui, text: &str) {
    let p = theme::palette(ui.ctx());
    ui.add(
        egui::Label::new(
            egui::RichText::new(text)
                .font(theme::regular(13.0))
                .color(p.bad),
        )
        .wrap(),
    );
}

/// Open `url` in the system browser. Linux `xdg-open`, macOS `open`, Windows `explorer`.
fn open_url(url: &str) -> bool {
    open_url_command(url).spawn().is_ok()
}

#[cfg(target_os = "macos")]
fn open_url_command(url: &str) -> std::process::Command {
    let mut command = std::process::Command::new("open");
    command.arg(url);
    command
}

#[cfg(target_os = "windows")]
fn open_url_command(url: &str) -> std::process::Command {
    let mut command = std::process::Command::new("explorer");
    command.arg(url);
    command
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn open_url_command(url: &str) -> std::process::Command {
    let mut command = std::process::Command::new("xdg-open");
    command.arg(url);
    command
}
