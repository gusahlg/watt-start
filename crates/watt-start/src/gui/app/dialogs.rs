//! Modal dialogs: the shared shell, duplicate, and confirmations.
//!
//! New instance and change source live in [`super::new_instance`] and use the same shell.
//! Backdrop clicks cancel a confirmation only. The three dialogs that hold typed input ignore
//! the backdrop so a stray click cannot throw away a name or a path.

use watt_core::meta::AUTO;
use watt_core::ops::OpRequest;

use super::{App, ConfirmAction, MainView, SourceChoice};
use crate::gui::theme;
use crate::gui::widgets::{self, Button, Size, Variant};

/// Op key for launcher maintenance. The settings page reads the same slot.
pub(crate) const MAINTENANCE_KEY: &str = "__maintenance__";

/// State of the duplicate dialog.
#[derive(Clone, Debug, Default)]
pub(crate) struct DuplicateState {
    /// Instance being copied.
    pub instance: String,
    /// Display name, defaulting to "`<title>` (copy)".
    pub display: String,
    /// Directory name. Derived until the user edits it.
    pub dir: String,
    /// The directory field is revealed.
    pub edit_dir: bool,
    /// Stop deriving `dir` from `display`.
    pub dir_edited: bool,
    /// The user has typed, so a validation error may show.
    pub typed: bool,
    /// Request focus on the display field once.
    pub focus_display: bool,
    /// Request focus on the directory field once.
    pub focus_dir: bool,
    /// Copy the instance's worlds along with its manifest.
    pub with_worlds: bool,
}

/// What the dialog shell observed this frame, before the body ran.
pub(crate) struct DialogResult {
    /// Escape on the top modal, with no popup open. Backdrop clicks are not escape.
    pub escape: bool,
    /// The dark backdrop was clicked.
    pub backdrop: bool,
    /// Enter was pressed and no popup was open at the start of the frame.
    pub enter: bool,
}

/// Flags a form body and footer report back to the opener.
pub(crate) struct FormAction {
    /// The primary action is allowed.
    pub valid: bool,
    /// Cancel was clicked.
    pub cancel: bool,
    /// The primary button was clicked.
    pub submit: bool,
    /// Some other control took the click. Enter must not also submit.
    pub interacted: bool,
}

impl FormAction {
    pub(crate) fn fresh() -> Self {
        Self {
            valid: false,
            cancel: false,
            submit: false,
            interacted: false,
        }
    }
}

/// Close on Escape, Cancel, and optionally the backdrop.
pub(crate) fn form_closed(
    dialog: &DialogResult,
    action: &FormAction,
    backdrop_cancels: bool,
) -> bool {
    action.cancel || dialog.escape || (backdrop_cancels && dialog.backdrop)
}

/// Primary button, or Enter when the form is valid and nothing else handled the key.
pub(crate) fn form_submitted(dialog: &DialogResult, action: &FormAction) -> bool {
    action.valid && !action.cancel && (action.submit || (dialog.enter && !action.interacted))
}

