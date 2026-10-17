//! Mods tab: installed packages and the catalogue, with staged changes.
//!
//! The instance page hands this the content rect under the tab strip. Page padding is the
//! parent's; this tab does not add its own inset.

use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use egui::{
    Align, Color32, CursorIcon, FontId, Id, Key, Layout, Modifiers, Rect, Sense, Stroke,
    TextFormat, Ui, UiBuilder, Vec2, pos2, vec2,
};
use pwc_manifest::{Version, VersionReq};
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
///
/// Row models are cached here, not on [`InstanceModsUi`]: the view is cloned every frame, and
/// the models are the expensive part (closures, reverse dependencies, both lists).
#[derive(Clone, Debug, Default)]
pub(crate) struct ModsTabState {
    views: HashMap<String, InstanceModsUi>,
    cache: Option<ModelCache>,
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
    /// Selected package. Shared by Installed and Browse.
    selected: Option<String>,
    /// Narrow width: the details panel has replaced the list.
    details_open: bool,
    /// `/` asked the search field to take focus this frame.
    focus_search: bool,
    /// Arrow keys moved the selection. The row paints a focus ring.
    key_nav: bool,
    /// The next list paint scrolls the selected row into view. Cleared after that paint.
    pending_scroll: bool,
    /// The pending-bar diff popup is open.
    impact_open: bool,
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
            selected: None,
            details_open: false,
            focus_search: false,
            key_nav: false,
            pending_scroll: false,
            impact_open: false,
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
    /// `true` rebuilds after the mod list changes.
    Apply(bool),
    Discard,
    Retry,
    /// Stage one direct mod for a newest-version relock.
    Update(String),
    UpdateAll,
    /// Stage `id` or `id@=version`, replacing any staged spec for that id.
    Pin(String),
    /// Drop a staged newest-version relock.
    UndoUpdate(String),
}

enum Catalog {
    Pending,
    /// The backend's shared catalogue (mods, warnings): borrowed, never copied per frame.
    Ready(Arc<(Vec<AvailableMod>, Vec<String>)>),
    Failed(String),
}

impl Catalog {
    fn from_loaded(loaded: Loaded<Arc<(Vec<AvailableMod>, Vec<String>)>>) -> Self {
        match loaded {
            Loaded::Idle | Loaded::Loading => Self::Pending,
            Loaded::Ready(data) => Self::Ready(data),
            Loaded::Err(message) => Self::Failed(message),
        }
    }

    fn mods(&self) -> &[AvailableMod] {
        match self {
            Self::Ready(data) => &data.0,
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

#[derive(Clone, Debug)]
struct AddRow {
    id: String,
    name: String,
    version: String,
    description: String,
    kind: String,
    /// "Will be added" plus the predicted dependencies, when there are any.
    pill: String,
    compat: Option<CompatNote>,
}

#[derive(Clone, Debug)]
struct PlainRow {
    id: String,
    name: String,
    description: String,
    removing: bool,
    /// "Will be removed", with the orphan count when this removal drops packages.
    pill: String,
}

#[derive(Clone, Debug)]
struct MemberRow {
    pkg: PackageCard,
    removing: bool,
    /// Orphans in this package's installed closure, when it is staged for removal.
    unused: usize,
}

#[derive(Clone, Debug)]
struct DirectRow {
    pkg: PackageCard,
    open: bool,
    /// Open because a filter or a removal forced it, not because the user expanded it.
    forced: bool,
    members: Vec<MemberRow>,
    member_total: usize,
    removing: bool,
    /// Orphans this removal drops. Zero when the row is not staged for removal.
    unused: usize,
    /// Catalogue version newer than the lock, for a direct repository/store package.
    newer: Option<String>,
    /// Staged relock onto the newest version.
    updating_to: Option<String>,
    /// Staged `id@=version`.
    pinned: Option<String>,
}

#[derive(Clone, Debug)]
struct DepGroup {
    count: usize,
    open: bool,
    forced: bool,
    rows: Vec<MemberRow>,
}

#[derive(Clone, Debug)]
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

#[derive(Clone, Copy, Debug)]
enum BrowseAction {
    Add,
    Installed,
    Indirect,
    Added,
    Removing,
}

#[derive(Clone, Debug)]
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
    compat: Option<CompatNote>,
}

#[derive(Clone, Debug)]
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
        tip: String,
    },
    /// "↑ 1.4.0", semibold 11 on an arc tint. Click stages the update.
    Newer(String),
    /// "Will update → 1.4.0".
    Updating(String),
    /// "→ 1.3.0 pinned".
    Pinned(String),
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
    Update,
}

/// Side of 880 px: the details panel sits beside the list. Narrower, it replaces the list.
const DETAILS_BREAK: f32 = 880.0;

/// What [`draw_row`] needs to paint selection. The id is compared as a string, never focused.
struct Chrome<'a> {
    selected: &'a str,
    key_ring: bool,
    scroll: bool,
}

impl Chrome<'_> {
    fn from_view(view: &InstanceModsUi) -> Chrome<'_> {
        Chrome {
            selected: view.selected.as_deref().unwrap_or(""),
            key_ring: view.key_nav,
            scroll: view.pending_scroll,
        }
    }

    fn picks(&self, id: &str) -> bool {
        self.selected == id
    }
}

/// A catalogue requirement the instance's Mod API does not satisfy.
#[derive(Clone, Debug)]
struct CompatNote {
    /// "needs Mod API >=0.5".
    pill: String,
    /// Why Add will probably fail.
    tip: String,
    req: String,
    have: String,
}

/// One predicted package change.
#[derive(Clone, Debug)]
struct DiffLine {
    id: String,
    name: String,
    version: String,
    library: bool,
    /// Staged-add ids that pull this package in. Empty when the line is itself a staged root,
    /// or when it is leaving rather than arriving.
    needed_by: Vec<String>,
}

/// Predicted result of the staged list. The resolver can still disagree.
#[derive(Clone, Debug, Default)]
struct Impact {
    /// Staged adds and the dependencies they pull in, minus anything already installed.
    adds: Vec<DiffLine>,
    /// Staged removals that nothing remaining still reaches.
    removes: Vec<DiffLine>,
    /// Installed packages nothing reachable needs, and that the user did not stage.
    orphans: Vec<DiffLine>,
    /// Staged adds whose Mod API requirement excludes this instance.
    incompatible: usize,
}

/// What Delete does to the row that is selected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeleteAction {
    None,
    Remove,
    UndoAdd,
    UndoRemove,
    UndoUpdate,
}

/// One keyboard-navigable row, in the order the list paints it.
#[derive(Clone, Debug)]
struct NavRow {
    id: String,
    bundle: bool,
    forced_open: bool,
    delete: DeleteAction,
}

/// Which Undo a row's button means.
#[derive(Clone, Copy)]
enum UndoKind {
    Add,
    Remove,
    Update,
}

/// Cached list models. One entry: the Mods tab shows one instance at a time.
#[derive(Clone, Debug)]
struct ModelCache {
    instance: String,
    data_fp: u64,
    staged: StagedMods,
    impact: Impact,
    /// Package id → mods and bundles that require it.
    rev: HashMap<String, Vec<RevRef>>,
    view_fp: u64,
    installed: Option<InstalledModel>,
    browse: Option<BrowseModel>,
    installed_n: usize,
    browse_n: Option<usize>,
    /// Category name and how many packages in the visible segment have it.
    categories: Vec<(String, usize)>,
    nav: Vec<NavRow>,
    /// Direct mods with a newer catalogue version that are not staged yet.
    update_n: usize,
    /// Details-panel model for the selected id. Dropped with the rest when data or staging
    /// changes.
    detail: Option<Detail>,
}

/// A package that requires another, for the details panel.
#[derive(Clone, Debug)]
struct RevRef {
    id: String,
    name: String,
    version: String,
    kind: String,
}

impl App {
    pub(crate) fn mods_tab(&mut self, ui: &mut egui::Ui, entry: &InstanceEntry) {
        let name = entry.card.name.clone();
        let has_packages = !entry.card.packages.is_empty();
        let catalog = Catalog::from_loaded(self.backend.available_mods(&name));
        // Moved out and back in, not cloned: the view holds the expansion sets.
        let mut view = self
            .mods_ui
            .views
            .remove(&name)
            .unwrap_or_else(|| InstanceModsUi::fresh(has_packages));
        let empty = StagedMods::default();
        let staged = self.staged_mods.get(&name).unwrap_or(&empty);
        let rebuild = self.settings_draft.rebuild_after_mod_change;
        let busy = self.busy(&name) || entry.status.busy;
        let wait = wait_reason(self, &name, entry.status.busy);
        let hint = rebuild_hint(rebuild, last_build_ms(entry));
        let modal = self.modal_open();

        let click = ui
            .push_id(&name, |ui| {
                let mut keys = Click::None;
                let narrow = ui.available_width() < DETAILS_BREAK;
                handle_mods_keys(
                    ui,
                    modal,
                    narrow,
                    &name,
                    &mut view,
                    self.mods_ui.cache.as_ref(),
                    &mut keys,
                );
                let painted = paint_tab(
                    ui,
                    &mut view,
                    entry,
                    staged,
                    &catalog,
                    &BarOpts {
                        hint: &hint,
                        busy,
                        wait: &wait,
                        rebuild,
                        last_ms: last_build_ms(entry),
                    },
                    &mut self.mods_ui.cache,
                );
                set_click(&mut keys, painted);
                keys
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

    /// Snapshot helper: select a package and, on a narrow window, push the details panel.
    #[allow(dead_code)]
    pub(crate) fn mods_select_for_tests(&mut self, instance: &str, id: &str, push: bool) {
        let has = self.instance_has_mods(instance);
        let view = self
            .mods_ui
            .views
            .entry(instance.to_owned())
            .or_insert_with(|| InstanceModsUi::fresh(has));
        view.selected = Some(id.to_owned());
        view.details_open = push;
    }

    /// Snapshot helper: open the dependency-diff popup.
    #[allow(dead_code)]
    pub(crate) fn mods_open_impact_for_tests(&mut self, instance: &str) {
        let has = self.instance_has_mods(instance);
        let view = self
            .mods_ui
            .views
            .entry(instance.to_owned())
            .or_insert_with(|| InstanceModsUi::fresh(has));
        view.impact_open = true;
    }

    /// Keyboard test: the selected package, if any.
    #[allow(dead_code)]
    pub(crate) fn mods_selected_for_tests(&self, instance: &str) -> Option<String> {
        self.mods_ui
            .views
            .get(instance)
            .and_then(|view| view.selected.clone())
    }

    /// Keyboard test: ids staged for removal.
    #[allow(dead_code)]
    pub(crate) fn mods_staged_remove_for_tests(&self, instance: &str) -> Vec<String> {
        self.staged_mods
            .get(instance)
            .map(|staged| staged.remove.clone())
            .unwrap_or_default()
    }

    /// Keyboard test: ids staged for a newest-version relock.
    #[allow(dead_code)]
    pub(crate) fn mods_staged_update_for_tests(&self, instance: &str) -> Vec<String> {
        self.staged_mods
            .get(instance)
            .map(|staged| staged.update.clone())
            .unwrap_or_default()
    }

    /// Snapshot helper: stage a newest-version relock for one direct mod.
    #[allow(dead_code)]
    pub(crate) fn mods_stage_update_for_tests(&mut self, instance: &str, id: &str) {
        self.with_staged(instance, |staged| stage_update(staged, id));
    }

    /// Whether the Mods tab wants Enter this frame: a row is selected, so Enter acts on it
    /// instead of starting Play. Read by the app's shortcut handler before the tab paints.
    pub(crate) fn mods_claims_enter(&self) -> bool {
        self.instance_tab == super::InstanceTab::Mods
            && matches!(&self.main_view, Some(super::MainView::Instance(name))
                if self.mods_ui.views.get(name).is_some_and(|view| view.selected.is_some()))
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
            Click::Apply(rebuild) => self.apply_staged(instance, rebuild),
            Click::Add(id) => self.with_staged(instance, |staged| stage_add(staged, &id)),
            Click::Remove(id) => self.with_staged(instance, |staged| stage_remove(staged, &id)),
            Click::UndoAdd(id) => self.with_staged(instance, |staged| {
                undo_add(staged, &id);
            }),
            Click::UndoRemove(id) => self.with_staged(instance, |staged| undo_remove(staged, &id)),
            Click::Update(id) => self.with_staged(instance, |staged| stage_update(staged, &id)),
            Click::UpdateAll => {
                let entry = self
                    .backend
                    .instances()
                    .iter()
                    .find(|entry| entry.card.name == instance)
                    .cloned();
                let Some(entry) = entry else {
                    return;
                };
                let catalog = Catalog::from_loaded(self.backend.available_mods(instance));
                self.with_staged(instance, |staged| {
                    stage_update_all(staged, &entry.card.packages, catalog.mods());
                });
            }
            Click::Pin(spec) => self.with_staged(instance, |staged| stage_pin(staged, &spec)),
            Click::UndoUpdate(id) => {
                self.with_staged(instance, |staged| undo_update(staged, &id));
            }
        }
    }

    fn apply_staged(&mut self, instance: &str, rebuild: bool) {
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
                update: staged.update,
                build: Some(rebuild),
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
    rebuild: bool,
    last_ms: Option<u64>,
}

fn paint_tab(
    ui: &mut Ui,
    view: &mut InstanceModsUi,
    entry: &InstanceEntry,
    staged: &StagedMods,
    catalog: &Catalog,
    bar: &BarOpts<'_>,
    cache: &mut Option<ModelCache>,
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

    ensure_cache(cache, entry, catalog, view, staged);
    let (installed_n, browse_n, update_n) = cache
        .as_ref()
        .map(|item| (item.installed_n, item.browse_n, item.update_n))
        .unwrap_or((0, None, 0));
    let categories = cache
        .as_ref()
        .map(|item| item.categories.as_slice())
        .unwrap_or(&[]);
    let before = (
        view.search.clone(),
        view.category.clone(),
        view.show_libraries,
        view.segment,
    );
    let mut click = paint_toolbar(
        ui,
        view,
        &entry.card.name,
        categories,
        installed_n,
        browse_n,
        update_n,
    );
    let extra = (12.0 - ui.spacing().item_spacing.y).max(0.0);
    if extra > 0.0 {
        ui.add_space(extra);
    }
    if view.search != before.0 || view.category != before.1 || view.show_libraries != before.2 {
        view.collapsed.clear();
        view.deps_collapsed = false;
    }
    if view.search != before.0
        || view.category != before.1
        || view.show_libraries != before.2
        || view.segment != before.3
    {
        ensure_cache(cache, entry, catalog, view, staged);
    }

    let list_top = ui.next_widget_position().y;
    let list_h = (full.bottom() - reserve - list_top).max(0.0);
    let wide = full.width() >= DETAILS_BREAK;
    let selected = view.selected.clone();
    let show_side = wide && selected.is_some();
    let show_push = !wide && view.details_open && selected.is_some();
    if list_h > 1.0 {
        if show_push {
            let mut rect = list_rect(ui, list_h);
            if reserve <= 0.0 {
                rect.max.y -= 16.0;
            }
            let _ = ui.allocate_rect(rect, Sense::hover());
            set_click(
                &mut click,
                paint_details(ui, rect, true, view, entry, catalog, staged, cache),
            );
        } else if show_side {
            let gap = 16.0;
            let panel_w = 320.0;
            let list_w = (full.width() - gap - panel_w).max(120.0);
            let list_rect = Rect::from_min_size(pos2(full.left(), list_top), vec2(list_w, list_h));
            // Without the pending bar the card would sit on the window edge.
            let panel_h = if reserve > 0.0 {
                list_h
            } else {
                (list_h - 16.0).max(0.0)
            };
            let panel_rect = Rect::from_min_size(
                pos2(full.left() + list_w + gap, list_top),
                vec2(panel_w, panel_h),
            );
            let _ = ui.allocate_rect(
                Rect::from_min_size(pos2(full.left(), list_top), vec2(full.width(), list_h)),
                Sense::hover(),
            );
            let mut list_ui = ui.new_child(egui::UiBuilder::new().max_rect(list_rect));
            list_ui.set_clip_rect(list_rect);
            let listed = paint_segment(
                &mut list_ui,
                list_h,
                &entry.card.name,
                view,
                staged,
                catalog,
                cache,
                false,
            );
            set_click(&mut click, listed);
            set_click(
                &mut click,
                paint_details(ui, panel_rect, false, view, entry, catalog, staged, cache),
            );
        } else {
            let listed = paint_segment(
                ui,
                list_h,
                &entry.card.name,
                view,
                staged,
                catalog,
                cache,
                !wide,
            );
            set_click(&mut click, listed);
        }
    }
    view.pending_scroll = false;
    if bar_h > 0.5 {
        let bar_rect = Rect::from_min_max(pos2(full.left(), full.bottom() - bar_h), full.max);
        let impact = cache.as_ref().map(|item| &item.impact);
        set_click(
            &mut click,
            paint_pending(
                ui,
                bar_rect,
                staged,
                entry,
                impact,
                &mut view.impact_open,
                bar,
                stacked,
            ),
        );
    }
    click
}

fn paint_toolbar(
    ui: &mut Ui,
    view: &mut InstanceModsUi,
    instance: &str,
    categories: &[(String, usize)],
    installed_n: usize,
    browse_n: Option<usize>,
    update_n: usize,
) -> Click {
    let wide = ui.available_width() >= TOOLBAR_WRAP;
    let mut click = Click::None;
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
            let update_w = if update_n > 0 { 156.0 } else { 0.0 };
            let search_w = (ui.available_width() - 312.0 - update_w).max(80.0);
            let response = widgets::search_input_with_id(
                ui,
                ("mods-search", instance),
                &mut view.search,
                "Search mods",
                search_w,
            );
            take_search_focus(view, &response);
            category_box(ui, view, categories);
            libraries_toggle(ui, &mut view.show_libraries);
            set_click(&mut click, update_all_button(ui, update_n));
        } else {
            let search_w = ui.available_width().max(40.0);
            let response = widgets::search_input_with_id(
                ui,
                ("mods-search", instance),
                &mut view.search,
                "Search mods",
                search_w,
            );
            take_search_focus(view, &response);
        }
    });
    if !wide {
        ui.horizontal(|ui| {
            category_box(ui, view, categories);
            libraries_toggle(ui, &mut view.show_libraries);
            set_click(&mut click, update_all_button(ui, update_n));
        });
    }
    click
}

