//! Mods tab: installed packages and the catalogue, with staged changes.
//!
//! The instance page hands this the content rect under the tab strip. Page padding is the
//! parent's; this tab does not add its own inset.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use egui::{
    Align, Color32, CursorIcon, FontId, Id, Layout, Rect, Sense, Stroke, TextFormat, Ui, UiBuilder,
    Vec2, pos2, vec2,
};
use watt_core::catalog::{AvailableMod, PackageCard};
use watt_core::ops::OpRequest;

use super::{App, StagedMods};
use crate::gui::backend::{InstanceEntry, Loaded};
use crate::gui::theme::{self, Palette};
use crate::gui::widgets::{self, Icon, Size, Variant};

const ROW_H: f32 = 44.0;
const BROWSE_H: f32 = 56.0;
const PAD_X: f32 = 12.0;
const ICON_PX: f32 = 24.0;
const MEMBER_INDENT: f32 = 28.0;
const BAR_H: f32 = 56.0;
/// Text above the buttons, used when the one-row bar would clip Discard.
const BAR_H_STACKED: f32 = 96.0;
const BAR_STACK_BREAK: f32 = 480.0;
const BAR_GAP: f32 = 8.0;
const TOOLBAR_WRAP: f32 = 720.0;
/// Name column: the 24px glyph, the 12px row pad, and the gap before the text.
const NAME_X: f32 = PAD_X + ICON_PX + 10.0;
/// 8% of 255. Staged-row tint.
const TINT: u8 = 20;

/// Per-instance Mods tab UI. The segment is chosen once, the first time that instance is shown.
#[derive(Clone, Debug, Default)]
pub(crate) struct ModsTabState {
    views: HashMap<String, InstanceModsUi>,
}

impl ModsTabState {
    /// Whether the Mods tab of `instance` currently lists libraries (the header's mod count
    /// follows it).
    pub(crate) fn shows_libraries(&self, instance: &str) -> bool {
        self.views.get(instance).is_some_and(|v| v.show_libraries)
    }
}

#[derive(Clone, Debug)]
struct InstanceModsUi {
    /// 0 = Installed, 1 = Browse.
    segment: usize,
    search: String,
    category: Option<String>,
    show_libraries: bool,
    expanded: HashSet<String>,
    deps_expanded: bool,
    /// Bundles the user collapsed while a filter was forcing them open. Beats that force
    /// until the filter changes.
    collapsed: HashSet<String>,
    /// Same override for the Dependencies group.
    deps_collapsed: bool,
}

impl InstanceModsUi {
    fn fresh(has_packages: bool) -> Self {
        Self {
            segment: usize::from(!has_packages),
            search: String::new(),
            category: None,
            show_libraries: false,
            expanded: HashSet::new(),
            deps_expanded: false,
            collapsed: HashSet::new(),
            deps_collapsed: false,
        }
    }
}

/// What a control asked for. Applied after the frame is painted.
enum Click {
    None,
    Add(String),
    Remove(String),
    UndoAdd(String),
    UndoRemove(String),
    Apply,
    Discard,
    Retry,
}

enum Catalog {
    Pending,
    Ready {
        mods: Vec<AvailableMod>,
        warnings: Vec<String>,
    },
    Failed(String),
}

impl Catalog {
    fn from_loaded(loaded: Loaded<Arc<(Vec<AvailableMod>, Vec<String>)>>) -> Self {
        match loaded {
            Loaded::Idle | Loaded::Loading => Self::Pending,
            Loaded::Ready(data) => {
                let (mods, warnings) = (*data).clone();
                Self::Ready { mods, warnings }
            }
            Loaded::Err(message) => Self::Failed(message),
        }
    }

    fn mods(&self) -> &[AvailableMod] {
        match self {
            Self::Ready { mods, .. } => mods,
            Self::Pending | Self::Failed(_) => &[],
        }
    }
}

struct Filter {
    query: String,
    category: Option<String>,
    show_libraries: bool,
}

impl Filter {
    fn from_view(view: &InstanceModsUi) -> Self {
        Self {
            query: view.search.trim().to_lowercase(),
            category: view.category.clone(),
            show_libraries: view.show_libraries,
        }
    }

    fn active(&self) -> bool {
        !self.query.is_empty() || self.category.is_some()
    }

    fn search(&self, name: &str, id: &str, desc: &str) -> bool {
        self.query.is_empty()
            || name.to_lowercase().contains(&self.query)
            || id.to_lowercase().contains(&self.query)
            || desc.to_lowercase().contains(&self.query)
    }

    fn category_ok(&self, cats: &[String]) -> bool {
        self.category
            .as_ref()
            .is_none_or(|want| cats.iter().any(|cat| cat == want))
    }

    fn matches(&self, name: &str, id: &str, desc: &str, cats: &[String]) -> bool {
        self.search(name, id, desc) && self.category_ok(cats)
    }
}

struct AddRow {
    id: String,
    name: String,
    version: String,
    description: String,
    kind: String,
}

struct PlainRow {
    id: String,
    name: String,
    description: String,
    removing: bool,
}

struct MemberRow {
    pkg: PackageCard,
    removing: bool,
}

struct DirectRow {
    pkg: PackageCard,
    open: bool,
    /// Open because a filter or a removal forced it, not because the user expanded it.
    forced: bool,
    members: Vec<MemberRow>,
    member_total: usize,
    removing: bool,
}

struct DepGroup {
    count: usize,
    open: bool,
    forced: bool,
    rows: Vec<MemberRow>,
}

struct InstalledModel {
    adding: Vec<AddRow>,
    unresolved: Vec<PlainRow>,
    orphans: Vec<PlainRow>,
    direct: Vec<DirectRow>,
    deps: DepGroup,
    hidden_libraries: usize,
}

impl InstalledModel {
    fn has_body(&self) -> bool {
        !self.adding.is_empty()
            || !self.unresolved.is_empty()
            || !self.orphans.is_empty()
            || !self.direct.is_empty()
            || self.deps.count > 0
    }

    /// Segment badge: top-level rows, not nested members or dependencies.
    fn top_count(&self) -> usize {
        self.adding.len() + self.unresolved.len() + self.orphans.len() + self.direct.len()
    }
}

enum BrowseAction {
    Add,
    Installed,
    Indirect,
    Added,
    Removing,
}

struct BrowseRow {
    id: String,
    name: String,
    version: String,
    sub: String,
    kind: String,
    categories: Vec<String>,
    members: Vec<String>,
    member_count: usize,
    action: BrowseAction,
    required_by: String,
}

struct BrowseModel {
    rows: Vec<BrowseRow>,
    hidden_libraries: usize,
    /// Non-library catalogue entries, before the search filter.
    non_library: usize,
}

enum EmptyKind {
    NoMods,
    NoCatalogue,
    OnlyInstalledLibraries,
    OnlyCatalogueLibraries,
    Filtered,
}

enum EmptyAction {
    None,
    Browse,
    ShowLibraries,
    Clear,
}

enum Trail {
    Members {
        label: String,
        open: bool,
        tip: String,
        toggle: bool,
    },
    Pill {
        text: String,
        fg: Color32,
        bg: Color32,
    },
    Added,
    Note(String),
    Remove,
    Undo,
    Add,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RowHit {
    None,
    Body,
    Members,
    Remove,
    Undo,
    Add,
}

impl App {
    pub(crate) fn mods_tab(&mut self, ui: &mut egui::Ui, entry: &InstanceEntry) {
        let name = entry.card.name.clone();
        let has_packages = !entry.card.packages.is_empty();
        let catalog = Catalog::from_loaded(self.backend.available_mods(&name));
        let mut view = self
            .mods_ui
            .views
            .entry(name.clone())
            .or_insert_with(|| InstanceModsUi::fresh(has_packages))
            .clone();
        let staged = self.staged_mods.get(&name).cloned().unwrap_or_default();
        let rebuild = self.settings_draft.rebuild_after_mod_change;
        let busy = self.busy(&name) || entry.status.busy;
        let wait = wait_reason(self, &name, entry.status.busy);
        let hint = rebuild_hint(rebuild, last_build_ms(entry));

        let click = ui
            .push_id(&name, |ui| {
                paint_tab(
                    ui,
                    &mut view,
                    entry,
                    &staged,
                    &catalog,
                    &BarOpts {
                        hint: &hint,
                        busy,
                        wait: &wait,
                    },
                )
            })
            .inner;

        self.mods_ui.views.insert(name.clone(), view);
        self.apply_mods_click(&name, click);
    }

    /// Snapshot helper: show Browse for `instance` without waiting for the first-open default.
    #[allow(dead_code)]
    pub(crate) fn mods_show_browse_for_tests(&mut self, instance: &str) {
        let has = self.instance_has_mods(instance);
        let view = self
            .mods_ui
            .views
            .entry(instance.to_owned())
            .or_insert_with(|| InstanceModsUi::fresh(has));
        view.segment = 1;
    }

    /// Snapshot helper: an instance with no packages, if the demo has one.
    #[allow(dead_code)]
    pub(crate) fn mods_empty_instance_for_tests(&self) -> Option<String> {
        self.backend
            .instances()
            .iter()
            .find(|entry| entry.card.packages.is_empty())
            .map(|entry| entry.card.name.clone())
    }