/// A centered dialog: card fill, line stroke, radius 16, black backdrop at 55%.
///
/// `width` is the outer width, clamped so a short or zoomed window cannot overflow.
/// The body scrolls; the footer stays put.
pub(crate) fn show_dialog<R>(
    ctx: &egui::Context,
    id: egui::Id,
    title: &str,
    subtitle: Option<&str>,
    width: f32,
    add_contents: impl FnOnce(&mut DialogUi<'_>) -> R,
) -> (DialogResult, R) {
    let screen = ctx.content_rect();
    let outer = width.min((screen.width() - 32.0).max(280.0));
    let inner_w = (outer - 40.0).max(200.0);
    let subtitle_h = if subtitle.is_some() { 22.0 } else { 0.0 };
    let max_body = (screen.height() - 188.0 - subtitle_h).max(80.0);
    let enter = ctx.input(|i| i.key_pressed(egui::Key::Enter)) && !egui::Popup::is_any_open(ctx);

    let modal = egui::Modal::new(id)
        .backdrop_color(egui::Color32::from_black_alpha(140))
        .frame(egui::Frame::NONE)
        .show(ctx, |ui| {
            let p = theme::palette(ui.ctx());
            let shadow = ui.style().visuals.window_shadow;
            egui::Area::new(id.with("panel"))
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .order(egui::Order::Foreground)
                .interactable(true)
                .movable(false)
                .fade_in(false)
                .show(ui.ctx(), |ui| {
                    egui::Frame::new()
                        .fill(p.bg_card)
                        .stroke(egui::Stroke::new(1.0, p.line))
                        .corner_radius(16.0)
                        .inner_margin(20.0)
                        .shadow(shadow)
                        .show(ui, |ui| {
                            ui.set_min_width(inner_w);
                            ui.set_max_width(inner_w);
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(title)
                                        .font(theme::semibold(20.0))
                                        .color(p.text_bright),
                                )
                                .truncate(),
                            );
                            if let Some(subtitle) = subtitle {
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(subtitle)
                                            .font(theme::regular(13.0))
                                            .color(p.text_muted),
                                    )
                                    .truncate(),
                                );
                            }
                            let mut dialog_ui = DialogUi { ui, max_body };
                            add_contents(&mut dialog_ui)
                        })
                        .inner
                })
                .inner
        });

    let backdrop = modal.backdrop_response.clicked();
    let escape = modal.should_close() && !backdrop;
    (
        DialogResult {
            escape,
            backdrop,
            enter,
        },
        modal.inner,
    )
}

/// The dialog's body and footer, used one after the other.
pub(crate) struct DialogUi<'a> {
    ui: &'a mut egui::Ui,
    max_body: f32,
}

/// A thin scrollbar that stays visible while content overflows.
///
/// The theme's default bar is hidden until the pointer is over the scroll area, which
/// makes a clipped dialog look like a broken layout. `outer_margin` pulls the bar in
/// from the clip edge so a full-bleed page does not paint it on the window border.
pub(crate) fn visible_scrollbar(ui: &mut egui::Ui, outer_margin: f32) {
    let mut scroll = egui::style::ScrollStyle::thin();
    scroll.foreground_color = true;
    scroll.floating_width = 4.0;
    scroll.bar_outer_margin = outer_margin;
    ui.style_mut().spacing.scroll = scroll;
}

impl DialogUi<'_> {
    /// Scroll the body when it is taller than the window. Shrinks when the content is short.
    pub(crate) fn scroll<R>(&mut self, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
        visible_scrollbar(self.ui, 0.0);
        egui::ScrollArea::vertical()
            .id_salt("dialog-body")
            .max_height(self.max_body)
            .auto_shrink([false, true])
            .show(self.ui, |ui| {
                ui.set_min_width(ui.available_width());
                add(ui)
            })
            .inner
    }

    /// A separator and a right-aligned button row. Add the primary button first.
    pub(crate) fn footer(&mut self, add: impl FnOnce(&mut egui::Ui)) {
        self.ui.add_space(4.0);
        self.ui.separator();
        self.ui.add_space(4.0);
        self.ui
            .with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                add(ui);
            });
    }
}

/// Directory names already used by an instance or a pending op.
pub(crate) fn taken_dirs(app: &App) -> Vec<String> {
    app.backend
        .instances()
        .iter()
        .map(|e| e.card.name.clone())
        .chain(app.ops.keys().cloned())
        .collect()
}

/// Lowercase ASCII slug, at most 64 characters, hyphens collapsed.
pub(crate) fn slugify(s: &str) -> String {
    let mut out = String::new();
    for c in s.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out.chars().take(64).collect()
}

/// `slug`, or `slug-2`, `slug-3`, … until it is not in `taken`. Empty stays empty.
pub(crate) fn unique_dir(slug: &str, taken: &[String]) -> String {
    if slug.is_empty() || !taken.iter().any(|t| t == slug) {
        return slug.to_owned();
    }
    for n in 2..10_000 {
        let mut candidate = format!("{slug}-{n}");
        if candidate.len() > 64 {
            candidate = candidate.chars().take(64).collect();
        }
        if !taken.iter().any(|t| t == &candidate) {
            return candidate;
        }
    }
    slug.to_owned()
}