fn take_search_focus(view: &mut InstanceModsUi, response: &egui::Response) {
    // The hint is a placeholder, not the accessible name. The keyboard test looks up "Search mods".
    let _ = response.clone().accessible_name("Search mods");
    if view.focus_search {
        response.request_focus();
        view.focus_search = false;
    }
}

fn update_all_button(ui: &mut Ui, n: usize) -> Click {
    if n == 0 {
        return Click::None;
    }
    let label = format!("Update all ({n})");
    let response = widgets::Button::new(&label)
        .variant(Variant::Ghost)
        .size(Size::Small)
        .show(ui)
        .on_hover_text("Move every outdated mod to its newest compatible version");
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &label));
    if response.clicked() {
        Click::UpdateAll
    } else {
        Click::None
    }
}

fn category_box(ui: &mut Ui, view: &mut InstanceModsUi, categories: &[(String, usize)]) {
    let p = theme::palette(ui.ctx());
    let label = match &view.category {
        Some(cat) => categories
            .iter()
            .find(|(name, _)| name == cat)
            .map(|(name, n)| format!("{name} · {n}"))
            .unwrap_or_else(|| cat.clone()),
        None => "All categories".to_owned(),
    };
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
            for (cat, n) in categories {
                ui.selectable_value(
                    &mut view.category,
                    Some(cat.clone()),
                    format!("{cat} · {n}"),
                );
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

#[allow(clippy::too_many_arguments)]
fn paint_segment(
    ui: &mut Ui,
    list_h: f32,
    instance: &str,
    view: &mut InstanceModsUi,
    staged: &StagedMods,
    catalog: &Catalog,
    cache: &Option<ModelCache>,
    push: bool,
) -> Click {
    let Some(cache) = cache.as_ref() else {
        return Click::None;
    };
    if view.segment == 1 {
        paint_browse(ui, list_h, instance, view, catalog, cache, push)
    } else {
        paint_installed(ui, list_h, instance, view, staged, cache, push)
    }
}

fn paint_installed(
    ui: &mut Ui,
    list_h: f32,
    instance: &str,
    view: &mut InstanceModsUi,
    staged: &StagedMods,
    cache: &ModelCache,
    push: bool,
) -> Click {
    let Some(model) = cache.installed.as_ref() else {
        return Click::None;
    };
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
                let hit = paint_add_row(ui, row, &Chrome::from_view(view), &p);
                note_hit(view, &row.id, hit, UndoKind::Add, push, &mut click);
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
                let hit = paint_plain_row(ui, row, true, &Chrome::from_view(view), &p);
                note_hit(view, &row.id, hit, UndoKind::Remove, push, &mut click);
            }
        }
        for row in &model.orphans {
            let hit = paint_plain_row(ui, row, false, &Chrome::from_view(view), &p);
            note_hit(view, &row.id, hit, UndoKind::Remove, push, &mut click);
        }
        if !model.direct.is_empty() {
            section_label(
                ui,
                &mut started,
                &format!("Mods ({})", model.direct.len()),
                p.text_muted,
            );
            for row in &model.direct {
                paint_direct(ui, instance, view, row, &p, push, &mut click);
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
                    let hit = paint_member_row(
                        ui,
                        &row.pkg,
                        row.removing,
                        row.unused,
                        "",
                        0.0,
                        &Chrome::from_view(view),
                        &p,
                    );
                    note_hit(view, &row.pkg.id, hit, UndoKind::Remove, push, &mut click);
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
    push: bool,
    click: &mut Click,
) {
    let hit = paint_direct_row(ui, row, &Chrome::from_view(view), p);
    if hit == RowHit::Members {
        if row.forced {
            toggle_expand(&mut view.collapsed, &row.pkg.id);
        } else {
            toggle_expand(&mut view.expanded, &row.pkg.id);
        }
    }
    let undo = if row.updating_to.is_some() {
        UndoKind::Update
    } else if row.pinned.is_some() {
        UndoKind::Add
    } else {
        UndoKind::Remove
    };
    note_hit(view, &row.pkg.id, hit, undo, push, click);
    if row.pkg.kind == "bundle" && row.member_total > 0 {
        let bundle_name = row.pkg.name.clone();
        with_reveal(ui, reveal_id(instance, &row.pkg.id), row.open, |ui| {
            for member in &row.members {
                let hit = paint_member_row(
                    ui,
                    &member.pkg,
                    member.removing,
                    member.unused,
                    &bundle_name,
                    MEMBER_INDENT,
                    &Chrome::from_view(view),
                    p,
                );
                note_hit(view, &member.pkg.id, hit, UndoKind::Remove, push, click);
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
    cache: &ModelCache,
    push: bool,
) -> Click {
    let Some(model) = cache.browse.as_ref() else {
        return Click::None;
    };
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
        Catalog::Ready(data) => {
            let warnings = &data.1;
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
                let sectioned = view.search.trim().is_empty();
                scroll_list(ui, list_h, (instance, "browse"), |ui| {
                    let p = theme::palette(ui.ctx());
                    paint_warnings(ui, &warnings, &p);
                    if sectioned {
                        paint_browse_sections(ui, view, model, &p, push, &mut click);
                    } else {
                        for row in &model.rows {
                            paint_one_browse(ui, view, row, &p, push, &mut click);
                        }
                    }
                    ui.add_space(8.0);
                });
                click
            }
        }
    }
}

fn paint_browse_sections(
    ui: &mut Ui,
    view: &mut InstanceModsUi,
    model: &BrowseModel,
    p: &Palette,
    push: bool,
    click: &mut Click,
) {
    let mut bundles = Vec::new();
    let mut mods = Vec::new();
    for row in &model.rows {
        if row.kind == "bundle" {
            bundles.push(row);
        } else {
            mods.push(row);
        }
    }
    let mut started = false;
    if !bundles.is_empty() {
        section_label(
            ui,
            &mut started,
            &format!("Bundles ({})", bundles.len()),
            p.volt,
        );
        for row in bundles {
            paint_one_browse(ui, view, row, p, push, click);
        }
    }
    if !mods.is_empty() {
        section_label(
            ui,
            &mut started,
            &format!("Mods ({})", mods.len()),
            p.text_muted,
        );
        for row in mods {
            paint_one_browse(ui, view, row, p, push, click);
        }
    }
}

fn paint_one_browse(
    ui: &mut Ui,
    view: &mut InstanceModsUi,
    row: &BrowseRow,
    p: &Palette,
    push: bool,
    click: &mut Click,
) {
    let hit = paint_browse_row(ui, row, &Chrome::from_view(view), p);
    let undo = match row.action {
        BrowseAction::Added => UndoKind::Add,
        BrowseAction::Removing => UndoKind::Remove,
        BrowseAction::Add | BrowseAction::Installed | BrowseAction::Indirect => UndoKind::Remove,
    };
    note_hit(view, &row.id, hit, undo, push, click);
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
                    selected: false,
                    key_ring: false,
                    scroll: false,
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
    impact: &Impact,
) -> InstalledModel {
    let filter = Filter::from_view(view);
    let packages = &entry.card.packages;
    let unresolved_ids = unresolved_ids(entry);
    let avail = index_available(catalog.mods());
    let by_pkg = index_packages(packages);
    let facts = Facts {
        impact,
        game_api: entry.card.game.api.as_deref(),
        avail: &avail,
        by_pkg: &by_pkg,
    };
    let (directs, members, deps) = group_children(packages);
    let direct = directs
        .into_iter()
        .filter_map(|pkg| {
            let kids = members.get(&pkg.id).map(Vec::as_slice).unwrap_or(&[]);
            plan_direct(pkg, kids, view, &filter, staged, &facts)
        })
        .collect();
    InstalledModel {
        adding: adding_rows(packages, &unresolved_ids, &filter, staged, &facts),
        unresolved: unresolved_rows(&unresolved_ids, &filter, staged, &facts),
        orphans: orphan_rows(packages, &unresolved_ids, &filter, staged),
        direct,
        deps: plan_deps(&deps, view, &filter, staged, &facts),
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
    let parents = bundle_parents(packages);
    let mut direct = Vec::new();
    let mut members: HashMap<String, Vec<PackageCard>> = HashMap::new();
    let mut deps = Vec::new();
    for pkg in packages {
        if pkg.direct {
            direct.push(pkg.clone());
            continue;
        }
        if let Some(parent) = parents.get(&pkg.id) {
            members.entry(parent.clone()).or_default().push(pkg.clone());
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

fn bundle_parents(packages: &[PackageCard]) -> HashMap<String, String> {
    let bundles: Vec<&PackageCard> = packages
        .iter()
        .filter(|pkg| pkg.direct && pkg.kind == "bundle")
        .collect();
    let bundle_ids: HashSet<&str> = bundles.iter().map(|pkg| pkg.id.as_str()).collect();
    let mut map = HashMap::new();
    for pkg in packages {
        if pkg.direct {
            continue;
        }
        if let Some(id) = pkg
            .in_bundles
            .iter()
            .find(|id| bundle_ids.contains(id.as_str()))
        {
            map.insert(pkg.id.clone(), id.clone());
            continue;
        }
        if let Some(parent) = bundles
            .iter()
            .find(|parent| parent.dependencies.iter().any(|dep| dep == &pkg.id))
        {
            map.insert(pkg.id.clone(), parent.id.clone());
        }
    }
    map
}

fn plan_direct(
    pkg: PackageCard,
    kids: &[PackageCard],
    view: &InstanceModsUi,
    filter: &Filter,
    staged: &StagedMods,
    facts: &Facts<'_>,
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
    let members = shown_members(kids, open, user_open, filter, staged, facts);
    let newer = newer_version(&pkg, facts.avail.get(pkg.id.as_str()).copied());
    let updating_to = staged.update.iter().any(|id| id == &pkg.id).then(|| {
        facts
            .avail
            .get(pkg.id.as_str())
            .map(|item| item.version.clone())
            .unwrap_or_else(|| "newest".to_owned())
    });
    let pinned = pin_label(staged, &pkg.id);
    let unused = if removing {
        unused_count(&pkg.id, facts)
    } else {
        0
    };
    Some(DirectRow {
        pkg,
        open,
        forced,
        members,
        member_total: visible_kids,
        removing,
        unused,
        newer,
        updating_to,
        pinned,
    })
}

fn shown_members(
    kids: &[PackageCard],
    open: bool,
    user_open: bool,
    filter: &Filter,
    staged: &StagedMods,
    facts: &Facts<'_>,
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
        .map(|kid| {
            let removing = is_removing(staged, &kid.id);
            MemberRow {
                unused: if removing {
                    unused_count(&kid.id, facts)
                } else {
                    0
                },
                removing,
                pkg: kid.clone(),
            }
        })
        .collect()
}

fn plan_deps(
    deps: &[PackageCard],
    view: &InstanceModsUi,
    filter: &Filter,
    staged: &StagedMods,
    facts: &Facts<'_>,
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
            unused: if removing {
                unused_count(&pkg.id, facts)
            } else {
                0
            },
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
    filter: &Filter,
    staged: &StagedMods,
    facts: &Facts<'_>,
) -> Vec<AddRow> {
    let mut rows = Vec::new();
    for spec in &staged.add {
        let id = spec_id(spec);
        if packages.iter().any(|pkg| pkg.id == id) || unresolved.iter().any(|item| item == id) {
            continue;
        }
        let found = facts.avail.get(id).copied();
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
            pill: add_pill_text(id, facts.impact),
            compat: found.and_then(|item| compat_note(item, facts.game_api)),
            id: id.to_owned(),
            name: name.to_owned(),
            version: version.to_owned(),
            description: desc.to_owned(),
            kind: kind.to_owned(),
        });
    }
    rows.sort_by_cached_key(|row| (row.name.to_lowercase(), row.id.clone()));
    rows
}

fn unresolved_rows(
    ids: &[String],
    filter: &Filter,
    staged: &StagedMods,
    facts: &Facts<'_>,
) -> Vec<PlainRow> {
    let desc = "Requested, but not in the lock.";
    let mut rows = Vec::new();
    for id in ids {
        let removing = is_removing(staged, id);
        let found = facts.avail.get(id.as_str()).copied();
        let name = found.map(|m| m.name.as_str()).unwrap_or(id.as_str());
        let cats: &[String] = found.map(|m| m.categories.as_slice()).unwrap_or(&[]);
        if !removing && (!filter.search(name, id, desc) || !filter.category_ok(cats)) {
            continue;
        }
        rows.push(PlainRow {
            pill: if removing {
                remove_pill_text(0)
            } else {
                String::new()
            },
            id: id.clone(),
            name: name.to_owned(),
            description: desc.to_owned(),
            removing,
        });
    }
    rows.sort_by_cached_key(|row| (row.name.to_lowercase(), row.id.clone()));
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
            pill: remove_pill_text(0),
            id: id.clone(),
            name: id.clone(),
            description: String::new(),
            removing: true,
        });
    }
    rows.sort_by_cached_key(|row| row.id.clone());
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
    game_api: Option<&str>,
) -> BrowseModel {
    let Catalog::Ready(data) = catalog else {
        return BrowseModel {
            rows: Vec::new(),
            hidden_libraries: 0,
            non_library: 0,
        };
    };
    let mods = &data.0;
    let filter = Filter::from_view(view);
    let names: HashMap<&str, &str> = mods
        .iter()
        .map(|item| (item.id.as_str(), item.name.as_str()))
        .collect();
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
        let show_compat = matches!(action, BrowseAction::Add | BrowseAction::Added);
        rows.push(BrowseRow {
            compat: show_compat
                .then(|| compat_note(available, game_api))
                .flatten(),
            id: available.id.clone(),
            name: available.name.clone(),
            version: available.version.clone(),
            sub: subtitle(&available.description, &available.authors),
            kind: available.kind.clone(),
            categories: available.categories.clone(),
            members: available
                .members
                .iter()
                .map(|id| {
                    names
                        .get(id.as_str())
                        .copied()
                        .unwrap_or(id.as_str())
                        .to_owned()
                })
                .collect(),
            member_count: available.members.len(),
            action,
            required_by,
        });
    }
    rows.sort_by_cached_key(|row| (row.name.to_lowercase(), row.id.clone()));
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

/// Rows outside the scroll viewport only reserve their height: no trail, no galleys, no
/// animation lookups. The row the keyboard just moved to is always painted so it can scroll.
fn skip_offscreen(ui: &mut Ui, height: f32, scroll_here: bool) -> bool {
    if scroll_here {
        return false;
    }
    let rect = Rect::from_min_size(
        ui.next_widget_position(),
        vec2(ui.available_width(), height),
    );
    if ui.is_rect_visible(rect) {
        return false;
    }
    let _ = ui.allocate_exact_size(rect.size(), Sense::hover());
    true
}

fn paint_add_row(ui: &mut Ui, row: &AddRow, chrome: &Chrome<'_>, p: &Palette) -> RowHit {
    if skip_offscreen(ui, ROW_H, chrome.scroll && chrome.picks(&row.id)) {
        return RowHit::None;
    }
    let (icon, color) = kind_mark(&row.kind, p);
    let mut trail = will_add_pill(p, &row.pill, row.compat.as_ref());
    trail.push(Trail::Undo);
    let tint = if chrome.picks(&row.id) {
        Some(theme::with_alpha(p.volt, 15))
    } else {
        Some(theme::with_alpha(p.volt, TINT))
    };
    draw_row(
        ui,
        RowSpec {
            id: &row.id,
            height: ROW_H,
            indent: 0.0,
            clickable: true,
            tint,
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
            selected: chrome.picks(&row.id),
            key_ring: chrome.key_ring && chrome.picks(&row.id),
            scroll: chrome.scroll && chrome.picks(&row.id),
        },
    )
}

fn paint_plain_row(
    ui: &mut Ui,
    row: &PlainRow,
    unresolved: bool,
    chrome: &Chrome<'_>,
    p: &Palette,
) -> RowHit {
    if skip_offscreen(ui, ROW_H, chrome.scroll && chrome.picks(&row.id)) {
        return RowHit::None;
    }
    let (icon, color) = if unresolved {
        (Icon::HollowCube, p.bad)
    } else {
        (Icon::Cube, p.text_muted)
    };
    let mut trail = Vec::new();
    if row.removing {
        trail.push(will_remove_pill(p, &row.pill));
        trail.push(Trail::Undo);
    } else {
        trail.push(Trail::Pill {
            text: "Unresolved".to_owned(),
            fg: p.bad,
            bg: theme::with_alpha(p.bad, 38),
            tip: String::new(),
        });
        trail.push(Trail::Remove);
    }
    let staged_tint = row.removing.then(|| theme::with_alpha(p.bad, TINT));
    let tint = if chrome.picks(&row.id) {
        Some(theme::with_alpha(p.volt, 15))
    } else {
        staged_tint
    };
    draw_row(
        ui,
        RowSpec {
            id: &row.id,
            height: ROW_H,
            indent: 0.0,
            clickable: true,
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
            selected: chrome.picks(&row.id),
            key_ring: chrome.key_ring && chrome.picks(&row.id),
            scroll: chrome.scroll && chrome.picks(&row.id),
        },
    )
}

fn paint_direct_row(ui: &mut Ui, row: &DirectRow, chrome: &Chrome<'_>, p: &Palette) -> RowHit {
    if skip_offscreen(ui, ROW_H, chrome.scroll && chrome.picks(&row.pkg.id)) {
        return RowHit::None;
    }
    let (icon, color) = kind_mark(&row.pkg.kind, p);
    let trail = direct_trail(row, p);
    let tint = if chrome.picks(&row.pkg.id) {
        Some(theme::with_alpha(p.volt, 15))
    } else if row.removing {
        Some(theme::with_alpha(p.bad, TINT))
    } else if row.updating_to.is_some() || row.pinned.is_some() {
        Some(theme::with_alpha(p.arc, TINT))
    } else {
        None
    };
    draw_row(
        ui,
        RowSpec {
            id: &row.pkg.id,
            height: ROW_H,
            indent: 0.0,
            clickable: true,
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
            selected: chrome.picks(&row.pkg.id),
            key_ring: chrome.key_ring && chrome.picks(&row.pkg.id),
            scroll: chrome.scroll && chrome.picks(&row.pkg.id),
        },
    )
}

fn direct_trail(row: &DirectRow, p: &Palette) -> Vec<Trail> {
    let mut trail = Vec::new();
    if let Some(version) = &row.newer
        && row.updating_to.is_none()
        && row.pinned.is_none()
        && !row.removing
    {
        trail.push(Trail::Newer(version.clone()));
    }
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
            tip: String::new(),
        });
    }
    if let Some(version) = &row.updating_to {
        trail.push(Trail::Updating(version.clone()));
        trail.push(Trail::Undo);
    } else if let Some(version) = &row.pinned {
        trail.push(Trail::Pinned(version.clone()));
        trail.push(Trail::Undo);
    } else if row.removing {
        trail.push(will_remove_pill(p, &remove_pill_text(row.unused)));
        trail.push(Trail::Undo);
    } else {
        trail.push(Trail::Remove);
    }
    trail
}