    /// Snapshot helper: set the search box. Does not change the segment.
    #[allow(dead_code)]
    pub(crate) fn mods_set_search_for_tests(&mut self, instance: &str, search: &str) {
        let has = self.instance_has_mods(instance);
        let view = self
            .mods_ui
            .views
            .entry(instance.to_owned())
            .or_insert_with(|| InstanceModsUi::fresh(has));
        view.search = search.to_owned();
    }

    fn instance_has_mods(&self, instance: &str) -> bool {
        self.backend
            .instances()
            .iter()
            .any(|entry| entry.card.name == instance && !entry.card.packages.is_empty())
    }

    fn apply_mods_click(&mut self, instance: &str, click: Click) {
        match click {
            Click::None => {}
            Click::Retry => self.backend.refresh(),
            Click::Discard => {
                self.staged_mods.remove(instance);
            }
            Click::Apply => self.apply_staged(instance),
            Click::Add(id) => self.with_staged(instance, |staged| stage_add(staged, &id)),
            Click::Remove(id) => self.with_staged(instance, |staged| stage_remove(staged, &id)),
            Click::UndoAdd(id) => self.with_staged(instance, |staged| {
                undo_add(staged, &id);
            }),
            Click::UndoRemove(id) => self.with_staged(instance, |staged| undo_remove(staged, &id)),
        }
    }

    fn apply_staged(&mut self, instance: &str) {
        let Some(staged) = self.staged_mods.get(instance).cloned() else {
            return;
        };
        if staged.is_empty() {
            return;
        }
        let started = self.start_op(
            instance,
            OpRequest::Mods {
                instance: instance.to_owned(),
                add: staged.add,
                remove: staged.remove,
                build: None,
            },
        );
        if started {
            self.staged_mods.remove(instance);
        }
    }

    fn with_staged(&mut self, instance: &str, change: impl FnOnce(&mut StagedMods)) {
        let mut staged = self.staged_mods.remove(instance).unwrap_or_default();
        change(&mut staged);
        if !staged.is_empty() {
            self.staged_mods.insert(instance.to_owned(), staged);
        }
    }
}

fn wait_reason(app: &App, instance: &str, external: bool) -> String {
    if let Some(kind) = app.op_kind_running(instance) {
        format!("Wait for the {} to finish", super::op_noun(kind))
    } else if external || app.busy(instance) {
        "Wait for the current operation to finish".to_owned()
    } else {
        String::new()
    }
}

fn last_build_ms(entry: &InstanceEntry) -> Option<u64> {
    entry
        .card
        .meta
        .generations
        .iter()
        .find_map(|generation| generation.build.as_ref().map(|build| build.duration_ms))
}

fn rebuild_hint(rebuild: bool, last_ms: Option<u64>) -> String {
    if !rebuild {
        return "The instance will need a build afterwards".to_owned();
    }
    match last_ms {
        Some(ms) => {
            let mins = ms / 60_000;
            format!("Applying rebuilds the game · last build {mins} min")
        }
        None => "Applying rebuilds the game".to_owned(),
    }
}

/// Pending-bar copy and whether Apply is blocked. Bundled so [`paint_tab`] stays small.
struct BarOpts<'a> {
    hint: &'a str,
    busy: bool,
    wait: &'a str,
}

fn paint_tab(
    ui: &mut Ui,
    view: &mut InstanceModsUi,
    entry: &InstanceEntry,
    staged: &StagedMods,
    catalog: &Catalog,
    bar: &BarOpts<'_>,
) -> Click {
    let full = ui.available_rect_before_wrap();
    let stacked = full.width() < BAR_STACK_BREAK;
    let bar_id = Id::unique(("mods-pending-h", entry.card.name.as_str()));
    let target = if staged.is_empty() {
        0.0
    } else if stacked {
        BAR_H_STACKED
    } else {
        BAR_H
    };
    let bar_h = ui
        .ctx()
        .animate_value_with_time(bar_id, target, theme::anim_secs(ui.ctx(), 0.15));
    let reserve = if bar_h > 0.5 { bar_h + BAR_GAP } else { 0.0 };

    let categories = collect_categories(&entry.card.packages, catalog);
    let mut installed = build_installed(entry, catalog, view, staged);
    let mut browse = build_browse(entry, catalog, view, staged);
    let installed_n = installed.top_count();
    let browse_n = match catalog {
        Catalog::Ready { .. } => Some(browse.rows.len()),
        Catalog::Pending | Catalog::Failed(_) => None,
    };
    let before = (
        view.search.clone(),
        view.category.clone(),
        view.show_libraries,
    );
    paint_toolbar(
        ui,
        view,
        &entry.card.name,
        &categories,
        installed_n,
        browse_n,
    );
    let extra = (12.0 - ui.spacing().item_spacing.y).max(0.0);
    if extra > 0.0 {
        ui.add_space(extra);
    }
    if view.search != before.0 || view.category != before.1 || view.show_libraries != before.2 {
        view.collapsed.clear();
        view.deps_collapsed = false;
        installed = build_installed(entry, catalog, view, staged);
        browse = build_browse(entry, catalog, view, staged);
    }

    let list_top = ui.next_widget_position().y;
    let list_h = (full.bottom() - reserve - list_top).max(0.0);
    let mut click = Click::None;
    if list_h > 1.0 {
        click = if view.segment == 1 {
            paint_browse(ui, list_h, &entry.card.name, view, catalog, &browse)
        } else {
            paint_installed(ui, list_h, &entry.card.name, view, staged, &installed)
        };
    }
    if bar_h > 0.5 {
        let bar_rect = Rect::from_min_max(pos2(full.left(), full.bottom() - bar_h), full.max);
        set_click(
            &mut click,
            paint_pending(ui, bar_rect, staged, bar.hint, bar.busy, bar.wait, stacked),
        );
    }
    click
}

fn paint_toolbar(
    ui: &mut Ui,
    view: &mut InstanceModsUi,
    instance: &str,
    categories: &[String],
    installed_n: usize,
    browse_n: Option<usize>,
) {
    let wide = ui.available_width() >= TOOLBAR_WRAP;
    ui.horizontal(|ui| {
        let mut segment = view.segment;
        widgets::segmented(
            ui,
            "mods-seg",
            &mut segment,
            &[("Installed", Some(installed_n)), ("Browse", browse_n)],
        );
        view.segment = segment;
        if wide {
            let search_w = (ui.available_width() - 312.0).max(80.0);
            widgets::search_input_with_id(
                ui,
                ("mods-search", instance),
                &mut view.search,
                "Search mods",
                search_w,
            );
            category_box(ui, view, categories);
            libraries_toggle(ui, &mut view.show_libraries);
        } else {
            let search_w = ui.available_width().max(40.0);
            widgets::search_input_with_id(
                ui,
                ("mods-search", instance),
                &mut view.search,
                "Search mods",
                search_w,
            );
        }
    });
    if !wide {
        ui.horizontal(|ui| {
            category_box(ui, view, categories);
            libraries_toggle(ui, &mut view.show_libraries);
        });
    }
}

fn category_box(ui: &mut Ui, view: &mut InstanceModsUi, categories: &[String]) {
    let p = theme::palette(ui.ctx());
    let label = view
        .category
        .clone()
        .unwrap_or_else(|| "All categories".to_owned());
    egui::ComboBox::from_id_salt("mod-category")
        .width(168.0)
        .truncate()
        .selected_text(
            egui::RichText::new(label)
                .font(theme::regular(13.0))
                .color(p.text),
        )
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut view.category, None, "All categories");
            for cat in categories {
                ui.selectable_value(&mut view.category, Some(cat.clone()), cat);
            }
        });
}

fn libraries_toggle(ui: &mut Ui, on: &mut bool) {
    widgets::switch(ui, on);
    let p = theme::palette(ui.ctx());
    let font = theme::regular(13.0);
    let width = ui
        .painter()
        .layout_no_wrap("Libraries".to_owned(), font.clone(), p.text)
        .size()
        .x;
    let (rect, mut response) = ui.allocate_exact_size(vec2(width, 22.0), Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Libraries"));
    let color = if response.hovered() {
        p.text_bright
    } else if *on {
        p.text
    } else {
        p.text_muted
    };
    let galley = ui
        .painter()
        .layout_no_wrap("Libraries".to_owned(), font, color);
    let y = rect.center().y - galley.size().y * 0.5;
    ui.painter().galley(pos2(rect.left(), y), galley, color);
    let _ = response.on_hover_cursor(CursorIcon::PointingHand);
}

fn paint_installed(
    ui: &mut Ui,
    list_h: f32,
    instance: &str,
    view: &mut InstanceModsUi,
    staged: &StagedMods,
    model: &InstalledModel,
) -> Click {
    let filter = Filter::from_view(view);
    if let Some(kind) = installed_empty(model, &filter) {
        let search = view.search.clone();
        return empty_in_rect(ui, list_h, kind, &search, view);
    }
    let mut click = Click::None;
    scroll_list(ui, list_h, (instance, "installed"), |ui| {
        let p = theme::palette(ui.ctx());
        if !staged.is_empty() {
            ui.add_space(2.0);
            indent_label(
                ui,
                "Preview after Apply",
                theme::regular(12.0),
                p.text_muted,
            );
            ui.add_space(4.0);
        }
        let mut started = false;
        if !model.adding.is_empty() {
            section_label(
                ui,
                &mut started,
                &format!("Adding ({})", model.adding.len()),
                p.volt,
            );
            for row in &model.adding {
                let hit = paint_add_row(ui, row, &p);
                set_click(&mut click, hit_click(hit, &row.id, true));
            }
        }
        if !model.unresolved.is_empty() {
            section_label(
                ui,
                &mut started,
                &format!("Unresolved ({})", model.unresolved.len()),
                p.bad,
            );
            for row in &model.unresolved {
                let hit = paint_plain_row(ui, row, true, &p);
                set_click(&mut click, hit_click(hit, &row.id, false));
            }
        }
        for row in &model.orphans {
            let hit = paint_plain_row(ui, row, false, &p);
            set_click(&mut click, hit_click(hit, &row.id, false));
        }
        if !model.direct.is_empty() {
            section_label(
                ui,
                &mut started,
                &format!("Mods ({})", model.direct.len()),
                p.text_muted,
            );
            for row in &model.direct {
                paint_direct(ui, instance, view, row, &p, &mut click);
            }
        }
        if model.deps.count > 0 {
            if started {
                ui.add_space(12.0);
            }
            if deps_header(ui, model.deps.open, model.deps.count, &p) {
                if model.deps.forced {
                    view.deps_collapsed = !view.deps_collapsed;
                } else {
                    view.deps_expanded = !view.deps_expanded;
                }
            }
            with_reveal(ui, reveal_id(instance, "deps"), model.deps.open, |ui| {
                for row in &model.deps.rows {
                    let hit = paint_member_row(ui, &row.pkg, row.removing, "", 0.0, &p);
                    set_click(&mut click, hit_click(hit, &row.pkg.id, false));
                }
            });
        }
        if model.hidden_libraries > 0 {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                // Ghost small pads 10px; shift so the label lines up with the names.
                ui.add_space((NAME_X - 10.0).max(0.0));
                let label = hidden_label(model.hidden_libraries);
                if widgets::button(ui, &label, Variant::Ghost, Size::Small).clicked() {
                    view.show_libraries = true;
                }
            });
        }
        ui.add_space(8.0);
    });
    click
}