/// Whether `s` can be an instance directory name.
pub(crate) fn valid_name(s: &str, taken: &[String]) -> Result<(), &'static str> {
    if s.is_empty() || s.len() > 64 {
        return Err("Use 1-64 characters");
    }
    let mut chars = s.chars();
    let first = chars.next().unwrap();
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return Err("Start with a lowercase letter or a digit");
    }
    if !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return Err("Use only lowercase letters, digits and '-'");
    }
    if taken.iter().any(|t| t == s) {
        return Err("This name is already in use");
    }
    Ok(())
}

/// Why a source cannot be applied yet.
pub(crate) fn source_problem(source: &SourceChoice) -> Option<&'static str> {
    match source {
        SourceChoice::Managed { channel, .. } if channel.trim().is_empty() => {
            Some("Choose a channel")
        }
        SourceChoice::Linked { path, .. } if path.trim().is_empty() => {
            Some("Enter the checkout path")
        }
        _ => None,
    }
}

/// A fresh managed source on `channel`, with engine and mods refs left on `auto`.
pub(crate) fn fresh_managed(channel: &str) -> SourceChoice {
    SourceChoice::Managed {
        channel: channel.to_owned(),
        engine_ref: AUTO.to_owned(),
        mods_ref: AUTO.to_owned(),
    }
}

/// A fresh linked source with no path and no extra repositories.
pub(crate) fn fresh_linked() -> SourceChoice {
    SourceChoice::Linked {
        path: String::new(),
        repositories: Vec::new(),
    }
}

/// Display-name field. Returns whether the text changed. Focuses once.
pub(crate) fn display_field(ui: &mut egui::Ui, display: &mut String, focus: &mut bool) -> bool {
    field_label(ui, "Name");
    let response = widgets::text_input(ui, display, "Display name", f32::INFINITY);
    if *focus {
        response.request_focus();
        *focus = false;
    }
    response.changed()
}

/// Derived directory name, or the editor once Edit is clicked.
///
/// Returns `(dir_changed, edit_clicked)`.
pub(crate) fn dir_line(
    ui: &mut egui::Ui,
    dir: &mut String,
    edit_dir: &mut bool,
    focus_dir: &mut bool,
) -> (bool, bool) {
    if *edit_dir {
        let response = widgets::text_input(ui, dir, "directory-name", f32::INFINITY);
        if *focus_dir {
            response.request_focus();
            *focus_dir = false;
        }
        return (response.changed(), false);
    }
    let p = theme::palette(ui.ctx());
    let mut edit_clicked = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let text_w = (ui.available_width() - 72.0).max(24.0);
        let empty = dir.is_empty();
        let shown = if empty {
            "directory name"
        } else {
            dir.as_str()
        };
        let color = if empty { p.text_faint } else { p.text_muted };
        let galley = ui
            .painter()
            .layout(shown.to_owned(), theme::mono(12.0), color, text_w);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(text_w, 28.0), egui::Sense::hover());
        let y = rect.center().y - galley.size().y * 0.5;
        ui.painter()
            .galley(egui::pos2(rect.left(), y), galley, color);
        if Button::new("Edit")
            .variant(Variant::Ghost)
            .size(Size::Small)
            .show(ui)
            .clicked()
        {
            *edit_dir = true;
            *focus_dir = true;
            edit_clicked = true;
        }
    });
    (false, edit_clicked)
}

/// 12px bad line under a field.
pub(crate) fn error_line(ui: &mut egui::Ui, text: &str) {
    let p = theme::palette(ui.ctx());
    ui.add(
        egui::Label::new(
            egui::RichText::new(text)
                .font(theme::regular(12.0))
                .color(p.bad),
        )
        .wrap(),
    );
}

/// Small bright label above a control.
pub(crate) fn field_label(ui: &mut egui::Ui, text: &str) {
    let p = theme::palette(ui.ctx());
    ui.add(
        egui::Label::new(
            egui::RichText::new(text)
                .font(theme::semibold(13.0))
                .color(p.text_bright),
        )
        .truncate(),
    );
}

fn confirm_focus_id() -> egui::Id {
    egui::Id::unique("confirm-type-focus")
}

fn clear_confirm_focus(ctx: &egui::Context) {
    ctx.data_mut(|d| d.remove::<bool>(confirm_focus_id()));
}