#[allow(clippy::too_many_arguments)]
fn paint_member_row(
    ui: &mut Ui,
    pkg: &PackageCard,
    removing: bool,
    unused: usize,
    bundle: &str,
    indent: f32,
    chrome: &Chrome<'_>,
    p: &Palette,
) -> RowHit {
    if skip_offscreen(ui, ROW_H, chrome.scroll && chrome.picks(&pkg.id)) {
        return RowHit::None;
    }
    let (icon, color) = kind_mark(&pkg.kind, p);
    let mut trail = Vec::new();
    if removing {
        trail.push(will_remove_pill(p, &remove_pill_text(unused)));
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
            clickable: true,
            tint: if chrome.picks(&pkg.id) {
                Some(theme::with_alpha(p.volt, 15))
            } else {
                tint
            },
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
            selected: chrome.picks(&pkg.id),
            key_ring: chrome.key_ring && chrome.picks(&pkg.id),
            scroll: chrome.scroll && chrome.picks(&pkg.id),
        },
    )
}

fn paint_browse_row(ui: &mut Ui, row: &BrowseRow, chrome: &Chrome<'_>, p: &Palette) -> RowHit {
    if skip_offscreen(ui, BROWSE_H, chrome.scroll && chrome.picks(&row.id)) {
        return RowHit::None;
    }
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
            clickable: true,
            tint: if chrome.picks(&row.id) {
                Some(theme::with_alpha(p.volt, 15))
            } else {
                tint
            },
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
            selected: chrome.picks(&row.id),
            key_ring: chrome.key_ring && chrome.picks(&row.id),
            scroll: chrome.scroll && chrome.picks(&row.id),
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
            tip: String::new(),
        });
    }
    match row.action {
        BrowseAction::Add => trail.push(Trail::Add),
        BrowseAction::Installed => {
            trail.push(Trail::Pill {
                text: "Installed".to_owned(),
                fg: p.ok,
                bg: theme::with_alpha(p.ok, 38),
                tip: String::new(),
            });
            trail.push(Trail::Remove);
        }
        BrowseAction::Indirect => {
            trail.push(Trail::Note(format!("Required by {}", row.required_by)));
        }
        BrowseAction::Added => {
            trail.push(Trail::Added);
            if let Some(note) = &row.compat {
                trail.push(compat_trail(p, note));
            }
            trail.push(Trail::Undo);
        }
        BrowseAction::Removing => {
            trail.push(will_remove_pill(p, "Will be removed"));
            trail.push(Trail::Undo);
        }
    }
    if matches!(row.action, BrowseAction::Add)
        && let Some(note) = &row.compat
    {
        // The badge sits immediately before the action, which is the last item.
        let action = trail.pop();
        trail.push(compat_trail(p, note));
        if let Some(action) = action {
            trail.push(action);
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
    selected: bool,
    /// Keyboard moved here. Painted, not requested: a real focus would swallow the arrows.
    key_ring: bool,
    /// Scroll this row into view once. Off-screen rows still allocate, then return.
    scroll: bool,
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
        if spec.scroll {
            ui.scroll_to_rect(rect, None);
        }
        if !ui.is_rect_visible(rect) {
            return RowHit::None;
        }
        paint_row_fill(
            ui,
            rect,
            &response,
            spec.tint,
            spec.clickable,
            spec.key_ring,
        );
        if spec.selected {
            let bar =
                Rect::from_center_size(pos2(rect.left() + 2.0, rect.center().y), vec2(2.0, 28.0));
            ui.painter()
                .rect_filled(bar, 1.0, theme::palette(ui.ctx()).volt);
        }
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
    key_ring: bool,
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
    if clickable && (response.has_focus() || key_ring) {
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
        Trail::Pill { text, fg, bg, tip } => {
            let response = widgets::pill(ui, text, *fg, *bg);
            let response = if tip.is_empty() {
                response
            } else {
                response.on_hover_text(tip)
            };
            (response, RowHit::None)
        }
        Trail::Newer(version) => {
            let response = arc_pill(ui, &format!("↑ {version}"));
            let hit = if response.clicked() {
                RowHit::Update
            } else {
                RowHit::None
            };
            (response, hit)
        }
        Trail::Updating(version) => {
            let p = theme::palette(ui.ctx());
            (
                widgets::pill(ui, &format!("Will update → {version}"), p.arc, p.arc_tint),
                RowHit::None,
            )
        }
        Trail::Pinned(version) => {
            let p = theme::palette(ui.ctx());
            (
                widgets::pill(ui, &format!("→ {version} pinned"), p.arc, p.arc_tint),
                RowHit::None,
            )
        }
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

/// "↑ 1.4.0". Semibold 11, arc on an arc tint. Click stages the update.
fn arc_pill(ui: &mut Ui, text: &str) -> egui::Response {
    let p = theme::palette(ui.ctx());
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), theme::semibold(11.0), p.arc);
    let width = galley.size().x + 16.0;
    let (rect, mut response) = ui.allocate_exact_size(vec2(width, 20.0), Sense::click());
    ui.painter().rect_filled(rect, 255.0, p.arc_tint);
    let y = rect.center().y - galley.size().y * 0.5;
    ui.painter()
        .galley(pos2(rect.left() + 8.0, y), galley, p.arc);
    if response.clicked() {
        response.mark_changed();
    }
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, text));
    response.on_hover_cursor(CursorIcon::PointingHand)
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