fn paint_direct(
    ui: &mut Ui,
    instance: &str,
    view: &mut InstanceModsUi,
    row: &DirectRow,
    p: &Palette,
    click: &mut Click,
) {
    let hit = paint_direct_row(ui, row, p);
    if matches!(hit, RowHit::Body | RowHit::Members) {
        if row.forced {
            toggle_expand(&mut view.collapsed, &row.pkg.id);
        } else {
            toggle_expand(&mut view.expanded, &row.pkg.id);
        }
    }
    set_click(click, hit_click(hit, &row.pkg.id, false));
    if row.pkg.kind == "bundle" && row.member_total > 0 {
        let bundle_name = row.pkg.name.clone();
        with_reveal(ui, reveal_id(instance, &row.pkg.id), row.open, |ui| {
            for member in &row.members {
                let hit = paint_member_row(
                    ui,
                    &member.pkg,
                    member.removing,
                    &bundle_name,
                    MEMBER_INDENT,
                    p,
                );
                set_click(click, hit_click(hit, &member.pkg.id, false));
            }
        });
    }
}

fn paint_browse(
    ui: &mut Ui,
    list_h: f32,
    instance: &str,
    view: &mut InstanceModsUi,
    catalog: &Catalog,
    model: &BrowseModel,
) -> Click {
    let filter = Filter::from_view(view);
    match catalog {
        Catalog::Pending => {
            scroll_list(ui, list_h, (instance, "browse"), paint_skeletons);
            Click::None
        }
        Catalog::Failed(message) => {
            let rect = list_rect(ui, list_h);
            let _ = ui.allocate_rect(rect, Sense::hover());
            if paint_error(ui, rect, message) {
                Click::Retry
            } else {
                Click::None
            }
        }
        Catalog::Ready { warnings, .. } => {
            if model.rows.is_empty() {
                let kind = if filter.active() {
                    EmptyKind::Filtered
                } else if model.non_library == 0 && model.hidden_libraries > 0 {
                    EmptyKind::OnlyCatalogueLibraries
                } else {
                    EmptyKind::NoCatalogue
                };
                let _ = warnings;
                let search = view.search.clone();
                empty_in_rect(ui, list_h, kind, &search, view)
            } else {
                let mut click = Click::None;
                let warnings = warnings.clone();
                scroll_list(ui, list_h, (instance, "browse"), |ui| {
                    let p = theme::palette(ui.ctx());
                    paint_warnings(ui, &warnings, &p);
                    for row in &model.rows {
                        let hit = paint_browse_row(ui, row, &p);
                        let undo_add = matches!(row.action, BrowseAction::Added);
                        set_click(&mut click, hit_click(hit, &row.id, undo_add));
                    }
                    ui.add_space(8.0);
                });
                click
            }
        }
    }
}

fn installed_empty(model: &InstalledModel, filter: &Filter) -> Option<EmptyKind> {
    if model.has_body() {
        None
    } else if filter.active() {
        Some(EmptyKind::Filtered)
    } else if model.hidden_libraries > 0 {
        Some(EmptyKind::OnlyInstalledLibraries)
    } else {
        Some(EmptyKind::NoMods)
    }
}

fn empty_in_rect(
    ui: &mut Ui,
    list_h: f32,
    kind: EmptyKind,
    search: &str,
    view: &mut InstanceModsUi,
) -> Click {
    let rect = list_rect(ui, list_h);
    let _ = ui.allocate_rect(rect, Sense::hover());
    let action = paint_empty(ui, rect, kind, search);
    apply_empty(view, action)
}

fn list_rect(ui: &Ui, list_h: f32) -> Rect {
    Rect::from_min_size(
        ui.next_widget_position(),
        vec2(ui.available_width(), list_h),
    )
}

fn scroll_list(
    ui: &mut Ui,
    list_h: f32,
    salt: impl std::hash::Hash + std::fmt::Debug,
    body: impl FnOnce(&mut Ui),
) {
    egui::ScrollArea::vertical()
        .id_salt(salt)
        .max_height(list_h)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let width = ui.available_width();
            ui.set_width(width);
            body(ui);
        });
}

fn section_label(ui: &mut Ui, started: &mut bool, text: &str, color: Color32) {
    let gap = if *started { 12.0 } else { 2.0 };
    *started = true;
    ui.add_space(gap);
    indent_label(ui, text, theme::semibold(12.0), color);
    ui.add_space(4.0);
}

fn indent_label(ui: &mut Ui, text: &str, font: FontId, color: Color32) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.add_space(NAME_X);
        ui.label(egui::RichText::new(text).font(font).color(color));
    });
}

fn deps_header(ui: &mut Ui, open: bool, count: usize, p: &Palette) -> bool {
    let icon = if open {
        Icon::ChevronDown
    } else {
        Icon::ChevronRight
    };
    let label = format!("Dependencies ({count})");
    let hit = ui
        .push_id("deps-header", |ui| {
            draw_row(
                ui,
                RowSpec {
                    id: "deps",
                    height: 36.0,
                    indent: 0.0,
                    clickable: true,
                    tint: None,
                    icon,
                    icon_color: p.text_muted,
                    strike: false,
                    hover: "",
                    lines: Lines::One {
                        name: &label,
                        version: "",
                        desc: "",
                        name_color: p.text_muted,
                        name_size: 12.0,
                    },
                    trail: &[],
                },
            )
        })
        .inner;
    matches!(hit, RowHit::Body)
}

fn build_installed(
    entry: &InstanceEntry,
    catalog: &Catalog,
    view: &InstanceModsUi,
    staged: &StagedMods,
) -> InstalledModel {
    let filter = Filter::from_view(view);
    let packages = &entry.card.packages;
    let unresolved_ids = unresolved_ids(entry);
    let (directs, members, deps) = group_children(packages);
    let direct = directs
        .into_iter()
        .filter_map(|pkg| {
            let kids = members.get(&pkg.id).map(Vec::as_slice).unwrap_or(&[]);
            plan_direct(pkg, kids, view, &filter, staged)
        })
        .collect();
    InstalledModel {
        adding: adding_rows(packages, &unresolved_ids, catalog.mods(), &filter, staged),
        unresolved: unresolved_rows(&unresolved_ids, catalog.mods(), &filter, staged),
        orphans: orphan_rows(packages, &unresolved_ids, &filter, staged),
        direct,
        deps: plan_deps(&deps, view, &filter, staged),
        hidden_libraries: hidden_library_count(packages, staged, filter.show_libraries),
    }
}

fn group_children(
    packages: &[PackageCard],
) -> (
    Vec<PackageCard>,
    HashMap<String, Vec<PackageCard>>,
    Vec<PackageCard>,
) {
    let mut direct = Vec::new();
    let mut members: HashMap<String, Vec<PackageCard>> = HashMap::new();
    let mut deps = Vec::new();
    for pkg in packages {
        if pkg.direct {
            direct.push(pkg.clone());
            continue;
        }
        if let Some(parent) = bundle_parent_id(pkg, packages) {
            members.entry(parent).or_default().push(pkg.clone());
        } else {
            deps.push(pkg.clone());
        }
    }
    sort_bundles_first(&mut direct);
    for group in members.values_mut() {
        sort_by_name(group);
    }
    sort_by_name(&mut deps);
    (direct, members, deps)
}

fn bundle_parent_id(pkg: &PackageCard, packages: &[PackageCard]) -> Option<String> {
    if let Some(id) = pkg.in_bundles.iter().find(|id| {
        packages
            .iter()
            .any(|parent| &parent.id == *id && parent.direct && parent.kind == "bundle")
    }) {
        return Some(id.clone());
    }
    packages
        .iter()
        .find(|parent| {
            parent.direct
                && parent.kind == "bundle"
                && parent.dependencies.iter().any(|dep| dep == &pkg.id)
        })
        .map(|parent| parent.id.clone())
}