impl App {
    /// Ask before deleting `instance` (its directory name).
    pub(crate) fn ask_delete_instance(&mut self, instance: &str) {
        let title = self.instance_title(instance);
        self.confirm = Some(super::Confirm {
            title: format!("Delete {title}?"),
            body: format!(
                "This deletes the whole instance folder \"{instance}\", including its worlds, \
                 settings and logs. This cannot be undone."
            ),
            type_to_confirm: Some(instance.to_owned()),
            typed: String::new(),
            on_confirm: ConfirmAction::DeleteInstance(instance.to_owned()),
        });
    }

    /// Open the duplicate dialog. The copy is not started until the user confirms.
    pub(crate) fn open_duplicate_dialog(&mut self, instance: &str) {
        let title = self.instance_title(instance);
        let display = format!("{title} (copy)");
        let taken = taken_dirs(self);
        let dir = unique_dir(&slugify(&display), &taken);
        self.duplicate = Some(DuplicateState {
            instance: instance.to_owned(),
            display,
            dir,
            edit_dir: false,
            dir_edited: false,
            typed: false,
            focus_display: true,
            focus_dir: false,
            with_worlds: false,
        });
    }

    pub(crate) fn ask_maintenance(&mut self, title: &str, body: &str, request: OpRequest) {
        self.confirm = Some(super::Confirm {
            title: title.to_owned(),
            body: body.to_owned(),
            type_to_confirm: None,
            typed: String::new(),
            on_confirm: ConfirmAction::Maintenance(request),
        });
    }

    pub(crate) fn duplicate_dialog(&mut self, ctx: &egui::Context) {
        let Some(instance) = self.duplicate.as_ref().map(|s| s.instance.clone()) else {
            return;
        };
        let subtitle = self.instance_title(&instance);
        let taken = taken_dirs(self);
        let (dialog, action) = show_dialog(
            ctx,
            egui::Id::unique("duplicate"),
            "Duplicate instance",
            Some(subtitle.as_str()),
            440.0,
            |dialog| {
                let state = self.duplicate.as_mut().unwrap();
                let mut action = FormAction::fresh();
                let valid = dialog.scroll(|ui| duplicate_body(ui, state, &taken, &mut action));
                action.valid = valid;
                let tip = duplicate_tip(state, &taken);
                dialog.footer(|ui| duplicate_footer(ui, &mut action, tip));
                action
            },
        );
        if form_closed(&dialog, &action, false) {
            self.duplicate = None;
        } else if form_submitted(&dialog, &action) {
            self.confirm_duplicate();
        }
    }

    fn confirm_duplicate(&mut self) {
        let Some(state) = self.duplicate.take() else {
            return;
        };
        let new_name = state.dir.clone();
        let trimmed = state.display.trim();
        let display_name = (!trimmed.is_empty()).then(|| trimmed.to_owned());
        let request = OpRequest::Duplicate {
            instance: state.instance.clone(),
            new_name: new_name.clone(),
            display_name,
            with_worlds: state.with_worlds,
        };
        if self.start_op(new_name.clone(), request) {
            self.main_view = Some(MainView::Instance(new_name));
        } else {
            self.duplicate = Some(state);
        }
    }

    pub(crate) fn confirm_dialog(&mut self, ctx: &egui::Context) {
        if self.confirm.is_none() {
            clear_confirm_focus(ctx);
            return;
        }
        let title = self.confirm.as_ref().unwrap().title.clone();
        let danger = matches!(
            self.confirm.as_ref().map(|c| &c.on_confirm),
            Some(ConfirmAction::DeleteInstance(_))
        );
        let (dialog, action) = show_dialog(
            ctx,
            egui::Id::unique("confirm"),
            &title,
            None,
            420.0,
            |dialog| {
                let confirm = self.confirm.as_mut().unwrap();
                let mut action = FormAction::fresh();
                action.valid = dialog.scroll(|ui| confirm_body(ui, confirm));
                dialog.footer(|ui| confirm_footer(ui, &mut action, danger));
                action
            },
        );
        if form_closed(&dialog, &action, true) {
            self.confirm = None;
            clear_confirm_focus(ctx);
        } else if form_submitted(&dialog, &action) {
            self.run_confirm();
            clear_confirm_focus(ctx);
        }
    }