#[allow(clippy::too_many_arguments)]
fn paint_pending(
    ui: &mut Ui,
    rect: Rect,
    staged: &StagedMods,
    entry: &InstanceEntry,
    impact: Option<&Impact>,
    open: &mut bool,
    bar: &BarOpts<'_>,
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
    let incompatible = impact.map(|item| item.incompatible).unwrap_or(0);
    let hint_color = if incompatible > 0 {
        p.warn
    } else {
        p.text_muted
    };
    let hint_owned;
    let hint = if incompatible > 0 {
        let verb = if incompatible == 1 {
            "mod needs"
        } else {
            "mods need"
        };
        hint_owned = format!("{incompatible} {verb} a newer Mod API · Apply may fail to resolve");
        hint_owned.as_str()
    } else {
        bar.hint
    };
    let counts = bar_counts(staged, &entry.card.packages, impact);
    let tight = !stacked && rect.width() < 560.0;
    let size = if stacked || tight {
        Size::Small
    } else {
        Size::Medium
    };
    let primary_min = if stacked || tight { 0.0 } else { 150.0 };
    let primary_label = if bar.rebuild {
        "Apply & rebuild"
    } else {
        "Apply"
    };
    let alt_label = if bar.rebuild {
        "Apply only"
    } else {
        "Apply & rebuild"
    };
    let actions_rect = if stacked {
        let split = rect.top() + rect.height() * 0.46;
        Rect::from_min_max(pos2(rect.left(), split), rect.max)
    } else {
        rect
    };
    let mut host = ui.new_child(
        UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::left_to_right(Align::Center)),
    );
    host.set_clip_rect(rect);
    let mut right = host.new_child(
        UiBuilder::new()
            .max_rect(actions_rect)
            .layout(Layout::right_to_left(Align::Center))
            .id_salt("pending-actions"),
    );
    right.add_space(12.0);
    let mut primary = widgets::Button::new(primary_label)
        .variant(Variant::Primary)
        .size(size)
        .min_width(primary_min)
        .enabled(!bar.busy);
    let mut alt = widgets::Button::new(alt_label)
        .variant(Variant::Ghost)
        .size(size)
        .enabled(!bar.busy);
    if bar.busy {
        primary = primary.on_disabled_hover_text(bar.wait);
        alt = alt.on_disabled_hover_text(bar.wait);
    }
    let primary_response = primary.show(&mut right);
    let alt_response = alt.show(&mut right);
    let discard = widgets::Button::new("Discard")
        .variant(Variant::Ghost)
        .size(size)
        .show(&mut right);
    let _ = primary_response
        .clone()
        .on_hover_text(apply_tip(bar.rebuild, bar.last_ms));
    let _ = alt_response
        .clone()
        .on_hover_text(apply_tip(!bar.rebuild, bar.last_ms));
    let text_left = rect.left() + 16.0;
    let text_rect = if stacked {
        Rect::from_min_max(
            pos2(text_left, rect.top() + 6.0),
            pos2(rect.right() - 16.0, actions_rect.top() - 2.0),
        )
    } else {
        let text_right = primary_response
            .rect
            .left()
            .min(alt_response.rect.left())
            .min(discard.rect.left())
            - 12.0;
        Rect::from_min_max(pos2(text_left, rect.top()), pos2(text_right, rect.bottom()))
    };
    if text_rect.width() > 24.0 && text_rect.height() > 16.0 {
        let summary = summary_response(&mut host, text_rect, &counts, hint, hint_color, &p);
        if summary.clicked() {
            *open = !*open;
        }
        let _ = summary.clone().on_hover_cursor(CursorIcon::PointingHand);
        let frame = egui::Frame::new()
            .fill(p.bg_raised)
            .stroke(Stroke::new(1.0, p.line))
            .corner_radius(12.0)
            .inner_margin(12.0);
        egui::Popup::from_response(&summary)
            .align(egui::RectAlign::TOP_START)
            .width(360.0)
            .frame(frame)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .open_bool(open)
            .show(|ui| paint_diff_popup(ui, impact));
    }
    if primary_response.clicked() {
        Click::Apply(bar.rebuild)
    } else if alt_response.clicked() {
        Click::Apply(!bar.rebuild)
    } else if discard.clicked() {
        Click::Discard
    } else {
        Click::None
    }
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

fn will_add_pill(p: &Palette, text: &str, compat: Option<&CompatNote>) -> Vec<Trail> {
    let mut trail = vec![Trail::Pill {
        text: text.to_owned(),
        fg: p.volt,
        bg: p.volt_tint,
        tip: String::new(),
    }];
    if let Some(note) = compat {
        trail.push(compat_trail(p, note));
    }
    trail
}

fn will_remove_pill(p: &Palette, text: &str) -> Trail {
    Trail::Pill {
        text: text.to_owned(),
        fg: p.bad,
        bg: theme::with_alpha(p.bad, 38),
        tip: String::new(),
    }
}