fn plan_direct(
    pkg: PackageCard,
    kids: &[PackageCard],
    view: &InstanceModsUi,
    filter: &Filter,
    staged: &StagedMods,
) -> Option<DirectRow> {
    let removing = is_removing(staged, &pkg.id);
    let mut kid_removing = false;
    let mut kid_hit = false;
    let mut visible_kids = 0;
    for kid in kids {
        if !member_visible(kid, filter, staged) {
            continue;
        }
        visible_kids += 1;
        if is_removing(staged, &kid.id) {
            kid_removing = true;
        }
        if filter.matches(&kid.name, &kid.id, &kid.description, &kid.categories) {
            kid_hit = true;
        }
    }
    let self_hit = filter.matches(&pkg.name, &pkg.id, &pkg.description, &pkg.categories);
    let show = if removing || kid_removing {
        true
    } else if !show_kind(&pkg.kind, filter, false) {
        false
    } else if filter.active() {
        self_hit || kid_hit
    } else {
        true
    };
    if !show {
        return None;
    }
    let user_open = view.expanded.contains(&pkg.id);
    let forced = kid_removing || (filter.active() && kid_hit);
    let open = if forced {
        !view.collapsed.contains(&pkg.id)
    } else {
        user_open
    };
    let members = shown_members(kids, open, user_open, filter, staged);
    Some(DirectRow {
        pkg,
        open,
        forced,
        members,
        member_total: visible_kids,
        removing,
    })
}

fn shown_members(
    kids: &[PackageCard],
    open: bool,
    user_open: bool,
    filter: &Filter,
    staged: &StagedMods,
) -> Vec<MemberRow> {
    if !open {
        return Vec::new();
    }
    kids.iter()
        .filter(|kid| {
            if !member_visible(kid, filter, staged) {
                return false;
            }
            if filter.active() && !user_open {
                return is_removing(staged, &kid.id)
                    || filter.matches(&kid.name, &kid.id, &kid.description, &kid.categories);
            }
            true
        })
        .map(|kid| MemberRow {
            removing: is_removing(staged, &kid.id),
            pkg: kid.clone(),
        })
        .collect()
}

fn plan_deps(
    deps: &[PackageCard],
    view: &InstanceModsUi,
    filter: &Filter,
    staged: &StagedMods,
) -> DepGroup {
    let mut listed = Vec::new();
    let mut any_hit = false;
    let mut any_removing = false;
    for pkg in deps {
        let removing = is_removing(staged, &pkg.id);
        if !show_kind(&pkg.kind, filter, removing) {
            continue;
        }
        let hit = filter.matches(&pkg.name, &pkg.id, &pkg.description, &pkg.categories);
        if filter.active() && !hit && !removing {
            continue;
        }
        any_hit |= filter.active() && hit;
        any_removing |= removing;
        listed.push(MemberRow {
            pkg: pkg.clone(),
            removing,
        });
    }
    let forced = any_removing || any_hit;
    let open = if forced {
        !view.deps_collapsed
    } else {
        view.deps_expanded
    };
    let count = listed.len();
    if !open {
        listed.clear();
    }
    DepGroup {
        count,
        open,
        forced,
        rows: listed,
    }
}

fn adding_rows(
    packages: &[PackageCard],
    unresolved: &[String],
    catalog: &[AvailableMod],
    filter: &Filter,
    staged: &StagedMods,
) -> Vec<AddRow> {
    let mut rows = Vec::new();
    for spec in &staged.add {
        let id = spec_id(spec);
        if packages.iter().any(|pkg| pkg.id == id) || unresolved.iter().any(|item| item == id) {
            continue;
        }
        let found = find_available(catalog, id);
        let name = found.map(|m| m.name.as_str()).unwrap_or(id);
        let version = found.map(|m| m.version.as_str()).unwrap_or("");
        let desc = found
            .map(|m| m.description.as_str())
            .unwrap_or("Not in the catalogue");
        let kind = found.map(|m| m.kind.as_str()).unwrap_or("mod");
        let cats: &[String] = found.map(|m| m.categories.as_slice()).unwrap_or(&[]);
        if !filter.search(name, id, desc) || !filter.category_ok(cats) {
            continue;
        }
        rows.push(AddRow {
            id: id.to_owned(),
            name: name.to_owned(),
            version: version.to_owned(),
            description: desc.to_owned(),
            kind: kind.to_owned(),
        });
    }
    rows.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.id.cmp(&b.id))
    });
    rows
}

fn unresolved_rows(
    ids: &[String],
    catalog: &[AvailableMod],
    filter: &Filter,
    staged: &StagedMods,
) -> Vec<PlainRow> {
    let desc = "Requested, but not in the lock.";
    let mut rows = Vec::new();
    for id in ids {
        let removing = is_removing(staged, id);
        let found = find_available(catalog, id);
        let name = found.map(|m| m.name.as_str()).unwrap_or(id.as_str());
        let cats: &[String] = found.map(|m| m.categories.as_slice()).unwrap_or(&[]);
        if !removing && (!filter.search(name, id, desc) || !filter.category_ok(cats)) {
            continue;
        }
        rows.push(PlainRow {
            id: id.clone(),
            name: name.to_owned(),
            description: desc.to_owned(),
            removing,
        });
    }
    rows.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.id.cmp(&b.id))
    });
    rows
}

fn orphan_rows(
    packages: &[PackageCard],
    unresolved: &[String],
    filter: &Filter,
    staged: &StagedMods,
) -> Vec<PlainRow> {
    let mut rows = Vec::new();
    for id in &staged.remove {
        if packages.iter().any(|pkg| &pkg.id == id) || unresolved.iter().any(|item| item == id) {
            continue;
        }
        if filter.category.is_some() || !filter.search(id, id, "") {
            continue;
        }
        rows.push(PlainRow {
            id: id.clone(),
            name: id.clone(),
            description: String::new(),
            removing: true,
        });
    }
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    rows
}

fn unresolved_ids(entry: &InstanceEntry) -> Vec<String> {
    let have: HashSet<&str> = entry
        .card
        .packages
        .iter()
        .map(|pkg| pkg.id.as_str())
        .collect();
    entry
        .card
        .manifest
        .mods
        .keys()
        .filter(|id| !have.contains(id.as_str()))
        .map(|id| id.as_str().to_owned())
        .collect()
}

fn build_browse(
    entry: &InstanceEntry,
    catalog: &Catalog,
    view: &InstanceModsUi,
    staged: &StagedMods,
) -> BrowseModel {
    let Catalog::Ready { mods, .. } = catalog else {
        return BrowseModel {
            rows: Vec::new(),
            hidden_libraries: 0,
            non_library: 0,
        };
    };
    let filter = Filter::from_view(view);
    let mut hidden_libraries = 0;
    let mut non_library = 0;
    let mut rows = Vec::new();
    for available in mods {
        if available.kind == "library" && !filter.show_libraries {
            hidden_libraries += 1;
            continue;
        }
        if available.kind != "library" {
            non_library += 1;
        }
        if !filter.matches(
            &available.name,
            &available.id,
            &available.description,
            &available.categories,
        ) {
            continue;
        }
        let installed = entry
            .card
            .packages
            .iter()
            .find(|pkg| pkg.id == available.id);
        let (action, required_by) = browse_action(
            installed,
            is_removing(staged, &available.id),
            is_adding(staged, &available.id),
            &entry.card.packages,
        );
        rows.push(BrowseRow {
            id: available.id.clone(),
            name: available.name.clone(),
            version: available.version.clone(),
            sub: subtitle(&available.description, &available.authors),
            kind: available.kind.clone(),
            categories: available.categories.clone(),
            members: member_names(&available.members, mods),
            member_count: available.members.len(),
            action,
            required_by,
        });
    }
    rows.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.id.cmp(&b.id))
    });
    BrowseModel {
        rows,
        hidden_libraries,
        non_library,
    }
}

fn browse_action(
    installed: Option<&PackageCard>,
    removing: bool,
    adding: bool,
    packages: &[PackageCard],
) -> (BrowseAction, String) {
    if removing {
        return (BrowseAction::Removing, String::new());
    }
    if let Some(pkg) = installed {
        if pkg.direct {
            (BrowseAction::Installed, String::new())
        } else {
            (BrowseAction::Indirect, required_by(pkg, packages))
        }
    } else if adding {
        (BrowseAction::Added, String::new())
    } else {
        (BrowseAction::Add, String::new())
    }
}

fn required_by(pkg: &PackageCard, packages: &[PackageCard]) -> String {
    if let Some(id) = pkg.in_bundles.first() {
        return packages
            .iter()
            .find(|parent| &parent.id == id)
            .map(|parent| parent.name.clone())
            .unwrap_or_else(|| id.clone());
    }
    packages
        .iter()
        .find(|parent| parent.dependencies.iter().any(|dep| dep == &pkg.id))
        .map(|parent| parent.name.clone())
        .unwrap_or_else(|| "another mod".to_owned())
}

fn member_names(ids: &[String], mods: &[AvailableMod]) -> Vec<String> {
    ids.iter()
        .map(|id| {
            find_available(mods, id)
                .map(|m| m.name.clone())
                .unwrap_or_else(|| id.clone())
        })
        .collect()
}