    fn run_confirm(&mut self) {
        let Some(confirm) = self.confirm.take() else {
            return;
        };
        match confirm.on_confirm {
            ConfirmAction::DeleteInstance(name) => {
                let started = self.start_op(
                    name.clone(),
                    OpRequest::Remove {
                        instance: name.clone(),
                    },
                );
                if started && matches!(&self.main_view, Some(MainView::Instance(n)) if n == &name) {
                    self.main_view = self
                        .backend
                        .instances()
                        .iter()
                        .map(|e| e.card.name.clone())
                        .find(|n| n != &name)
                        .map(MainView::Instance);
                }
            }
            ConfirmAction::Maintenance(request) => {
                self.start_op(MAINTENANCE_KEY, request);
            }
        }
    }
}

fn duplicate_footer(ui: &mut egui::Ui, action: &mut FormAction, tip: &'static str) {
    if Button::new("Duplicate")
        .variant(Variant::Primary)
        .min_width(104.0)
        .enabled(action.valid)
        .on_disabled_hover_text(tip)
        .show(ui)
        .clicked()
    {
        action.submit = true;
    }
    if Button::new("Cancel")
        .variant(Variant::Ghost)
        .min_width(88.0)
        .show(ui)
        .clicked()
    {
        action.cancel = true;
    }
}

fn confirm_footer(ui: &mut egui::Ui, action: &mut FormAction, danger: bool) {
    let variant = if danger {
        Variant::DangerPrimary
    } else {
        Variant::Primary
    };
    let button = Button::new("Confirm").variant(variant).min_width(96.0);
    let button = if action.valid {
        button
    } else {
        button
            .enabled(false)
            .on_disabled_hover_text("Type the name to confirm")
    };
    if button.show(ui).clicked() {
        action.submit = true;
    }
    if Button::new("Cancel")
        .variant(Variant::Ghost)
        .min_width(88.0)
        .show(ui)
        .clicked()
    {
        action.cancel = true;
    }
}

fn duplicate_body(
    ui: &mut egui::Ui,
    state: &mut DuplicateState,
    taken: &[String],
    action: &mut FormAction,
) -> bool {
    let display_changed = display_field(ui, &mut state.display, &mut state.focus_display);
    if !state.dir_edited {
        state.dir = unique_dir(&slugify(&state.display), taken);
    }
    let (dir_changed, edit_clicked) = dir_line(
        ui,
        &mut state.dir,
        &mut state.edit_dir,
        &mut state.focus_dir,
    );
    if edit_clicked {
        action.interacted = true;
    }
    if display_changed || dir_changed {
        state.typed = true;
    }
    if dir_changed {
        state.dir_edited = true;
    }
    let name_err = valid_name(&state.dir, taken).err();
    if state.typed
        && let Some(err) = name_err
    {
        error_line(ui, err);
    }
    ui.add_space(4.0);
    widgets::switch_row(
        ui,
        &mut state.with_worlds,
        "Copy worlds",
        "Include this instance's worlds.",
    );
    name_err.is_none()
}

fn duplicate_tip(state: &DuplicateState, taken: &[String]) -> &'static str {
    match valid_name(&state.dir, taken) {
        Err(err) if state.typed => err,
        _ => "Enter a name",
    }
}

fn confirm_body(ui: &mut egui::Ui, confirm: &mut super::Confirm) -> bool {
    let p = theme::palette(ui.ctx());
    ui.add(
        egui::Label::new(
            egui::RichText::new(&confirm.body)
                .font(theme::regular(14.0))
                .color(p.text),
        )
        .wrap(),
    );
    let Some(name) = confirm.type_to_confirm.clone() else {
        return true;
    };
    ui.add(
        egui::Label::new(
            egui::RichText::new(format!("Type \"{name}\" to confirm."))
                .font(theme::regular(13.0))
                .color(p.text_muted),
        )
        .wrap(),
    );
    let response = widgets::text_input(ui, &mut confirm.typed, &name, f32::INFINITY);
    let focus_id = confirm_focus_id();
    let already = ui
        .ctx()
        .data(|d| d.get_temp::<bool>(focus_id).unwrap_or(false));
    if !already {
        response.request_focus();
        ui.ctx().data_mut(|d| d.insert_temp(focus_id, true));
    }
    confirm.typed == name
}