fn compat_trail(p: &Palette, note: &CompatNote) -> Trail {
    Trail::Pill {
        text: note.pill.clone(),
        fg: p.warn,
        bg: theme::with_alpha(p.warn, 38),
        tip: note.tip.clone(),
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
    pkgs.sort_by_cached_key(|pkg| (pkg.name.to_lowercase(), pkg.id.clone()));
}

fn sort_bundles_first(pkgs: &mut [PackageCard]) {
    pkgs.sort_by_cached_key(|pkg| {
        (
            u8::from(pkg.kind != "bundle"),
            pkg.name.to_lowercase(),
            pkg.id.clone(),
        )
    });
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
    undo_update(staged, id);
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
    undo_update(staged, id);
    if undo_add(staged, id) {
        return;
    }
    if !is_removing(staged, id) {
        staged.remove.push(id.to_owned());
    }
}

fn stage_update(staged: &mut StagedMods, id: &str) {
    undo_add(staged, id);
    staged.remove.retain(|item| item != id);
    if !staged.update.iter().any(|item| item == id) {
        staged.update.push(id.to_owned());
    }
}

fn stage_update_all(staged: &mut StagedMods, packages: &[PackageCard], catalog: &[AvailableMod]) {
    let avail = index_available(catalog);
    for pkg in packages {
        let Some(found) = avail.get(pkg.id.as_str()).copied() else {
            continue;
        };
        if newer_version(pkg, Some(found)).is_some() {
            stage_update(staged, &pkg.id);
        }
    }
}

fn stage_pin(staged: &mut StagedMods, spec: &str) {
    let id = spec_id(spec);
    undo_add(staged, id);
    staged.remove.retain(|item| item != id);
    undo_update(staged, id);
    staged.add.push(spec.to_owned());
}

fn undo_update(staged: &mut StagedMods, id: &str) {
    staged.update.retain(|item| item != id);
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

fn hit_click(hit: RowHit, id: &str, undo: UndoKind) -> Click {
    match hit {
        RowHit::Add => Click::Add(id.to_owned()),
        RowHit::Remove => Click::Remove(id.to_owned()),
        RowHit::Update => Click::Update(id.to_owned()),
        RowHit::Undo => match undo {
            UndoKind::Add => Click::UndoAdd(id.to_owned()),
            UndoKind::Remove => Click::UndoRemove(id.to_owned()),
            UndoKind::Update => Click::UndoUpdate(id.to_owned()),
        },
        RowHit::None | RowHit::Body | RowHit::Members => Click::None,
    }
}
struct Facts<'a> {
    impact: &'a Impact,
    game_api: Option<&'a str>,
    avail: &'a HashMap<&'a str, &'a AvailableMod>,
    by_pkg: &'a HashMap<&'a str, &'a PackageCard>,
}

fn index_available(mods: &[AvailableMod]) -> HashMap<&str, &AvailableMod> {
    mods.iter().map(|item| (item.id.as_str(), item)).collect()
}

fn index_packages(packages: &[PackageCard]) -> HashMap<&str, &PackageCard> {
    packages.iter().map(|pkg| (pkg.id.as_str(), pkg)).collect()
}

/// `None` when the requirement parses and matches, or when either side is missing.
fn compat_note(available: &AvailableMod, game_api: Option<&str>) -> Option<CompatNote> {
    let req = available.pwc_api.as_deref()?;
    let have = game_api?;
    let parsed_req = VersionReq::parse(req).ok()?;
    let parsed_have = Version::parse(have).ok()?;
    if parsed_req.matches(&parsed_have) {
        return None;
    }
    Some(CompatNote {
        pill: format!("needs Mod API {req}"),
        tip: format!("This instance has Mod API {have}. Adding it will probably fail to resolve."),
        req: req.to_owned(),
        have: have.to_owned(),
    })
}

/// Catalogue version newer than the lock, for a direct repository or store package.
fn newer_version(pkg: &PackageCard, available: Option<&AvailableMod>) -> Option<String> {
    if !pkg.direct {
        return None;
    }
    let source = pkg.source.as_str();
    if !source.starts_with("repository:") && source != "store" {
        return None;
    }
    let available = available?;
    let locked = Version::parse(&pkg.version).ok()?;
    let newest = Version::parse(&available.version).ok()?;
    (newest > locked).then(|| available.version.clone())
}

/// Staged spec for an installed id: `*` follows newest, otherwise the pinned version.
fn pin_label(staged: &StagedMods, id: &str) -> Option<String> {
    let spec = staged.add.iter().find(|spec| spec_id(spec) == id)?;
    let Some((_, rest)) = spec.split_once('@') else {
        return Some("*".to_owned());
    };
    let version = rest.strip_prefix('=').unwrap_or(rest);
    if version.is_empty() {
        Some("*".to_owned())
    } else {
        Some(version.to_owned())
    }
}

fn remove_pill_text(n: usize) -> String {
    if n == 0 {
        "Will be removed".to_owned()
    } else {
        format!("Will be removed · −{n} unused")
    }
}

fn add_pill_text(id: &str, impact: &Impact) -> String {
    let mut extras: Vec<&DiffLine> = impact
        .adds
        .iter()
        .filter(|line| line.needed_by.iter().any(|root| root == id))
        .collect();
    extras.sort_by_cached_key(|line| (line.name.to_lowercase(), line.id.clone()));
    if extras.is_empty() {
        return "Will be added".to_owned();
    }
    if extras.len() > 2 {
        let n = extras.len();
        return format!("Will be added · +{n} dependencies");
    }
    let names = extras
        .iter()
        .map(|line| {
            if line.library {
                format!("{} (library)", line.name)
            } else {
                line.name.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("Will be added · {names}")
}

fn installed_closure(id: &str, by_pkg: &HashMap<&str, &PackageCard>) -> HashSet<String> {
    let mut seen = HashSet::new();
    let mut queue = VecDeque::new();
    queue.push_back(id.to_owned());
    while let Some(cur) = queue.pop_front() {
        if !seen.insert(cur.clone()) {
            continue;
        }
        let Some(pkg) = by_pkg.get(cur.as_str()) else {
            continue;
        };
        for dep in &pkg.dependencies {
            queue.push_back(dep.clone());
        }
    }
    seen
}

fn unused_count(id: &str, facts: &Facts<'_>) -> usize {
    let closure = installed_closure(id, facts.by_pkg);
    facts
        .impact
        .orphans
        .iter()
        .filter(|line| line.id != id && closure.contains(&line.id))
        .count()
}

fn closure_of(
    id: &str,
    avail: &HashMap<&str, &AvailableMod>,
    memo: &mut HashMap<String, Vec<String>>,
) -> Vec<String> {
    if let Some(found) = memo.get(id) {
        return found.clone();
    }
    let mut seen = HashSet::new();
    let mut order = Vec::new();
    let mut queue = VecDeque::new();
    queue.push_back(id.to_owned());
    while let Some(cur) = queue.pop_front() {
        if !seen.insert(cur.clone()) {
            continue;
        }
        order.push(cur.clone());
        let Some(item) = avail.get(cur.as_str()).copied() else {
            continue;
        };
        let mut next: Vec<&str> = item.dependencies.iter().map(String::as_str).collect();
        if item.kind == "bundle" {
            for member in &item.members {
                if !next.contains(&member.as_str()) {
                    next.push(member.as_str());
                }
            }
        }
        for dep in next {
            if !seen.contains(dep) {
                queue.push_back(dep.to_owned());
            }
        }
    }
    memo.insert(id.to_owned(), order.clone());
    order
}

fn describe_line(
    id: &str,
    avail: &HashMap<&str, &AvailableMod>,
    packages: &HashMap<&str, &PackageCard>,
    prefer_catalog: bool,
) -> (String, String, bool) {
    let from_catalog = avail.get(id).copied().map(|item| {
        (
            item.name.clone(),
            item.version.clone(),
            item.kind == "library",
        )
    });
    let from_pkg = packages
        .get(id)
        .copied()
        .map(|pkg| (pkg.name.clone(), pkg.version.clone(), pkg.kind == "library"));
    let preferred = if prefer_catalog {
        from_catalog.or(from_pkg)
    } else {
        from_pkg.or(from_catalog)
    };
    preferred.unwrap_or_else(|| (id.to_owned(), String::new(), false))
}

fn diff_line(
    id: &str,
    avail: &HashMap<&str, &AvailableMod>,
    packages: &HashMap<&str, &PackageCard>,
    prefer_catalog: bool,
    needed_by: Vec<String>,
) -> DiffLine {
    let (name, version, library) = describe_line(id, avail, packages, prefer_catalog);
    DiffLine {
        id: id.to_owned(),
        name,
        version,
        library,
        needed_by,
    }
}

/// Predicted adds, removals and orphans. The resolver can still disagree.
fn predict(
    packages: &[PackageCard],
    catalog: &[AvailableMod],
    game_api: Option<&str>,
    staged: &StagedMods,
) -> Impact {
    let avail = index_available(catalog);
    let by_pkg = index_packages(packages);
    let installed: HashSet<&str> = packages.iter().map(|pkg| pkg.id.as_str()).collect();
    let mut roots = HashSet::new();
    for spec in &staged.add {
        let id = spec_id(spec);
        if !installed.contains(id) {
            roots.insert(id.to_owned());
        }
    }

    let mut memo = HashMap::new();
    let mut adds: Vec<DiffLine> = Vec::new();
    let mut seen_add = HashSet::new();
    for root in &roots {
        for id in closure_of(root, &avail, &mut memo) {
            if installed.contains(id.as_str()) {
                continue;
            }
            if !seen_add.insert(id.clone()) {
                if !roots.contains(&id)
                    && let Some(line) = adds.iter_mut().find(|line| line.id == id)
                    && !line.needed_by.iter().any(|item| item == root)
                {
                    line.needed_by.push(root.clone());
                }
                continue;
            }
            let needed_by = if roots.contains(&id) {
                Vec::new()
            } else {
                vec![root.clone()]
            };
            adds.push(diff_line(&id, &avail, &by_pkg, true, needed_by));
        }
    }
    for line in &mut adds {
        line.needed_by.sort();
    }
    adds.sort_by_cached_key(|line| line.id.clone());

    let mut reach_roots: HashSet<String> = packages
        .iter()
        .filter(|pkg| pkg.direct)
        .map(|pkg| pkg.id.clone())
        .collect();
    for id in &staged.remove {
        reach_roots.remove(id);
    }
    for spec in &staged.add {
        reach_roots.insert(spec_id(spec).to_owned());
    }
    let reachable = reachable_from(&reach_roots, &by_pkg, &avail);

    let mut removes = Vec::new();
    for id in &staged.remove {
        if reachable.contains(id) {
            continue;
        }
        removes.push(diff_line(id, &avail, &by_pkg, false, Vec::new()));
    }
    removes.sort_by_cached_key(|line| line.id.clone());

    let mut orphans = Vec::new();
    for pkg in packages {
        if staged.remove.iter().any(|id| id == &pkg.id) || reachable.contains(&pkg.id) {
            continue;
        }
        orphans.push(diff_line(&pkg.id, &avail, &by_pkg, false, Vec::new()));
    }
    orphans.sort_by_cached_key(|line| line.id.clone());

    let mut incompatible = 0;
    for spec in &staged.add {
        let id = spec_id(spec);
        if installed.contains(id) {
            continue;
        }
        let Some(item) = avail.get(id).copied() else {
            continue;
        };
        if compat_note(item, game_api).is_some() {
            incompatible += 1;
        }
    }

    Impact {
        adds,
        removes,
        orphans,
        incompatible,
    }
}

fn reachable_from(
    roots: &HashSet<String>,
    by_pkg: &HashMap<&str, &PackageCard>,
    avail: &HashMap<&str, &AvailableMod>,
) -> HashSet<String> {
    let mut seen = HashSet::new();
    let mut queue: VecDeque<String> = roots.iter().cloned().collect();
    while let Some(id) = queue.pop_front() {
        if !seen.insert(id.clone()) {
            continue;
        }
        if let Some(pkg) = by_pkg.get(id.as_str()) {
            for dep in &pkg.dependencies {
                queue.push_back(dep.clone());
            }
        } else if let Some(item) = avail.get(id.as_str()).copied() {
            for dep in item.dependencies.iter().chain(item.members.iter()) {
                queue.push_back(dep.clone());
            }
        }
    }
    seen
}

fn rev_from_pkg(pkg: &PackageCard) -> RevRef {
    RevRef {
        id: pkg.id.clone(),
        name: pkg.name.clone(),
        version: pkg.version.clone(),
        kind: pkg.kind.clone(),
    }
}

fn rev_from_available(item: &AvailableMod) -> RevRef {
    RevRef {
        id: item.id.clone(),
        name: item.name.clone(),
        version: item.version.clone(),
        kind: item.kind.clone(),
    }
}

fn push_rev(map: &mut HashMap<String, Vec<RevRef>>, dep: &str, referrer: RevRef) {
    let list = map.entry(dep.to_owned()).or_default();
    if list.iter().any(|item| item.id == referrer.id) {
        return;
    }
    list.push(referrer);
}

fn reverse_refs(
    packages: &[PackageCard],
    catalog: &[AvailableMod],
) -> HashMap<String, Vec<RevRef>> {
    let mut map: HashMap<String, Vec<RevRef>> = HashMap::new();
    for pkg in packages {
        let referrer = rev_from_pkg(pkg);
        for dep in &pkg.dependencies {
            push_rev(&mut map, dep, referrer.clone());
        }
    }
    for pkg in packages {
        for bundle_id in &pkg.in_bundles {
            if let Some(bundle) = packages.iter().find(|item| &item.id == bundle_id) {
                push_rev(&mut map, &pkg.id, rev_from_pkg(bundle));
            }
        }
    }
    for item in catalog {
        let referrer = rev_from_available(item);
        for dep in item.dependencies.iter().chain(item.members.iter()) {
            push_rev(&mut map, dep, referrer.clone());
        }
    }
    for list in map.values_mut() {
        list.sort_by_cached_key(|item| (item.name.to_lowercase(), item.id.clone()));
    }
    map
}

#[derive(Clone, Copy, Default)]
struct BarCounts {
    mods: usize,
    deps: usize,
    leaving: usize,
    unused: usize,
    updates: usize,
}

impl BarCounts {
    fn is_blank(self) -> bool {
        self.mods + self.deps + self.leaving + self.unused + self.updates == 0
    }
}

fn bar_counts(staged: &StagedMods, packages: &[PackageCard], impact: Option<&Impact>) -> BarCounts {
    let installed: HashSet<&str> = packages.iter().map(|pkg| pkg.id.as_str()).collect();
    let updates = staged.update.len().saturating_add(
        staged
            .add
            .iter()
            .filter(|spec| installed.contains(spec_id(spec)))
            .count(),
    );
    let Some(impact) = impact else {
        return BarCounts {
            updates,
            ..BarCounts::default()
        };
    };
    let roots: HashSet<&str> = staged
        .add
        .iter()
        .map(|spec| spec_id(spec))
        .filter(|id| !installed.contains(id))
        .collect();
    let mut mods = 0;
    let mut deps = 0;
    for line in &impact.adds {
        if roots.contains(line.id.as_str()) {
            mods += 1;
        } else {
            deps += 1;
        }
    }
    BarCounts {
        mods,
        deps,
        leaving: impact.removes.len(),
        unused: impact.orphans.len(),
        updates,
    }
}

fn plural<'a>(n: usize, one: &'a str, many: &'a str) -> &'a str {
    if n == 1 { one } else { many }
}

fn summary_response(
    ui: &mut Ui,
    rect: Rect,
    counts: &BarCounts,
    hint: &str,
    hint_color: Color32,
    p: &Palette,
) -> egui::Response {
    let response = ui.interact(
        rect,
        ui.scope_id().with("mods-impact-summary"),
        Sense::click(),
    );
    let width = rect.width().max(1.0);
    let summary = summary_galley(ui, width, counts, p);
    let hint_galley = if hint.is_empty() {
        None
    } else {
        Some(hint_galley(ui, width, hint, hint_color))
    };
    let summary_h = summary.size().y;
    let hint_h = hint_galley
        .as_ref()
        .map(|galley| galley.size().y)
        .unwrap_or(0.0);
    let gap = if hint_galley.is_some() { 2.0 } else { 0.0 };
    let mut y = rect.center().y - (summary_h + gap + hint_h) * 0.5;
    if y < rect.top() {
        y = rect.top();
    }
    ui.painter().galley(pos2(rect.left(), y), summary, p.text);
    if let Some(galley) = hint_galley {
        let hint_y = y + summary_h + gap;
        if hint_y + galley.size().y <= rect.bottom() + 1.0 {
            ui.painter()
                .galley(pos2(rect.left(), hint_y), galley, hint_color);
        }
    }
    response
}

fn summary_galley(
    ui: &Ui,
    width: f32,
    counts: &BarCounts,
    p: &Palette,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = width;
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    if counts.is_blank() {
        job.append(
            "Changes pending",
            0.0,
            text_format(theme::regular(14.0), p.text_muted, false),
        );
    } else {
        let mut started = false;
        append_group(
            &mut job,
            &count_group(
                "+",
                p.ok,
                p.text,
                Phrase::new(counts.mods, "mod", "mods"),
                Phrase::new(counts.deps, "dependency", "dependencies"),
            ),
            p.text,
            &mut started,
        );
        append_group(
            &mut job,
            &count_group(
                "−",
                p.bad,
                p.text,
                Phrase::new(counts.leaving, "mod", "mods"),
                Phrase::new(counts.unused, "unused", "unused"),
            ),
            p.text,
            &mut started,
        );
        append_group(
            &mut job,
            &count_group(
                "↑",
                p.arc,
                p.text,
                Phrase::new(counts.updates, "update", "updates"),
                Phrase::new(0, "", ""),
            ),
            p.text,
            &mut started,
        );
    }
    ui.painter().layout_job(job)
}

fn hint_galley(ui: &Ui, width: f32, hint: &str, color: Color32) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = width;
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.append(hint, 0.0, text_format(theme::regular(12.0), color, false));
    ui.painter().layout_job(job)
}

struct Span {
    text: String,
    color: Color32,
    strong: bool,
}

struct Phrase {
    n: usize,
    one: &'static str,
    many: &'static str,
}

impl Phrase {
    fn new(n: usize, one: &'static str, many: &'static str) -> Self {
        Self { n, one, many }
    }
}

fn count_phrase(sign: &str, phrase: Phrase, color: Color32, text: Color32) -> Vec<Span> {
    if phrase.n == 0 || phrase.one.is_empty() {
        return Vec::new();
    }
    vec![
        Span {
            text: format!("{sign}{}", phrase.n),
            color,
            strong: true,
        },
        Span {
            text: format!(" {}", plural(phrase.n, phrase.one, phrase.many)),
            color: text,
            strong: false,
        },
    ]
}

fn count_group(
    sign: &str,
    color: Color32,
    text: Color32,
    first: Phrase,
    second: Phrase,
) -> Vec<Span> {
    let mut spans = count_phrase(sign, first, color, text);
    let extra = count_phrase(sign, second, color, text);
    if spans.is_empty() {
        return extra;
    }
    if !extra.is_empty() {
        spans.push(Span {
            text: ", ".to_owned(),
            color: text,
            strong: false,
        });
        spans.extend(extra);
    }
    spans
}

fn append_group(
    job: &mut egui::text::LayoutJob,
    spans: &[Span],
    text: Color32,
    started: &mut bool,
) {
    if spans.is_empty() {
        return;
    }
    if *started {
        job.append(" · ", 0.0, text_format(theme::regular(14.0), text, false));
    }
    *started = true;
    for span in spans {
        let font = if span.strong {
            theme::semibold(14.0)
        } else {
            theme::regular(14.0)
        };
        job.append(&span.text, 0.0, text_format(font, span.color, false));
    }
}

fn apply_tip(rebuild: bool, last_ms: Option<u64>) -> String {
    if !rebuild {
        return "Change the mod list now; build later with Play".to_owned();
    }
    match last_ms {
        Some(ms) => format!(
            "Change the mod list and rebuild now · last build {} min",
            ms / 60_000
        ),
        None => "Change the mod list and rebuild now".to_owned(),
    }
}

struct PopupRow {
    sign: &'static str,
    color_ok: bool,
    main: String,
    suffix: String,
    gap: bool,
}

fn paint_diff_popup(ui: &mut Ui, impact: Option<&Impact>) {
    let p = theme::palette(ui.ctx());
    let rows = impact.map(popup_rows).unwrap_or_default();
    if !rows.is_empty() {
        egui::ScrollArea::vertical()
            .id_salt("mods-diff-lines")
            .max_height(320.0)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 0.0;
                for row in &rows {
                    if row.gap {
                        ui.add_space(8.0);
                    }
                    paint_diff_row(ui, row, &p);
                }
            });
        ui.add_space(6.0);
        let (rule, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
        ui.painter().hline(
            rule.x_range(),
            rule.center().y,
            Stroke::new(1.0, p.line_soft),
        );
        ui.add_space(6.0);
    }
    ui.label(
        egui::RichText::new("Predicted by the launcher; the resolver decides.")
            .font(theme::regular(12.0))
            .color(p.text_muted),
    );
}

fn popup_rows(impact: &Impact) -> Vec<PopupRow> {
    let mut rows = Vec::new();
    let mut roots: Vec<&DiffLine> = impact
        .adds
        .iter()
        .filter(|line| line.needed_by.is_empty())
        .collect();
    let mut extras: Vec<&DiffLine> = impact
        .adds
        .iter()
        .filter(|line| !line.needed_by.is_empty())
        .collect();
    sort_lines(&mut roots);
    sort_lines(&mut extras);
    let mut removes: Vec<&DiffLine> = impact.removes.iter().collect();
    let mut orphans: Vec<&DiffLine> = impact.orphans.iter().collect();
    sort_lines(&mut removes);
    sort_lines(&mut orphans);
    push_popup(&mut rows, &roots, true, false, impact);
    push_popup(&mut rows, &extras, true, true, impact);
    push_popup(&mut rows, &removes, false, false, impact);
    push_popup(&mut rows, &orphans, false, true, impact);
    // One gap, between what arrives and what leaves.
    for index in 1..rows.len() {
        rows[index].gap = rows[index].sign != rows[index - 1].sign;
    }
    rows
}

fn sort_lines(lines: &mut [&DiffLine]) {
    lines.sort_by_cached_key(|line| (line.name.to_lowercase(), line.id.clone()));
}

fn push_popup(
    rows: &mut Vec<PopupRow>,
    lines: &[&DiffLine],
    adding: bool,
    suffix: bool,
    impact: &Impact,
) {
    for line in lines {
        let suffix = if !suffix {
            String::new()
        } else if adding {
            let names = line
                .needed_by
                .iter()
                .map(|id| impact_name(impact, id))
                .collect::<Vec<_>>()
                .join(", ");
            format!(" · needed by {names}")
        } else {
            " · no longer needed".to_owned()
        };
        rows.push(PopupRow {
            sign: if adding { "+" } else { "−" },
            color_ok: adding,
            main: diff_main(line),
            suffix,
            gap: false,
        });
    }
}

fn impact_name(impact: &Impact, id: &str) -> String {
    impact
        .adds
        .iter()
        .chain(impact.removes.iter())
        .chain(impact.orphans.iter())
        .find(|line| line.id == id)
        .map(|line| line.name.clone())
        .unwrap_or_else(|| id.to_owned())
}

fn diff_main(line: &DiffLine) -> String {
    let mut text = format!(" {}", line.name);
    if !line.version.is_empty() {
        text.push(' ');
        text.push_str(&line.version);
    }
    if line.library {
        text.push_str(" (library)");
    }
    text
}

fn paint_diff_row(ui: &mut Ui, row: &PopupRow, p: &Palette) {
    let width = ui.available_width().max(1.0);
    let sign_color = if row.color_ok { p.ok } else { p.bad };
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = width;
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.append(
        row.sign,
        0.0,
        text_format(theme::semibold(13.0), sign_color, false),
    );
    job.append(
        &row.main,
        0.0,
        text_format(theme::regular(13.0), p.text, false),
    );
    if !row.suffix.is_empty() {
        job.append(
            &row.suffix,
            0.0,
            text_format(theme::regular(13.0), p.text_muted, false),
        );
    }
    let galley = ui.painter().layout_job(job);
    let (rect, _) = ui.allocate_exact_size(vec2(width, 24.0), Sense::hover());
    let y = rect.center().y - galley.size().y * 0.5;
    ui.painter().galley(pos2(rect.left(), y), galley, p.text);
}
fn ensure_cache(
    cache: &mut Option<ModelCache>,
    entry: &InstanceEntry,
    catalog: &Catalog,
    view: &InstanceModsUi,
    staged: &StagedMods,
) {
    let data_fp = data_fingerprint(entry, catalog);
    let data_ok = cache.as_ref().is_some_and(|item| {
        item.instance == entry.card.name && item.data_fp == data_fp && &item.staged == staged
    });
    if !data_ok {
        let impact = predict(
            &entry.card.packages,
            catalog.mods(),
            entry.card.game.api.as_deref(),
            staged,
        );
        let rev = reverse_refs(&entry.card.packages, catalog.mods());
        *cache = Some(ModelCache {
            instance: entry.card.name.clone(),
            data_fp,
            staged: staged.clone(),
            impact,
            rev,
            view_fp: 0,
            installed: None,
            browse: None,
            installed_n: 0,
            browse_n: None,
            categories: Vec::new(),
            nav: Vec::new(),
            update_n: 0,
            detail: None,
        });
    }
    let view_fp = view_fingerprint(view);
    let ready = cache.as_ref().is_some_and(|item| {
        item.view_fp == view_fp
            && if view.segment == 1 {
                item.browse.is_some()
            } else {
                item.installed.is_some()
            }
    });
    if ready {
        return;
    }
    let Some(item) = cache.as_mut() else {
        return;
    };
    rebuild_view(item, entry, catalog, view, staged, view_fp);
}

fn rebuild_view(
    item: &mut ModelCache,
    entry: &InstanceEntry,
    catalog: &Catalog,
    view: &InstanceModsUi,
    staged: &StagedMods,
    view_fp: u64,
) {
    let impact = &item.impact;
    if view.segment == 1 {
        let model = build_browse(entry, catalog, view, staged, entry.card.game.api.as_deref());
        let sectioned = view.search.trim().is_empty();
        item.nav = nav_browse(&model, sectioned);
        item.browse_n = match catalog {
            Catalog::Ready(_) => Some(model.rows.len()),
            Catalog::Pending | Catalog::Failed(_) => None,
        };
        item.browse = Some(model);
        item.installed = None;
        item.installed_n = build_installed(entry, catalog, view, staged, impact).top_count();
        item.categories = category_counts_browse(catalog.mods(), view.show_libraries);
    } else {
        let model = build_installed(entry, catalog, view, staged, impact);
        item.nav = nav_installed(&model);
        item.installed_n = model.top_count();
        item.installed = Some(model);
        item.browse = None;
        item.browse_n = cheap_browse_len(catalog, view);
        item.categories = category_counts_installed(entry, catalog, staged, view.show_libraries);
    }
    item.update_n = count_updates(&entry.card.packages, catalog.mods(), staged);
    item.view_fp = view_fp;
}

fn count_updates(packages: &[PackageCard], catalog: &[AvailableMod], staged: &StagedMods) -> usize {
    let avail = index_available(catalog);
    packages
        .iter()
        .filter(|pkg| {
            !staged.update.iter().any(|id| id == &pkg.id)
                && newer_version(pkg, avail.get(pkg.id.as_str()).copied()).is_some()
        })
        .count()
}

fn cheap_browse_len(catalog: &Catalog, view: &InstanceModsUi) -> Option<usize> {
    let Catalog::Ready(data) = catalog else {
        return None;
    };
    let mods = &data.0;
    let filter = Filter::from_view(view);
    Some(
        mods.iter()
            .filter(|item| {
                if item.kind == "library" && !filter.show_libraries {
                    return false;
                }
                filter.matches(&item.name, &item.id, &item.description, &item.categories)
            })
            .count(),
    )
}

fn category_counts_installed(
    entry: &InstanceEntry,
    catalog: &Catalog,
    staged: &StagedMods,
    show_libraries: bool,
) -> Vec<(String, usize)> {
    let avail = index_available(catalog.mods());
    let mut map: BTreeMap<String, HashSet<String>> = BTreeMap::new();
    {
        let mut note = |id: &str, cats: &[String]| {
            for cat in cats {
                if cat.is_empty() {
                    continue;
                }
                map.entry(cat.clone()).or_default().insert(id.to_owned());
            }
        };
        for pkg in &entry.card.packages {
            let removing = is_removing(staged, &pkg.id);
            if pkg.kind == "library" && !show_libraries && !removing {
                continue;
            }
            note(&pkg.id, &pkg.categories);
        }
        for spec in &staged.add {
            let id = spec_id(spec);
            if entry.card.packages.iter().any(|pkg| pkg.id == id) {
                continue;
            }
            let Some(item) = avail.get(id).copied() else {
                continue;
            };
            note(id, &item.categories);
        }
        for id in unresolved_ids(entry) {
            if entry.card.packages.iter().any(|pkg| pkg.id == id) {
                continue;
            }
            let Some(item) = avail.get(id.as_str()).copied() else {
                continue;
            };
            note(&id, &item.categories);
        }
    }
    map.into_iter().map(|(cat, ids)| (cat, ids.len())).collect()
}

fn category_counts_browse(mods: &[AvailableMod], show_libraries: bool) -> Vec<(String, usize)> {
    let mut map: BTreeMap<String, HashSet<String>> = BTreeMap::new();
    for item in mods {
        if item.kind == "library" && !show_libraries {
            continue;
        }
        for cat in &item.categories {
            if cat.is_empty() {
                continue;
            }
            map.entry(cat.clone()).or_default().insert(item.id.clone());
        }
    }
    map.into_iter().map(|(cat, ids)| (cat, ids.len())).collect()
}

fn nav_installed(model: &InstalledModel) -> Vec<NavRow> {
    let mut nav = Vec::new();
    for row in &model.adding {
        nav.push(NavRow {
            id: row.id.clone(),
            bundle: false,
            forced_open: false,
            delete: DeleteAction::UndoAdd,
        });
    }
    for row in &model.unresolved {
        nav.push(NavRow {
            id: row.id.clone(),
            bundle: false,
            forced_open: false,
            delete: if row.removing {
                DeleteAction::UndoRemove
            } else {
                DeleteAction::Remove
            },
        });
    }
    for row in &model.orphans {
        nav.push(NavRow {
            id: row.id.clone(),
            bundle: false,
            forced_open: false,
            delete: DeleteAction::UndoRemove,
        });
    }
    for row in &model.direct {
        let delete = if row.updating_to.is_some() {
            DeleteAction::UndoUpdate
        } else if row.pinned.is_some() {
            DeleteAction::UndoAdd
        } else if row.removing {
            DeleteAction::UndoRemove
        } else {
            DeleteAction::Remove
        };
        nav.push(NavRow {
            id: row.pkg.id.clone(),
            bundle: row.pkg.kind == "bundle" && row.member_total > 0,
            forced_open: row.forced,
            delete,
        });
        if row.open {
            for member in &row.members {
                nav.push(NavRow {
                    id: member.pkg.id.clone(),
                    bundle: false,
                    forced_open: false,
                    delete: if member.removing {
                        DeleteAction::UndoRemove
                    } else {
                        DeleteAction::None
                    },
                });
            }
        }
    }
    if model.deps.open {
        for row in &model.deps.rows {
            nav.push(NavRow {
                id: row.pkg.id.clone(),
                bundle: false,
                forced_open: false,
                delete: if row.removing {
                    DeleteAction::UndoRemove
                } else {
                    DeleteAction::None
                },
            });
        }
    }
    nav
}

fn nav_browse(model: &BrowseModel, sectioned: bool) -> Vec<NavRow> {
    let mut order: Vec<&BrowseRow> = Vec::new();
    if sectioned {
        order.extend(model.rows.iter().filter(|row| row.kind == "bundle"));
        order.extend(model.rows.iter().filter(|row| row.kind != "bundle"));
    } else {
        order.extend(model.rows.iter());
    }
    order
        .into_iter()
        .map(|row| NavRow {
            id: row.id.clone(),
            bundle: false,
            forced_open: false,
            delete: match row.action {
                BrowseAction::Add | BrowseAction::Indirect => DeleteAction::None,
                BrowseAction::Installed => DeleteAction::Remove,
                BrowseAction::Added => DeleteAction::UndoAdd,
                BrowseAction::Removing => DeleteAction::UndoRemove,
            },
        })
        .collect()
}

fn data_fingerprint(entry: &InstanceEntry, catalog: &Catalog) -> u64 {
    let mut hasher = DefaultHasher::new();
    entry.card.game.api.hash(&mut hasher);
    // Runs every frame: hash in the given order (a reorder only costs one rebuild).
    entry.card.packages.len().hash(&mut hasher);
    for pkg in &entry.card.packages {
        pkg.id.hash(&mut hasher);
        pkg.version.hash(&mut hasher);
        pkg.direct.hash(&mut hasher);
        pkg.kind.hash(&mut hasher);
        pkg.source.hash(&mut hasher);
        pkg.dependencies.hash(&mut hasher);
        pkg.in_bundles.hash(&mut hasher);
        pkg.name.hash(&mut hasher);
        pkg.description.hash(&mut hasher);
        pkg.categories.hash(&mut hasher);
        pkg.authors.hash(&mut hasher);
        pkg.license.hash(&mut hasher);
    }
    for id in entry.card.manifest.mods.keys() {
        id.as_str().hash(&mut hasher);
    }
    match catalog {
        Catalog::Pending => 0u8.hash(&mut hasher),
        Catalog::Failed(message) => {
            1u8.hash(&mut hasher);
            message.hash(&mut hasher);
        }
        Catalog::Ready(data) => {
            let (mods, warnings) = (&data.0, &data.1);
            2u8.hash(&mut hasher);
            warnings.hash(&mut hasher);
            mods.len().hash(&mut hasher);
            for item in mods {
                item.id.hash(&mut hasher);
                item.version.hash(&mut hasher);
                item.versions.hash(&mut hasher);
                item.kind.hash(&mut hasher);
                item.dependencies.hash(&mut hasher);
                item.members.hash(&mut hasher);
                item.pwc_api.hash(&mut hasher);
                item.name.hash(&mut hasher);
                item.description.hash(&mut hasher);
                item.categories.hash(&mut hasher);
                item.authors.hash(&mut hasher);
                item.license.hash(&mut hasher);
            }
        }
    }
    hasher.finish()
}

fn view_fingerprint(view: &InstanceModsUi) -> u64 {
    let mut hasher = DefaultHasher::new();
    view.segment.hash(&mut hasher);
    view.search.hash(&mut hasher);
    view.category.hash(&mut hasher);
    view.show_libraries.hash(&mut hasher);
    view.deps_expanded.hash(&mut hasher);
    view.deps_collapsed.hash(&mut hasher);
    hash_ids(&view.expanded, &mut hasher);
    hash_ids(&view.collapsed, &mut hasher);
    hasher.finish()
}

fn hash_ids(ids: &HashSet<String>, hasher: &mut DefaultHasher) {
    let mut sorted: Vec<&str> = ids.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    sorted.hash(hasher);
}

fn handle_mods_keys(
    ui: &mut Ui,
    modal: bool,
    narrow: bool,
    instance: &str,
    view: &mut InstanceModsUi,
    cache: Option<&ModelCache>,
    keys: &mut Click,
) {
    if modal || view.impact_open {
        return;
    }
    let focused = ui.memory(|mem| mem.focused().is_some());
    if focused || egui::Popup::is_any_open(ui.ctx()) {
        return;
    }
    let pressed = |ui: &mut Ui, key| ui.input_mut(|input| input.consume_key(Modifiers::NONE, key));
    if pressed(ui, Key::Slash) {
        view.focus_search = true;
    }
    // Escape and Enter only mean something with a selection; otherwise leave them to the app.
    if view.selected.is_some() && pressed(ui, Key::Escape) {
        view.selected = None;
        view.details_open = false;
        view.key_nav = false;
    }
    let nav = cache
        .filter(|item| item.instance == instance)
        .map(|item| item.nav.as_slice())
        .unwrap_or(&[]);
    if nav.is_empty() {
        return;
    }
    let arrows = !(narrow && view.details_open);
    if arrows && pressed(ui, Key::ArrowDown) {
        move_selection(view, nav, true);
    }
    if arrows && pressed(ui, Key::ArrowUp) {
        move_selection(view, nav, false);
    }
    if view.selected.is_some() && pressed(ui, Key::Enter) {
        view.details_open = true;
    }
    if view.selected.is_some() && pressed(ui, Key::Delete) {
        set_click(keys, delete_click(nav, view.selected.as_deref()));
    }
    if view.selected.is_some()
        && pressed(ui, Key::Space)
        && let Some(row) = nav
            .iter()
            .find(|row| Some(row.id.as_str()) == view.selected.as_deref())
        && row.bundle
    {
        let id = row.id.clone();
        let forced = row.forced_open;
        if forced {
            toggle_expand(&mut view.collapsed, &id);
        } else {
            toggle_expand(&mut view.expanded, &id);
        }
    }
}

fn move_selection(view: &mut InstanceModsUi, nav: &[NavRow], down: bool) {
    if nav.is_empty() {
        return;
    }
    let current = view
        .selected
        .as_ref()
        .and_then(|id| nav.iter().position(|row| &row.id == id));
    let next = match (current, down) {
        (Some(index), true) => (index + 1).min(nav.len() - 1),
        (Some(index), false) => index.saturating_sub(1),
        (None, _) => 0,
    };
    view.selected = Some(nav[next].id.clone());
    view.key_nav = true;
    view.pending_scroll = true;
}

fn delete_click(nav: &[NavRow], selected: Option<&str>) -> Click {
    let Some(id) = selected else {
        return Click::None;
    };
    let Some(row) = nav.iter().find(|row| row.id == id) else {
        return Click::None;
    };
    match row.delete {
        DeleteAction::None => Click::None,
        DeleteAction::Remove => Click::Remove(id.to_owned()),
        DeleteAction::UndoAdd => Click::UndoAdd(id.to_owned()),
        DeleteAction::UndoRemove => Click::UndoRemove(id.to_owned()),
        DeleteAction::UndoUpdate => Click::UndoUpdate(id.to_owned()),
    }
}

fn note_hit(
    view: &mut InstanceModsUi,
    id: &str,
    hit: RowHit,
    undo: UndoKind,
    push: bool,
    click: &mut Click,
) {
    if hit == RowHit::Body {
        select_row(view, id, push);
    }
    set_click(click, hit_click(hit, id, undo));
}

fn select_row(view: &mut InstanceModsUi, id: &str, push: bool) {
    view.key_nav = false;
    if view.selected.as_deref() == Some(id) {
        view.selected = None;
        view.details_open = false;
        return;
    }
    view.selected = Some(id.to_owned());
    if push {
        view.details_open = true;
    }
}
#[derive(Clone, Debug)]
struct Detail {
    id: String,
    name: String,
    kind: String,
    description: String,
    installed_version: Option<String>,
    catalogue_version: String,
    authors: String,
    license: String,
    categories: Vec<String>,
    source: String,
    mod_api: String,
    installed: bool,
    direct: bool,
    unresolved: bool,
    adding: bool,
    removing: bool,
    updating_to: Option<String>,
    pinned: Option<String>,
    newer: Option<String>,
    compat: Option<CompatNote>,
    included_by: Option<String>,
    required_note: Option<String>,
    requires: Vec<RevRef>,
    required_by: Vec<RevRef>,
    bundles: Vec<RevRef>,
    members: Vec<RevRef>,
    versions: Vec<String>,
}

fn lookup_ref(
    id: &str,
    by_pkg: &HashMap<&str, &PackageCard>,
    avail: &HashMap<&str, &AvailableMod>,
) -> RevRef {
    if let Some(pkg) = by_pkg.get(id).copied() {
        rev_from_pkg(pkg)
    } else if let Some(item) = avail.get(id).copied() {
        rev_from_available(item)
    } else {
        RevRef {
            id: id.to_owned(),
            name: id.to_owned(),
            version: String::new(),
            kind: "mod".to_owned(),
        }
    }
}

fn source_label(source: &str) -> String {
    if source.is_empty() {
        return "—".to_owned();
    }
    match source.split_once(':') {
        Some((_, rest)) if !rest.is_empty() => rest.to_owned(),
        _ => source.to_owned(),
    }
}

fn joined_or_dash(items: &[String]) -> String {
    if items.is_empty() {
        "—".to_owned()
    } else {
        items.join(", ")
    }
}

fn mod_api_line(req: Option<&str>, have: Option<&str>) -> String {
    match (req, have) {
        (Some(req), Some(have)) => format!("{req} · instance has {have}"),
        (Some(req), None) => req.to_owned(),
        (None, Some(have)) => format!("any · instance has {have}"),
        (None, None) => "—".to_owned(),
    }
}

fn resolve_detail(
    id: &str,
    entry: &InstanceEntry,
    catalog: &Catalog,
    staged: &StagedMods,
    cache: &Option<ModelCache>,
) -> Detail {
    let avail = index_available(catalog.mods());
    let by_pkg = index_packages(&entry.card.packages);
    let pkg = by_pkg.get(id).copied();
    let item = avail.get(id).copied();
    let installed = pkg.is_some();
    let unresolved = !installed && unresolved_ids(entry).iter().any(|missing| missing == id);
    let mut members: Vec<RevRef> = entry
        .card
        .packages
        .iter()
        .filter(|candidate| candidate.in_bundles.iter().any(|bundle| bundle == id))
        .map(rev_from_pkg)
        .collect();
    if members.is_empty()
        && let Some(item) = item
        && item.kind == "bundle"
    {
        for member in &item.members {
            members.push(lookup_ref(member, &by_pkg, &avail));
        }
    }
    members.sort_by_cached_key(|member| (member.name.to_lowercase(), member.id.clone()));
    let member_ids: HashSet<&str> = members.iter().map(|member| member.id.as_str()).collect();
    let dep_ids: Vec<&str> = if let Some(pkg) = pkg {
        pkg.dependencies.iter().map(String::as_str).collect()
    } else if let Some(item) = item {
        item.dependencies.iter().map(String::as_str).collect()
    } else {
        Vec::new()
    };
    let mut requires: Vec<RevRef> = dep_ids
        .into_iter()
        .filter(|dep| !member_ids.contains(dep))
        .map(|dep| lookup_ref(dep, &by_pkg, &avail))
        .collect();
    requires.sort_by_cached_key(|dep| (dep.name.to_lowercase(), dep.id.clone()));
    let bundles: Vec<RevRef> = pkg
        .map(|pkg| {
            pkg.in_bundles
                .iter()
                .map(|bundle| lookup_ref(bundle, &by_pkg, &avail))
                .collect()
        })
        .unwrap_or_default();
    // A bundle that includes this package gets its own "In bundle" link; do not list it twice.
    let required_by: Vec<RevRef> = cache
        .as_ref()
        .filter(|cached| cached.instance == entry.card.name)
        .and_then(|cached| cached.rev.get(id))
        .map(|refs| {
            refs.iter()
                .filter(|referrer| bundles.iter().all(|bundle| bundle.id != referrer.id))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    let included_by = bundles.first().map(|bundle| bundle.name.clone());
    let required_note = included_by
        .clone()
        .or_else(|| required_by.first().map(|referrer| referrer.name.clone()));
    let compat = item.and_then(|item| compat_note(item, entry.card.game.api.as_deref()));
    let name = pkg
        .map(|pkg| pkg.name.clone())
        .or_else(|| item.map(|item| item.name.clone()))
        .unwrap_or_else(|| id.to_owned());
    let kind = pkg
        .map(|pkg| pkg.kind.clone())
        .or_else(|| item.map(|item| item.kind.clone()))
        .unwrap_or_else(|| "mod".to_owned());
    let description = pkg
        .map(|pkg| pkg.description.clone())
        .or_else(|| item.map(|item| item.description.clone()))
        .unwrap_or_default();
    let categories = pkg
        .map(|pkg| pkg.categories.clone())
        .or_else(|| item.map(|item| item.categories.clone()))
        .unwrap_or_default();
    let authors = joined_or_dash(
        pkg.map(|pkg| pkg.authors.as_slice())
            .or_else(|| item.map(|item| item.authors.as_slice()))
            .unwrap_or(&[]),
    );
    let license = pkg
        .map(|pkg| pkg.license.clone())
        .or_else(|| item.map(|item| item.license.clone()))
        .filter(|license| !license.is_empty())
        .unwrap_or_else(|| "—".to_owned());
    let source = source_label(pkg.map(|pkg| pkg.source.as_str()).unwrap_or(""));
    let updating_to = staged
        .update
        .iter()
        .any(|staged_id| staged_id == id)
        .then(|| {
            item.map(|item| item.version.clone())
                .unwrap_or_else(|| "newest".to_owned())
        });
    let mod_api = if let Some(note) = &compat {
        format!("needs {} · instance has {}", note.req, note.have)
    } else {
        mod_api_line(
            item.and_then(|item| item.pwc_api.as_deref()),
            entry.card.game.api.as_deref(),
        )
    };
    Detail {
        mod_api,
        catalogue_version: item.map(|item| item.version.clone()).unwrap_or_default(),
        installed_version: pkg.map(|pkg| pkg.version.clone()),
        newer: pkg.and_then(|pkg| newer_version(pkg, item)),
        pinned: pin_label(staged, id),
        adding: is_adding(staged, id),
        removing: is_removing(staged, id),
        direct: pkg.is_some_and(|pkg| pkg.direct),
        versions: item.map(|item| item.versions.clone()).unwrap_or_default(),
        id: id.to_owned(),
        name,
        kind,
        description,
        authors,
        license,
        categories,
        source,
        installed,
        unresolved,
        updating_to,
        compat,
        included_by,
        required_note,
        requires,
        required_by,
        bundles,
        members,
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_details(
    ui: &mut Ui,
    rect: Rect,
    back: bool,
    view: &mut InstanceModsUi,
    entry: &InstanceEntry,
    catalog: &Catalog,
    staged: &StagedMods,
    cache: &mut Option<ModelCache>,
) -> Click {
    let Some(selected) = view.selected.clone() else {
        return Click::None;
    };
    let p = theme::palette(ui.ctx());
    let mut top = rect.top();
    if back {
        let bar = Rect::from_min_size(rect.min, vec2(rect.width(), 28.0));
        let mut row = ui.new_child(
            UiBuilder::new()
                .max_rect(bar)
                .layout(Layout::left_to_right(Align::Center)),
        );
        row.set_clip_rect(bar);
        if widgets::Button::new("← Back")
            .variant(Variant::Ghost)
            .size(Size::Small)
            .show(&mut row)
            .clicked()
        {
            view.details_open = false;
        }
        top = bar.bottom() + 8.0;
    }
    if top >= rect.bottom() - 8.0 {
        return Click::None;
    }
    let card = Rect::from_min_max(pos2(rect.left(), top), rect.max);
    ui.painter().rect(
        card,
        12.0,
        p.bg_card,
        Stroke::new(1.0, p.line_soft),
        egui::StrokeKind::Inside,
    );
    let inner = card.shrink(16.0);
    if inner.width() < 8.0 || inner.height() < 8.0 {
        return Click::None;
    }
    let fresh = cache
        .as_ref()
        .and_then(|item| item.detail.as_ref())
        .is_some_and(|detail| detail.id == selected);
    if !fresh {
        let detail = resolve_detail(&selected, entry, catalog, staged, cache);
        if let Some(item) = cache.as_mut() {
            item.detail = Some(detail);
        }
    }
    let Some(detail) = cache.as_ref().and_then(|item| item.detail.as_ref()) else {
        return Click::None;
    };
    let mut body = ui.new_child(
        UiBuilder::new()
            .max_rect(inner)
            .layout(Layout::top_down(Align::Min)),
    );
    body.set_clip_rect(inner);
    egui::ScrollArea::vertical()
        .id_salt(("mods-detail", selected))
        .max_height(inner.height())
        .auto_shrink([false, false])
        .show(&mut body, |ui| {
            ui.set_width(ui.available_width());
            paint_detail_body(ui, detail, view, back)
        })
        .inner
}

fn paint_detail_body(
    ui: &mut Ui,
    detail: &Detail,
    view: &mut InstanceModsUi,
    pushed: bool,
) -> Click {
    let p = theme::palette(ui.ctx());
    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
    paint_detail_header(ui, detail, &p);
    ui.add_space(10.0);
    paint_detail_pills(ui, detail, &p);
    ui.add_space(12.0);
    let mut click = paint_detail_actions(ui, detail);
    if !detail.description.is_empty() {
        ui.add_space(10.0);
        ui.label(
            egui::RichText::new(&detail.description)
                .font(theme::regular(14.0))
                .color(p.text),
        );
    }
    ui.add_space(10.0);
    paint_detail_facts(ui, detail, &p);
    // Pinning a package another mod pulls in would make it direct; that is not this control.
    let pinnable = detail.direct || (!detail.installed && !detail.unresolved);
    if pinnable && !detail.versions.is_empty() && !detail.removing {
        ui.add_space(10.0);
        set_click(&mut click, paint_version_picker(ui, detail, &p));
    }
    paint_detail_links(ui, "Requires", &detail.requires, view, pushed, &p);
    paint_detail_links(ui, "Required by", &detail.required_by, view, pushed, &p);
    if !detail.bundles.is_empty() {
        ui.add_space(10.0);
        for bundle in &detail.bundles {
            let label = format!("In bundle: {}", bundle.name);
            if detail_link(ui, &bundle.kind, &label, "", &p) {
                open_detail(view, &bundle.id, pushed);
            }
        }
    }
    if !detail.members.is_empty() {
        let heading = format!("Members ({})", detail.members.len());
        paint_detail_links(ui, &heading, &detail.members, view, pushed, &p);
    }
    ui.add_space(4.0);
    click
}

fn paint_detail_header(ui: &mut Ui, detail: &Detail, p: &Palette) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        let (icon_rect, _) = ui.allocate_exact_size(Vec2::splat(32.0), Sense::hover());
        let (icon, color) = kind_mark(&detail.kind, p);
        widgets::paint_icon(ui.painter(), icon_rect, icon, color, 0.0);
        ui.vertical(|ui| {
            ui.set_width(ui.available_width());
            ui.label(
                egui::RichText::new(&detail.name)
                    .font(theme::semibold(20.0))
                    .color(p.text_bright),
            );
            ui.label(
                egui::RichText::new(&detail.id)
                    .font(theme::mono(12.0))
                    .color(p.text_muted),
            );
        });
    });
}

/// Status and compatibility pills, then the version and any newer one, wrapping as needed.
fn paint_detail_pills(ui: &mut Ui, detail: &Detail, p: &Palette) {
    let (label, color) = detail_status(detail, p);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(8.0, 6.0);
        widgets::status_pill(ui, &label, color, false);
        if let Some(note) = &detail.compat {
            let response = widgets::pill(ui, &note.pill, p.warn, theme::with_alpha(p.warn, 38));
            let _ = response.on_hover_text(&note.tip);
        }
        let version = match &detail.installed_version {
            Some(version) => format!("{version} installed"),
            None => detail.catalogue_version.clone(),
        };
        if !version.is_empty() {
            ui.label(
                egui::RichText::new(version)
                    .font(theme::regular(13.0))
                    .color(p.text_muted),
            );
        }
        if let Some(newer) = &detail.newer {
            widgets::pill(ui, &format!("{newer} available"), p.arc, p.arc_tint);
        }
    });
}

fn detail_status(detail: &Detail, p: &Palette) -> (String, Color32) {
    if detail.updating_to.is_some() {
        ("Will update".to_owned(), p.arc)
    } else if detail.pinned.is_some() {
        ("Pinned".to_owned(), p.arc)
    } else if detail.removing {
        ("Will be removed".to_owned(), p.bad)
    } else if detail.adding {
        ("Will be added".to_owned(), p.volt)
    } else if detail.unresolved {
        ("Unresolved".to_owned(), p.bad)
    } else if detail.installed && !detail.direct {
        let name = detail
            .required_note
            .clone()
            .unwrap_or_else(|| "another mod".to_owned());
        (format!("Required by {name}"), p.text_muted)
    } else if detail.installed {
        ("Installed".to_owned(), p.ok)
    } else {
        ("Not installed".to_owned(), p.neutral)
    }
}

fn paint_detail_actions(ui: &mut Ui, detail: &Detail) -> Click {
    let mut click = Click::None;
    let width = ui.available_width();
    // Update sits above Remove: the safe action first, the destructive one last.
    if let Some(newer) = &detail.newer
        && detail.updating_to.is_none()
        && detail.pinned.is_none()
        && !detail.removing
    {
        let label = format!("Update to {newer}");
        if wide_button(ui, &label, Variant::Secondary, true, None, width) {
            click = Click::Update(detail.id.clone());
        }
        ui.add_space(6.0);
    }
    if detail.updating_to.is_some() || detail.pinned.is_some() || detail.removing || detail.adding {
        if wide_button(ui, "Undo", Variant::Ghost, true, None, width) {
            set_click(&mut click, detail_undo(detail));
        }
    } else if !detail.installed && !detail.unresolved {
        if wide_button(ui, "+ Add", Variant::Secondary, true, None, width) {
            set_click(&mut click, Click::Add(detail.id.clone()));
        }
    } else if detail.installed && !detail.direct {
        let tip = remove_block_tip(detail);
        let _ = wide_button(ui, "Remove", Variant::Danger, false, Some(&tip), width);
        ui.add_space(4.0);
        let p = theme::palette(ui.ctx());
        ui.label(
            egui::RichText::new(tip)
                .font(theme::regular(12.0))
                .color(p.text_muted),
        );
    } else if wide_button(ui, "Remove", Variant::Danger, true, None, width) {
        set_click(&mut click, Click::Remove(detail.id.clone()));
    }
    click
}

fn detail_undo(detail: &Detail) -> Click {
    if detail.updating_to.is_some() {
        Click::UndoUpdate(detail.id.clone())
    } else if detail.pinned.is_some() || detail.adding {
        Click::UndoAdd(detail.id.clone())
    } else {
        Click::UndoRemove(detail.id.clone())
    }
}

fn remove_block_tip(detail: &Detail) -> String {
    if let Some(bundle) = &detail.included_by {
        format!("Included by {bundle}; remove the bundle")
    } else if let Some(name) = &detail.required_note {
        format!("Required by {name}; remove that mod")
    } else {
        "Required by another mod; remove that mod".to_owned()
    }
}

fn wide_button(
    ui: &mut Ui,
    label: &str,
    variant: Variant,
    enabled: bool,
    tip: Option<&str>,
    width: f32,
) -> bool {
    let mut button = widgets::Button::new(label)
        .variant(variant)
        .size(Size::Medium)
        .min_width(width)
        .enabled(enabled);
    if let Some(tip) = tip {
        button = button.on_disabled_hover_text(tip);
    }
    let response = button.show(ui);
    // Full width, a borderless ghost reads as a stray label. Outline it like a field.
    let p = theme::palette(ui.ctx());
    let outline = match variant {
        Variant::Danger if enabled => Some(theme::with_alpha(p.bad, 90)),
        Variant::Danger | Variant::Ghost => Some(p.line),
        Variant::Primary | Variant::Secondary | Variant::DangerPrimary => None,
    };
    if let Some(color) = outline {
        ui.painter().rect_stroke(
            response.rect,
            8.0,
            Stroke::new(1.0, color),
            egui::StrokeKind::Inside,
        );
    }
    response.clicked()
}

fn paint_detail_facts(ui: &mut Ui, detail: &Detail, p: &Palette) {
    fact_row(ui, "Authors", &detail.authors, p);
    ui.add_space(4.0);
    fact_row(ui, "Licence", &detail.license, p);
    ui.add_space(4.0);
    fact_categories(ui, &detail.categories, p);
    if detail.installed {
        ui.add_space(4.0);
        fact_row(ui, "Source", &detail.source, p);
    }
    ui.add_space(4.0);
    fact_row(ui, "Mod API", &detail.mod_api, p);
}

fn fact_row(ui: &mut Ui, key: &str, value: &str, p: &Palette) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let (key_rect, _) = ui.allocate_exact_size(vec2(76.0, 18.0), Sense::hover());
        let galley =
            ui.painter()
                .layout_no_wrap(key.to_owned(), theme::regular(12.0), p.text_muted);
        let y = key_rect.center().y - galley.size().y * 0.5;
        ui.painter()
            .galley(pos2(key_rect.left(), y), galley, p.text_muted);
        let value_w = ui.available_width().max(1.0);
        let mut job = egui::text::LayoutJob::default();
        job.wrap.max_width = value_w;
        job.wrap.max_rows = 3;
        job.append(value, 0.0, text_format(theme::regular(12.0), p.text, false));
        let galley = ui.painter().layout_job(job);
        let (rect, _) =
            ui.allocate_exact_size(vec2(value_w, galley.size().y.max(18.0)), Sense::hover());
        let y = rect.center().y - galley.size().y * 0.5;
        ui.painter().galley(pos2(rect.left(), y), galley, p.text);
    });
}