fn paint_add_row(ui: &mut Ui, row: &AddRow, p: &Palette) -> RowHit {
    let (icon, color) = kind_mark(&row.kind, p);
    let trail = vec![will_add_pill(p), Trail::Undo];
    draw_row(
        ui,
        RowSpec {
            id: &row.id,
            height: ROW_H,
            indent: 0.0,
            clickable: false,
            tint: Some(theme::with_alpha(p.volt, TINT)),
            icon,
            icon_color: color,
            strike: false,
            hover: "",
            lines: Lines::One {
                name: &row.name,
                version: &row.version,
                desc: &row.description,
                name_color: p.text_bright,
                name_size: 14.0,
            },
            trail: &trail,
        },
    )
}

fn paint_plain_row(ui: &mut Ui, row: &PlainRow, unresolved: bool, p: &Palette) -> RowHit {
    let (icon, color) = if unresolved {
        (Icon::HollowCube, p.bad)
    } else {
        (Icon::Cube, p.text_muted)
    };
    let mut trail = Vec::new();
    if row.removing {
        trail.push(will_remove_pill(p));
        trail.push(Trail::Undo);
    } else {
        trail.push(Trail::Pill {
            text: "Unresolved".to_owned(),
            fg: p.bad,
            bg: theme::with_alpha(p.bad, 38),
        });
        trail.push(Trail::Remove);
    }
    let tint = row.removing.then(|| theme::with_alpha(p.bad, TINT));
    draw_row(
        ui,
        RowSpec {
            id: &row.id,
            height: ROW_H,
            indent: 0.0,
            clickable: false,
            tint,
            icon,
            icon_color: color,
            strike: row.removing,
            hover: "",
            lines: Lines::One {
                name: &row.name,
                version: "",
                desc: &row.description,
                name_color: if row.removing {
                    p.text_muted
                } else {
                    p.text_bright
                },
                name_size: 14.0,
            },
            trail: &trail,
        },
    )
}

fn paint_direct_row(ui: &mut Ui, row: &DirectRow, p: &Palette) -> RowHit {
    let (icon, color) = kind_mark(&row.pkg.kind, p);
    let trail = direct_trail(row, p);
    let clickable = row.pkg.kind == "bundle" && row.member_total > 0;
    let tint = row.removing.then(|| theme::with_alpha(p.bad, TINT));
    draw_row(
        ui,
        RowSpec {
            id: &row.pkg.id,
            height: ROW_H,
            indent: 0.0,
            clickable,
            tint,
            icon,
            icon_color: color,
            strike: row.removing,
            hover: "",
            lines: Lines::One {
                name: &row.pkg.name,
                version: &row.pkg.version,
                desc: &row.pkg.description,
                name_color: if row.removing {
                    p.text_muted
                } else {
                    p.text_bright
                },
                name_size: 14.0,
            },
            trail: &trail,
        },
    )
}

fn direct_trail(row: &DirectRow, p: &Palette) -> Vec<Trail> {
    let mut trail = Vec::new();
    if row.pkg.kind == "bundle" && row.member_total > 0 {
        trail.push(Trail::Members {
            label: members_label(row.member_total, true),
            open: row.open,
            tip: String::new(),
            toggle: true,
        });
    }
    if row.pkg.kind == "library" {
        trail.push(Trail::Pill {
            text: "library".to_owned(),
            fg: p.text_muted,
            bg: p.bg_raised,
        });
    }
    if row.removing {
        trail.push(will_remove_pill(p));
        trail.push(Trail::Undo);
    } else {
        trail.push(Trail::Remove);
    }
    trail
}

fn paint_member_row(
    ui: &mut Ui,
    pkg: &PackageCard,
    removing: bool,
    bundle: &str,
    indent: f32,
    p: &Palette,
) -> RowHit {
    let (icon, color) = kind_mark(&pkg.kind, p);
    let mut trail = Vec::new();
    if removing {
        trail.push(will_remove_pill(p));
        trail.push(Trail::Undo);
    }
    let hover = if bundle.is_empty() {
        String::new()
    } else {
        format!("Included by {bundle}; remove the bundle to remove it")
    };
    let tint = removing.then(|| theme::with_alpha(p.bad, TINT));
    draw_row(
        ui,
        RowSpec {
            id: &pkg.id,
            height: ROW_H,
            indent,
            clickable: false,
            tint,
            icon,
            icon_color: color,
            strike: removing,
            hover: &hover,
            lines: Lines::One {
                name: &pkg.name,
                version: &pkg.version,
                desc: &pkg.description,
                name_color: if removing {
                    p.text_muted
                } else {
                    p.text_bright
                },
                name_size: 14.0,
            },
            trail: &trail,
        },
    )
}

fn paint_browse_row(ui: &mut Ui, row: &BrowseRow, p: &Palette) -> RowHit {
    let (icon, color) = kind_mark(&row.kind, p);
    let trail = browse_trail(row, p);
    let removing = matches!(row.action, BrowseAction::Removing);
    let adding = matches!(row.action, BrowseAction::Added);
    let tint = if removing {
        Some(theme::with_alpha(p.bad, TINT))
    } else if adding {
        Some(theme::with_alpha(p.volt, TINT))
    } else {
        None
    };
    draw_row(
        ui,
        RowSpec {
            id: &row.id,
            height: BROWSE_H,
            indent: 0.0,
            clickable: false,
            tint,
            icon,
            icon_color: color,
            strike: removing,
            hover: "",
            lines: Lines::Two {
                name: &row.name,
                version: &row.version,
                sub: &row.sub,
            },
            trail: &trail,
        },
    )
}

fn browse_trail(row: &BrowseRow, p: &Palette) -> Vec<Trail> {
    let mut trail = Vec::new();
    if row.member_count > 0 {
        trail.push(Trail::Members {
            label: members_label(row.member_count, false),
            open: false,
            tip: row.members.join("\n"),
            toggle: false,
        });
    }
    for cat in shown_categories(&row.categories) {
        trail.push(Trail::Pill {
            text: cat,
            fg: p.text,
            bg: p.bg_raised,
        });
    }
    match row.action {
        BrowseAction::Add => trail.push(Trail::Add),
        BrowseAction::Installed => {
            trail.push(Trail::Pill {
                text: "Installed".to_owned(),
                fg: p.ok,
                bg: theme::with_alpha(p.ok, 38),
            });
            trail.push(Trail::Remove);
        }
        BrowseAction::Indirect => {
            trail.push(Trail::Note(format!("Required by {}", row.required_by)));
        }
        BrowseAction::Added => {
            trail.push(Trail::Added);
            trail.push(Trail::Undo);
        }
        BrowseAction::Removing => {
            trail.push(will_remove_pill(p));
            trail.push(Trail::Undo);
        }
    }
    trail
}

enum Lines<'a> {
    One {
        name: &'a str,
        version: &'a str,
        desc: &'a str,
        name_color: Color32,
        name_size: f32,
    },
    Two {
        name: &'a str,
        version: &'a str,
        sub: &'a str,
    },
}

/// Arguments for [`draw_row`]. One struct keeps the painter under clippy's argument cap.
struct RowSpec<'a> {
    id: &'a str,
    height: f32,
    indent: f32,
    clickable: bool,
    tint: Option<Color32>,
    icon: Icon,
    icon_color: Color32,
    strike: bool,
    hover: &'a str,
    lines: Lines<'a>,
    trail: &'a [Trail],
}

/// List row. Hover and press match [`widgets::row`]; the tint sits under the wash.
///
/// The kit's ghost icon button stays muted on hover, so the remove control is painted here.
/// Rows that do nothing (no expand) use hover sense only, so they do not show a dead hand.
fn draw_row(ui: &mut Ui, spec: RowSpec<'_>) -> RowHit {
    ui.push_id(spec.id, |ui| {
        let width = ui.available_width();
        let sense = if spec.clickable {
            Sense::click()
        } else {
            Sense::hover()
        };
        let (rect, mut response) = ui.allocate_exact_size(vec2(width, spec.height), sense);
        paint_row_fill(ui, rect, &response, spec.tint, spec.clickable);
        let p = theme::palette(ui.ctx());
        let x0 = rect.left() + spec.indent;
        ui.painter().hline(
            x0..=rect.right(),
            rect.bottom() - 0.5,
            Stroke::new(1.0, p.line_soft),
        );
        let icon_rect = Rect::from_center_size(
            pos2(
                rect.left() + PAD_X + spec.indent + ICON_PX * 0.5,
                rect.center().y,
            ),
            Vec2::splat(ICON_PX),
        );
        if spec.icon_color != Color32::TRANSPARENT {
            widgets::paint_icon(ui.painter(), icon_rect, spec.icon, spec.icon_color, 0.0);
        }
        let (trail_left, mut hit) = place_trail(ui, rect, spec.trail);
        let text_left = if spec.icon_color == Color32::TRANSPARENT {
            rect.left() + PAD_X + spec.indent
        } else {
            rect.left() + NAME_X + spec.indent
        };
        let text_right = if spec.trail.is_empty() {
            rect.right() - PAD_X
        } else {
            trail_left - 12.0
        };
        if text_right > text_left + 8.0 {
            let text_rect =
                Rect::from_min_max(pos2(text_left, rect.top()), pos2(text_right, rect.bottom()));
            paint_lines(ui, text_rect, &spec.lines, spec.strike);
        }
        if spec.clickable && hit == RowHit::None && response.clicked() {
            hit = RowHit::Body;
            response.mark_changed();
        }
        if spec.clickable {
            response.widget_info(|| {
                let name = match &spec.lines {
                    Lines::One { name, .. } | Lines::Two { name, .. } => *name,
                };
                egui::WidgetInfo::labeled(egui::Role::Button, true, name)
            });
        }
        if !spec.hover.is_empty() {
            response.clone().on_hover_text(spec.hover);
        }
        if spec.clickable {
            let _ = response.on_hover_cursor(CursorIcon::PointingHand);
        }
        hit
    })
    .inner
}

fn paint_row_fill(
    ui: &Ui,
    rect: Rect,
    response: &egui::Response,
    tint: Option<Color32>,
    clickable: bool,
) {
    let p = theme::palette(ui.ctx());
    if let Some(tint) = tint {
        ui.painter().rect_filled(rect, 8.0, tint);
    }
    let hovered = response.hovered() && response.enabled();
    let pressed = clickable && response.is_pointer_button_down_on() && response.enabled();
    let hover_t = ui.ctx().animate_bool_with_time(
        response.id.with("hover"),
        hovered,
        theme::anim_secs(ui.ctx(), 0.12),
    );
    let press_t = ui.ctx().animate_bool_with_time(
        response.id.with("press"),
        pressed,
        theme::anim_secs(ui.ctx(), 0.12),
    );
    if hover_t > 0.0 {
        ui.painter()
            .rect_filled(rect, 8.0, p.wash_hover.gamma_multiply(hover_t));
    }
    if press_t > 0.0 {
        ui.painter()
            .rect_filled(rect, 8.0, p.wash_pressed.gamma_multiply(press_t));
    }
    if clickable && response.has_focus() {
        ui.painter().rect_stroke(
            rect.shrink(1.0),
            8.0,
            Stroke::new(1.5, p.arc),
            egui::StrokeKind::Inside,
        );
    }
}

fn paint_lines(ui: &Ui, rect: Rect, lines: &Lines<'_>, strike: bool) {
    let p = theme::palette(ui.ctx());
    match lines {
        Lines::One {
            name,
            version,
            desc,
            name_color,
            name_size,
        } => {
            let galley = one_line(
                ui,
                rect.width(),
                &LineParts {
                    name,
                    color: *name_color,
                    size: *name_size,
                    version,
                    desc,
                    strike,
                    strong: true,
                },
            );
            let y = rect.center().y - galley.size().y * 0.5;
            ui.painter().galley(pos2(rect.left(), y), galley, p.text);
        }
        Lines::Two { name, version, sub } => {
            let name_g = one_line(
                ui,
                rect.width(),
                &LineParts {
                    name,
                    color: p.text_bright,
                    size: 14.0,
                    version,
                    desc: "",
                    strike,
                    strong: true,
                },
            );
            let sub_g = one_line(
                ui,
                rect.width(),
                &LineParts {
                    name: sub,
                    color: p.text_muted,
                    size: 12.0,
                    version: "",
                    desc: "",
                    strike: false,
                    strong: false,
                },
            );
            let gap = 2.0;
            let block = name_g.size().y + gap + sub_g.size().y;
            let mut y = rect.center().y - block * 0.5;
            let name_h = name_g.size().y;
            ui.painter().galley(pos2(rect.left(), y), name_g, p.text);
            y += name_h + gap;
            ui.painter()
                .galley(pos2(rect.left(), y), sub_g, p.text_muted);
        }
    }
}

/// One ellipsised line. `strong` picks semibold (names) or regular (the browse subtitle).
struct LineParts<'a> {
    name: &'a str,
    color: Color32,
    size: f32,
    version: &'a str,
    desc: &'a str,
    strike: bool,
    strong: bool,
}

fn one_line(ui: &Ui, width: f32, parts: &LineParts<'_>) -> std::sync::Arc<egui::Galley> {
    let p = theme::palette(ui.ctx());
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = width.max(0.0);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    let name_color = if parts.strike {
        p.text_muted
    } else {
        parts.color
    };
    let font = if parts.strong {
        theme::semibold(parts.size)
    } else {
        theme::regular(parts.size)
    };
    job.append(parts.name, 0.0, text_format(font, name_color, parts.strike));
    if !parts.version.is_empty() {
        let version = parts.version;
        job.append(
            &format!("  {version}"),
            0.0,
            text_format(theme::regular(12.0), p.text_muted, parts.strike),
        );
    }
    if !parts.desc.is_empty() {
        let desc = parts.desc;
        job.append(
            &format!("   {desc}"),
            0.0,
            text_format(theme::regular(12.0), p.text_muted, parts.strike),
        );
    }
    ui.painter().layout_job(job)
}

fn text_format(font: FontId, color: Color32, strike: bool) -> TextFormat {
    TextFormat {
        font_id: font,
        color,
        strikethrough: if strike {
            Stroke::new(1.0, color)
        } else {
            Stroke::NONE
        },
        valign: Align::BOTTOM,
        ..Default::default()
    }
}

fn place_trail(ui: &mut Ui, rect: Rect, items: &[Trail]) -> (f32, RowHit) {
    if items.is_empty() {
        return (rect.right(), RowHit::None);
    }
    let mut trail = ui.new_child(
        UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::right_to_left(Align::Center))
            .id_salt("trail"),
    );
    trail.add_space(PAD_X);
    let mut left = rect.right();
    let mut hit = RowHit::None;
    for item in items.iter().rev() {
        let (response, item_hit) = place_trail_item(&mut trail, item);
        left = left.min(response.rect.left());
        if hit == RowHit::None && item_hit != RowHit::None {
            hit = item_hit;
        }
    }
    (left, hit)
}

fn place_trail_item(ui: &mut Ui, item: &Trail) -> (egui::Response, RowHit) {
    match item {
        Trail::Members {
            label,
            open,
            tip,
            toggle,
        } => members_control(ui, label, *open, *toggle, tip),
        Trail::Pill { text, fg, bg } => (widgets::pill(ui, text, *fg, *bg), RowHit::None),
        Trail::Added => (added_pill(ui), RowHit::None),
        Trail::Note(text) => (note_label(ui, text), RowHit::None),
        Trail::Remove => {
            let response = remove_button(ui);
            let hit = if response.clicked() {
                RowHit::Remove
            } else {
                RowHit::None
            };
            (response, hit)
        }
        Trail::Undo => {
            let response = widgets::Button::new("Undo")
                .variant(Variant::Ghost)
                .size(Size::Small)
                .show(ui);
            let hit = if response.clicked() {
                RowHit::Undo
            } else {
                RowHit::None
            };
            (response, hit)
        }
        Trail::Add => {
            let response = widgets::Button::new("+ Add")
                .variant(Variant::Secondary)
                .size(Size::Small)
                .min_width(76.0)
                .show(ui);
            let hit = if response.clicked() {
                RowHit::Add
            } else {
                RowHit::None
            };
            (response, hit)
        }
    }
}

/// 28px remove. Muted at rest, bad on hover. The kit's ghost button cannot do that.
fn remove_button(ui: &mut Ui) -> egui::Response {
    let (rect, mut response) = ui.allocate_exact_size(Vec2::splat(28.0), Sense::click());
    let p = theme::palette(ui.ctx());
    let hovered = response.hovered() && response.enabled();
    let pressed = response.is_pointer_button_down_on() && response.enabled();
    let hover_t = ui.ctx().animate_bool_with_time(
        response.id.with("hover"),
        hovered,
        theme::anim_secs(ui.ctx(), 0.12),
    );
    let press_t = ui.ctx().animate_bool_with_time(
        response.id.with("press"),
        pressed,
        theme::anim_secs(ui.ctx(), 0.12),
    );
    let alpha = (38.0 * hover_t + 26.0 * press_t).round().clamp(0.0, 255.0) as u8;
    if alpha > 0 {
        ui.painter()
            .rect_filled(rect, 6.0, theme::with_alpha(p.bad, alpha));
    }
    let fg = p.text_muted.lerp_to_gamma(p.bad, hover_t.max(press_t));
    let icon = Rect::from_center_size(rect.center(), Vec2::splat(12.0));
    widgets::paint_icon(ui.painter(), icon, Icon::Cross, fg, 0.0);
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect.expand(2.0),
            8.0,
            Stroke::new(2.0, p.arc),
            egui::StrokeKind::Inside,
        );
    }
    if response.clicked() {
        response.mark_changed();
    }
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::Role::Button, true, "Remove from this instance")
    });
    response = response.on_hover_text("Remove from this instance");
    response.on_hover_cursor(CursorIcon::PointingHand)
}

fn members_control(
    ui: &mut Ui,
    text: &str,
    open: bool,
    toggle: bool,
    tip: &str,
) -> (egui::Response, RowHit) {
    let p = theme::palette(ui.ctx());
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), theme::regular(12.0), p.volt);
    let chevron = if toggle { 14.0 } else { 0.0 };
    let width = galley.size().x + 16.0 + chevron;
    let sense = if toggle {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, mut response) = ui.allocate_exact_size(vec2(width, 20.0), sense);
    ui.painter().rect_filled(rect, 255.0, p.volt_tint);
    let y = rect.center().y - galley.size().y * 0.5;
    ui.painter()
        .galley(pos2(rect.left() + 8.0, y), galley, p.volt);
    if toggle {
        let icon = if open {
            Icon::ChevronDown
        } else {
            Icon::ChevronRight
        };
        let icon_rect = Rect::from_center_size(
            pos2(rect.right() - 10.0, rect.center().y),
            Vec2::splat(12.0),
        );
        widgets::paint_icon(ui.painter(), icon_rect, icon, p.volt, 0.0);
        if response.clicked() {
            response.mark_changed();
        }
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, text));
        response = response.on_hover_cursor(CursorIcon::PointingHand);
    }
    if !tip.is_empty() {
        response = response.on_hover_text(tip);
    }
    let hit = if toggle && response.clicked() {
        RowHit::Members
    } else {
        RowHit::None
    };
    (response, hit)
}