fn fact_categories(ui: &mut Ui, categories: &[String], p: &Palette) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let (key_rect, _) = ui.allocate_exact_size(vec2(76.0, 20.0), Sense::hover());
        let galley = ui.painter().layout_no_wrap(
            "Categories".to_owned(),
            theme::regular(12.0),
            p.text_muted,
        );
        let y = key_rect.center().y - galley.size().y * 0.5;
        ui.painter()
            .galley(pos2(key_rect.left(), y), galley, p.text_muted);
        if categories.is_empty() {
            ui.label(
                egui::RichText::new("—")
                    .font(theme::regular(12.0))
                    .color(p.text),
            );
        } else {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
                for cat in categories {
                    widgets::pill(ui, cat, p.text, p.bg_raised);
                }
            });
        }
    });
}

fn paint_version_picker(ui: &mut Ui, detail: &Detail, p: &Palette) -> Click {
    ui.label(
        egui::RichText::new("Version")
            .font(theme::semibold(12.0))
            .color(p.text_muted),
    );
    ui.add_space(4.0);
    let mut choice = detail.pinned.clone();
    let before = choice.clone();
    let selected = match &choice {
        Some(version) if version == "*" => "Follow newest (*)".to_owned(),
        Some(version) => version.clone(),
        None => "Pin version".to_owned(),
    };
    egui::ComboBox::from_id_salt(("mod-pin", detail.id.as_str()))
        .width(ui.available_width())
        .truncate()
        .selected_text(
            egui::RichText::new(selected)
                .font(theme::regular(13.0))
                .color(p.text),
        )
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut choice, Some("*".to_owned()), "Follow newest (*)");
            for version in &detail.versions {
                ui.selectable_value(&mut choice, Some(version.clone()), version);
            }
        });
    if choice == before {
        return Click::None;
    }
    match choice {
        Some(version) if version == "*" => Click::Pin(detail.id.clone()),
        Some(version) => Click::Pin(format!("{}@={version}", detail.id)),
        None => Click::None,
    }
}

fn paint_detail_links(
    ui: &mut Ui,
    heading: &str,
    rows: &[RevRef],
    view: &mut InstanceModsUi,
    pushed: bool,
    p: &Palette,
) {
    if rows.is_empty() {
        return;
    }
    ui.add_space(10.0);
    ui.label(
        egui::RichText::new(heading)
            .font(theme::semibold(12.0))
            .color(p.text_muted),
    );
    ui.add_space(2.0);
    for row in rows {
        if detail_link(ui, &row.kind, &row.name, &row.version, p) {
            open_detail(view, &row.id, pushed);
        }
    }
}

fn detail_link(ui: &mut Ui, kind: &str, name: &str, version: &str, p: &Palette) -> bool {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(rect, 6.0, p.wash_hover);
    }
    let (icon, color) = kind_mark(kind, p);
    let icon_rect =
        Rect::from_center_size(pos2(rect.left() + 10.0, rect.center().y), Vec2::splat(16.0));
    widgets::paint_icon(ui.painter(), icon_rect, icon, color, 0.0);
    let text_left = rect.left() + 26.0;
    let text_right = rect.right();
    if text_right > text_left + 8.0 {
        let mut job = egui::text::LayoutJob::default();
        job.wrap.max_width = text_right - text_left;
        job.wrap.max_rows = 1;
        job.wrap.break_anywhere = true;
        job.append(name, 0.0, text_format(theme::semibold(13.0), p.text, false));
        if !version.is_empty() {
            job.append(
                &format!("  {version}"),
                0.0,
                text_format(theme::regular(12.0), p.text_muted, false),
            );
        }
        let galley = ui.painter().layout_job(job);
        let y = rect.center().y - galley.size().y * 0.5;
        ui.painter().galley(pos2(text_left, y), galley, p.text);
    }
    let _ = response.clone().on_hover_cursor(CursorIcon::PointingHand);
    response.clicked()
}