fn added_pill(ui: &mut Ui) -> egui::Response {
    let p = theme::palette(ui.ctx());
    let galley = ui
        .painter()
        .layout_no_wrap("Added".to_owned(), theme::regular(12.0), p.volt);
    let width = galley.size().x + 16.0 + 14.0;
    let (rect, response) = ui.allocate_exact_size(vec2(width, 20.0), Sense::hover());
    ui.painter().rect_filled(rect, 255.0, p.volt_tint);
    let icon = Rect::from_center_size(pos2(rect.left() + 12.0, rect.center().y), Vec2::splat(12.0));
    widgets::paint_icon(ui.painter(), icon, Icon::Check, p.volt, 0.0);
    let y = rect.center().y - galley.size().y * 0.5;
    ui.painter()
        .galley(pos2(rect.left() + 22.0, y), galley, p.volt);
    response
}

fn note_label(ui: &mut Ui, text: &str) -> egui::Response {
    let p = theme::palette(ui.ctx());
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = 220.0;
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.append(
        text,
        0.0,
        text_format(theme::regular(12.0), p.text_muted, false),
    );
    let galley = ui.painter().layout_job(job);
    let elided = galley.elided;
    let (rect, response) = ui.allocate_exact_size(galley.size(), Sense::hover());
    let y = rect.center().y - galley.size().y * 0.5;
    ui.painter()
        .galley(pos2(rect.left(), y), galley, p.text_muted);
    if elided {
        response.on_hover_text(text)
    } else {
        response
    }
}

fn paint_pending(
    ui: &mut Ui,
    rect: Rect,
    staged: &StagedMods,
    hint: &str,
    busy: bool,
    wait: &str,
    stacked: bool,
) -> Click {
    let p = theme::palette(ui.ctx());
    ui.painter().rect(
        rect,
        12.0,
        p.bg_raised,
        Stroke::new(1.0, p.line),
        egui::StrokeKind::Inside,
    );
    let actions_rect = if stacked {
        let split = rect.top() + rect.height() * 0.46;
        paint_pending_text(
            ui,
            rect.left() + 16.0,
            rect.right() - 16.0,
            Rect::from_min_max(rect.min, pos2(rect.right(), split)),
            staged,
            hint,
            &p,
        );
        Rect::from_min_max(pos2(rect.left(), split), rect.max)
    } else {
        rect
    };
    let mut bar = ui.new_child(
        UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::left_to_right(Align::Center)),
    );
    bar.set_clip_rect(rect);
    let mut right = bar.new_child(
        UiBuilder::new()
            .max_rect(actions_rect)
            .layout(Layout::right_to_left(Align::Center))
            .id_salt("pending-actions"),
    );
    right.add_space(12.0);
    let apply_label = if hint.starts_with("Applying rebuilds") {
        "Apply & rebuild"
    } else {
        "Apply"
    };
    let mut apply = widgets::Button::new(apply_label)
        .variant(Variant::Primary)
        .size(Size::Medium)
        .min_width(150.0)
        .enabled(!busy);
    if busy {
        apply = apply.on_disabled_hover_text(wait);
    }
    let apply_response = apply.show(&mut right);
    let discard = widgets::Button::new("Discard")
        .variant(Variant::Ghost)
        .size(Size::Medium)
        .show(&mut right);
    let text_right = apply_response.rect.left().min(discard.rect.left()) - 12.0;
    let text_left = rect.left() + 16.0;
    if !stacked && text_right > text_left + 24.0 && rect.height() > 32.0 {
        paint_pending_text(&bar, text_left, text_right, rect, staged, hint, &p);
    }
    if apply_response.clicked() {
        Click::Apply
    } else if discard.clicked() {
        Click::Discard
    } else {
        Click::None
    }
}

fn paint_pending_text(
    ui: &Ui,
    left: f32,
    right: f32,
    rect: Rect,
    staged: &StagedMods,
    hint: &str,
    p: &Palette,
) {
    let width = (right - left).max(0.0);
    let changes = changes_galley(ui, width, staged.add.len(), staged.remove.len(), p);
    let hint_g = one_line(
        ui,
        width,
        &LineParts {
            name: hint,
            color: p.text_muted,
            size: 12.0,
            version: "",
            desc: "",
            strike: false,
            strong: false,
        },
    );
    let gap = 2.0;
    let block = changes.size().y + gap + hint_g.size().y;
    let mut y = rect.center().y - block * 0.5;
    let changes_h = changes.size().y;
    ui.painter().galley(pos2(left, y), changes, p.text);
    y += changes_h + gap;
    ui.painter().galley(pos2(left, y), hint_g, p.text_muted);
}

fn changes_galley(
    ui: &Ui,
    width: f32,
    adds: usize,
    removes: usize,
    p: &Palette,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = width;
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    let mut first = true;
    if adds > 0 {
        job.append(
            &format!("+{adds}"),
            0.0,
            text_format(theme::semibold(14.0), p.ok, false),
        );
        first = false;
    }
    if removes > 0 {
        job.append(
            &format!("–{removes}"),
            if first { 0.0 } else { 8.0 },
            text_format(theme::semibold(14.0), p.bad, false),
        );
    }
    job.append(
        "  changes",
        0.0,
        text_format(theme::regular(14.0), p.text, false),
    );
    ui.painter().layout_job(job)
}

fn paint_warnings(ui: &mut Ui, warnings: &[String], p: &Palette) {
    if warnings.is_empty() {
        return;
    }
    widgets::card_with_bar(ui, p.warn, |ui| {
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
        for (i, warning) in warnings.iter().enumerate() {
            if i > 0 {
                ui.add_space(4.0);
            }
            ui.label(
                egui::RichText::new(warning)
                    .font(theme::regular(13.0))
                    .color(p.text),
            );
        }
    });
    ui.add_space(8.0);
}

fn paint_error(ui: &mut Ui, rect: Rect, message: &str) -> bool {
    let area = rect.shrink(8.0);
    if area.height() < 8.0 {
        return false;
    }
    let mut col = ui.new_child(
        UiBuilder::new()
            .max_rect(area)
            .layout(Layout::top_down(Align::Min)),
    );
    let p = theme::palette(ui.ctx());
    let mut retry = false;
    widgets::card_with_bar(&mut col, p.bad, |ui| {
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
        ui.label(
            egui::RichText::new("Could not load the catalogue")
                .font(theme::semibold(14.0))
                .color(p.text_bright),
        );
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(message)
                .font(theme::regular(13.0))
                .color(p.text),
        );
        ui.add_space(8.0);
        if widgets::Button::new("Try again")
            .variant(Variant::Secondary)
            .size(Size::Small)
            .show(ui)
            .clicked()
        {
            retry = true;
        }
    });
    retry
}

fn paint_skeletons(ui: &mut Ui) {
    let p = theme::palette(ui.ctx());
    for i in 0..4 {
        if i > 0 {
            ui.add_space(8.0);
        }
        let width = (ui.available_width() * 0.6).max(1.0);
        let (rect, _) = ui.allocate_exact_size(vec2(width, 36.0), Sense::hover());
        ui.painter().rect_filled(rect, 8.0, p.bg_raised);
    }
}