fn open_detail(view: &mut InstanceModsUi, id: &str, pushed: bool) {
    view.selected = Some(id.to_owned());
    view.key_nav = false;
    if pushed {
        view.details_open = true;
    }
}

#[cfg(test)]
mod tests {
    use super::StagedMods;
    use super::predict;
    use crate::gui::backend::{Backend, DemoBackend, Loaded};

    fn sorted_ids(lines: &[super::DiffLine]) -> Vec<String> {
        let mut ids: Vec<String> = lines.iter().map(|line| line.id.clone()).collect();
        ids.sort();
        ids
    }

    #[test]
    fn predicted_diff_on_modded_demo() {
        let mut backend = DemoBackend::new();
        let entry = backend
            .instances()
            .iter()
            .find(|entry| entry.card.name == "modded")
            .cloned()
            .expect("modded instance");
        let mods = match backend.available_mods("modded") {
            Loaded::Ready(data) => data.0.clone(),
            Loaded::Idle => panic!("catalogue idle"),
            Loaded::Loading => panic!("catalogue loading"),
            Loaded::Err(message) => panic!("catalogue failed: {message}"),
        };
        let mut staged = StagedMods::default();
        staged.add.push("pwc.rivers".to_owned());
        staged.remove.push("pwc.soundscape".to_owned());
        staged.remove.push("pwc.ambient-audio".to_owned());
        let impact = predict(
            &entry.card.packages,
            &mods,
            entry.card.game.api.as_deref(),
            &staged,
        );
        assert_eq!(
            sorted_ids(&impact.adds),
            vec!["pwc.material-names".to_owned(), "pwc.rivers".to_owned()]
        );
        let rivers = impact
            .adds
            .iter()
            .find(|line| line.id == "pwc.rivers")
            .expect("rivers");
        assert_eq!(rivers.name, "Rivers");
        assert_eq!(rivers.version, "0.5.0");
        assert!(!rivers.library);
        assert!(rivers.needed_by.is_empty());
        let names = impact
            .adds
            .iter()
            .find(|line| line.id == "pwc.material-names")
            .expect("material names");
        assert_eq!(names.name, "Material Names");
        assert_eq!(names.version, "0.4.0");
        assert!(names.library);
        assert_eq!(names.needed_by, vec!["pwc.rivers".to_owned()]);
        assert_eq!(
            sorted_ids(&impact.removes),
            vec!["pwc.ambient-audio".to_owned(), "pwc.soundscape".to_owned()]
        );
        let recordings = impact
            .removes
            .iter()
            .find(|line| line.id == "pwc.soundscape")
            .expect("field recordings");
        assert_eq!(recordings.name, "Field Recordings");
        assert_eq!(recordings.version, "1.0.1");
        assert_eq!(
            sorted_ids(&impact.orphans),
            vec!["pwc.audio-core".to_owned()]
        );
        let core = &impact.orphans[0];
        assert_eq!(core.name, "Audio Core");
        assert_eq!(core.version, "0.2.0");
        assert!(core.library);
        assert!(
            impact
                .adds
                .iter()
                .chain(&impact.removes)
                .chain(&impact.orphans)
                .all(|line| line.id != "pwc.geology")
        );
        assert_eq!(impact.incompatible, 0);
        assert_eq!(
            super::add_pill_text("pwc.rivers", &impact),
            "Will be added · Material Names (library)"
        );
        let avail = super::index_available(&mods);
        let by_pkg = super::index_packages(&entry.card.packages);
        let facts = super::Facts {
            impact: &impact,
            game_api: entry.card.game.api.as_deref(),
            avail: &avail,
            by_pkg: &by_pkg,
        };
        assert_eq!(super::unused_count("pwc.soundscape", &facts), 1);
        assert_eq!(super::unused_count("pwc.ambient-audio", &facts), 1);
        let counts = super::bar_counts(&staged, &entry.card.packages, Some(&impact));
        assert_eq!(counts.mods, 1);
        assert_eq!(counts.deps, 1);
        assert_eq!(counts.leaving, 2);
        assert_eq!(counts.unused, 1);
        assert_eq!(counts.updates, 0);

        let mut only_sound = StagedMods::default();
        only_sound.remove.push("pwc.soundscape".to_owned());
        let alone = predict(
            &entry.card.packages,
            &mods,
            entry.card.game.api.as_deref(),
            &only_sound,
        );
        assert_eq!(
            sorted_ids(&alone.removes),
            vec!["pwc.soundscape".to_owned()]
        );
        assert!(alone.orphans.iter().all(|line| line.id != "pwc.audio-core"));
        assert!(alone.removes.iter().all(|line| line.id != "pwc.audio-core"));

        let mut rail = StagedMods::default();
        rail.add.push("pwc.rail-network".to_owned());
        let incompatible = predict(
            &entry.card.packages,
            &mods,
            entry.card.game.api.as_deref(),
            &rail,
        );
        assert_eq!(incompatible.incompatible, 1);
    }
}