struct EmptyCopy {
    title: String,
    body: &'static str,
    cube: bool,
    button: Option<(&'static str, Variant, EmptyAction)>,
}

fn paint_empty(ui: &mut Ui, rect: Rect, kind: EmptyKind, search: &str) -> EmptyAction {
    let copy = empty_copy(kind, search);
    let p = theme::palette(ui.ctx());
    let block_h = if copy.cube { 168.0 } else { 96.0 };
    let top = if rect.height() > block_h + 48.0 {
        rect.center().y - block_h * 0.5
    } else {
        rect.top() + 8.0
    };
    let area = Rect::from_min_max(
        pos2(rect.left() + 16.0, top),
        pos2(rect.right() - 16.0, rect.bottom() - 8.0),
    );
    if area.height() < 8.0 || area.width() < 8.0 {
        return EmptyAction::None;
    }
    let mut col = ui.new_child(
        UiBuilder::new()
            .max_rect(area)
            .layout(Layout::top_down(Align::Center)),
    );
    if copy.cube {
        let (icon, _) = col.allocate_exact_size(Vec2::splat(56.0), Sense::hover());
        widgets::paint_icon(col.painter(), icon, Icon::HollowCube, p.text_faint, 0.0);
        col.add_space(12.0);
    }
    centered_line(&mut col, &copy.title, theme::semibold(15.0), p.text_bright);
    col.add_space(4.0);
    centered_line(&mut col, copy.body, theme::regular(13.0), p.text_muted);
    if let Some((label, variant, action)) = copy.button {
        col.add_space(12.0);
        if widgets::Button::new(label)
            .variant(variant)
            .size(Size::Small)
            .show(&mut col)
            .clicked()
        {
            return action;
        }
    }
    EmptyAction::None
}

fn empty_copy(kind: EmptyKind, search: &str) -> EmptyCopy {
    match kind {
        EmptyKind::NoMods => EmptyCopy {
            title: "No mods in this instance".to_owned(),
            body: "Browse the catalogue to add some.",
            cube: true,
            button: Some(("Browse mods", Variant::Secondary, EmptyAction::Browse)),
        },
        EmptyKind::NoCatalogue => EmptyCopy {
            title: "No mods in the catalogue".to_owned(),
            body: "This instance's sources did not offer any mods.",
            cube: true,
            button: None,
        },
        EmptyKind::OnlyInstalledLibraries => EmptyCopy {
            title: "Only libraries are installed".to_owned(),
            body: "Libraries are hidden. Show them to see what this instance uses.",
            cube: true,
            button: Some((
                "Show libraries",
                Variant::Secondary,
                EmptyAction::ShowLibraries,
            )),
        },
        EmptyKind::OnlyCatalogueLibraries => EmptyCopy {
            title: "Only libraries are in the catalogue".to_owned(),
            body: "Turn on Libraries to list them.",
            cube: true,
            button: Some((
                "Show libraries",
                Variant::Secondary,
                EmptyAction::ShowLibraries,
            )),
        },
        EmptyKind::Filtered => EmptyCopy {
            title: match_title(search),
            body: "Clear the search, category and library filter.",
            cube: false,
            button: Some(("Clear filters", Variant::Ghost, EmptyAction::Clear)),
        },
    }
}

fn match_title(search: &str) -> String {
    let query = search.trim();
    if query.is_empty() {
        return "Nothing matches these filters".to_owned();
    }
    let mut shown: String = query.chars().take(32).collect();
    if query.chars().count() > 32 {
        shown.push('…');
    }
    format!("Nothing matches \"{shown}\"")
}

fn centered_line(ui: &mut Ui, text: &str, font: FontId, color: Color32) {
    let width = ui.available_width().max(1.0);
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = width;
    job.wrap.max_rows = 3;
    job.halign = Align::Center;
    job.append(text, 0.0, text_format(font, color, false));
    let galley = ui.painter().layout_job(job);
    let (rect, _) = ui.allocate_exact_size(vec2(width, galley.size().y), Sense::hover());
    ui.painter()
        .galley(pos2(rect.left(), rect.top()), galley, color);
}

fn apply_empty(view: &mut InstanceModsUi, action: EmptyAction) -> Click {
    match action {
        EmptyAction::None => Click::None,
        EmptyAction::Browse => {
            view.segment = 1;
            Click::None
        }
        EmptyAction::ShowLibraries => {
            view.show_libraries = true;
            Click::None
        }
        EmptyAction::Clear => {
            view.search.clear();
            view.category = None;
            view.show_libraries = false;
            Click::None
        }
    }
}

fn with_reveal(ui: &mut Ui, id: Id, open: bool, body: impl FnOnce(&mut Ui)) {
    let t = ui
        .ctx()
        .animate_bool_with_time(id, open, theme::anim_secs(ui.ctx(), 0.12));
    if t <= 0.001 {
        return;
    }
    if t >= 0.999 {
        let before = ui.next_widget_position().y;
        body(ui);
        let height = ui.next_widget_position().y - before;
        if height.is_finite() && height > 0.0 {
            ui.ctx()
                .data_mut(|data| data.insert_temp(id.with("h"), height));
        }
        return;
    }
    let full = ui
        .ctx()
        .data(|data| data.get_temp::<f32>(id.with("h")))
        .unwrap_or(ROW_H);
    let shown = full * t;
    if shown < 0.5 {
        return;
    }
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, shown), Sense::hover());
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(Rect::from_min_size(rect.min, vec2(width, full.max(shown))))
            .layout(Layout::top_down(Align::Min)),
    );
    child.set_clip_rect(rect);
    child.set_opacity(t);
    body(&mut child);
}

fn reveal_id(instance: &str, key: &str) -> Id {
    Id::unique(("mods-reveal", instance, key))
}

fn will_add_pill(p: &Palette) -> Trail {
    Trail::Pill {
        text: "Will be added".to_owned(),
        fg: p.volt,
        bg: p.volt_tint,
    }
}

fn will_remove_pill(p: &Palette) -> Trail {
    Trail::Pill {
        text: "Will be removed".to_owned(),
        fg: p.bad,
        bg: theme::with_alpha(p.bad, 38),
    }
}

fn members_label(n: usize, bundle: bool) -> String {
    let noun = if n == 1 {
        "1 member".to_owned()
    } else {
        format!("{n} members")
    };
    if bundle {
        format!("bundle · {noun}")
    } else {
        noun
    }
}

fn shown_categories(cats: &[String]) -> Vec<String> {
    let other: Vec<String> = cats
        .iter()
        .filter(|cat| cat.as_str() != "bundle" && cat.as_str() != "library")
        .take(2)
        .cloned()
        .collect();
    if other.is_empty() {
        cats.iter().take(1).cloned().collect()
    } else {
        other
    }
}

fn kind_mark(kind: &str, p: &Palette) -> (Icon, Color32) {
    match kind {
        "bundle" => (Icon::StackedCubes, p.volt),
        "library" => (Icon::HollowCube, p.text_faint),
        _ => (Icon::Cube, p.text_muted),
    }
}

fn subtitle(desc: &str, authors: &[String]) -> String {
    let author = authors.first().map(String::as_str).unwrap_or("");
    match (desc.is_empty(), author.is_empty()) {
        (true, true) => String::new(),
        (false, true) => desc.to_owned(),
        (true, false) => author.to_owned(),
        (false, false) => format!("{desc} · {author}"),
    }
}

fn hidden_label(n: usize) -> String {
    if n == 1 {
        "+1 library hidden".to_owned()
    } else {
        format!("+{n} libraries hidden")
    }
}

fn show_kind(kind: &str, filter: &Filter, removing: bool) -> bool {
    filter.show_libraries || removing || kind != "library"
}

fn member_visible(pkg: &PackageCard, filter: &Filter, staged: &StagedMods) -> bool {
    show_kind(&pkg.kind, filter, is_removing(staged, &pkg.id))
}

fn hidden_library_count(packages: &[PackageCard], staged: &StagedMods, show: bool) -> usize {
    if show {
        return 0;
    }
    packages
        .iter()
        .filter(|pkg| pkg.kind == "library" && !is_removing(staged, &pkg.id))
        .count()
}

fn sort_by_name(pkgs: &mut [PackageCard]) {
    pkgs.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.id.cmp(&b.id))
    });
}

fn sort_bundles_first(pkgs: &mut [PackageCard]) {
    pkgs.sort_by(|a, b| {
        let rank = |pkg: &PackageCard| u8::from(pkg.kind != "bundle");
        rank(a).cmp(&rank(b)).then_with(|| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| a.id.cmp(&b.id))
        })
    });
}

fn collect_categories(packages: &[PackageCard], catalog: &Catalog) -> Vec<String> {
    let mut set = BTreeSet::new();
    for pkg in packages {
        for cat in &pkg.categories {
            if !cat.is_empty() {
                set.insert(cat.clone());
            }
        }
    }
    for available in catalog.mods() {
        for cat in &available.categories {
            if !cat.is_empty() {
                set.insert(cat.clone());
            }
        }
    }
    set.into_iter().collect()
}

fn find_available<'a>(mods: &'a [AvailableMod], id: &str) -> Option<&'a AvailableMod> {
    mods.iter().find(|available| available.id == id)
}

fn spec_id(spec: &str) -> &str {
    spec.split_once('@').map(|(id, _)| id).unwrap_or(spec)
}

fn is_removing(staged: &StagedMods, id: &str) -> bool {
    staged.remove.iter().any(|item| item == id)
}

fn is_adding(staged: &StagedMods, id: &str) -> bool {
    staged.add.iter().any(|spec| spec_id(spec) == id)
}

fn stage_add(staged: &mut StagedMods, id: &str) {
    let before = staged.remove.len();
    staged.remove.retain(|item| item != id);
    if staged.remove.len() != before {
        return;
    }
    if !is_adding(staged, id) {
        staged.add.push(id.to_owned());
    }
}

fn stage_remove(staged: &mut StagedMods, id: &str) {
    if undo_add(staged, id) {
        return;
    }
    if !is_removing(staged, id) {
        staged.remove.push(id.to_owned());
    }
}

fn undo_add(staged: &mut StagedMods, id: &str) -> bool {
    let before = staged.add.len();
    staged.add.retain(|spec| spec_id(spec) != id);
    staged.add.len() != before
}

fn undo_remove(staged: &mut StagedMods, id: &str) {
    staged.remove.retain(|item| item != id);
}

fn toggle_expand(set: &mut HashSet<String>, id: &str) {
    if !set.remove(id) {
        set.insert(id.to_owned());
    }
}

fn set_click(slot: &mut Click, next: Click) {
    if matches!(slot, Click::None) && !matches!(next, Click::None) {
        *slot = next;
    }
}

fn hit_click(hit: RowHit, id: &str, undo_is_add: bool) -> Click {
    match hit {
        RowHit::Add => Click::Add(id.to_owned()),
        RowHit::Remove => Click::Remove(id.to_owned()),
        RowHit::Undo if undo_is_add => Click::UndoAdd(id.to_owned()),
        RowHit::Undo => Click::UndoRemove(id.to_owned()),
        RowHit::None | RowHit::Body | RowHit::Members => Click::None,
    }
}
