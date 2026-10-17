//! The real backend: a thin front end over `watt-core`. Every slow call runs on a background
//! thread; results are delivered through a channel and picked up by [`RealBackend::tick`].

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use watt_core::catalog::{self, AvailableMod};
use watt_core::doctor::{self, Check};
use watt_core::events::RepoLog;
use watt_core::git::{self, BranchInfo};
use watt_core::meta::{self, Meta};
use watt_core::ops::OpRequest;
use watt_core::paths::{Paths, Repo};
use watt_core::settings::Settings;
use watt_core::status;

use super::{Backend, InstanceEntry, Loaded, MetaEdit, RunningOp, read_log_tail};

/// `catalog::scan_instances` + `status::compute` for every instance, plus the unreadable ones.
type CatalogResult = Result<(Vec<Arc<InstanceEntry>>, Vec<(String, String)>), String>;
/// `catalog::available_mods`.
type ModsResult = Result<(Vec<AvailableMod>, Vec<String>), String>;
/// `(name, busy, running pid)` from [`status::liveness`].
type LivenessResult = Vec<(String, bool, Option<u32>)>;

/// A background job's channel.
///
/// `RealBackend` and everything below are only exercised when `WATT_START_DEMO` is unset; the
/// snapshot tests always use [`super::DemoBackend`], so these are legitimately unused there
/// (the dead-code lint cannot see across the `WATT_START_DEMO` branch in `App::new`).
#[allow(dead_code)]
struct Job<T> {
    rx: Receiver<T>,
}

#[allow(dead_code)]
impl<T: Send + 'static> Job<T> {
    /// Run `f`, then wake egui if a context has been installed. The context is read when the
    /// job finishes, not when it starts, so a scan that began before the first frame still wakes.
    fn spawn(
        ctx: Arc<Mutex<Option<egui::Context>>>,
        f: impl FnOnce() -> T + Send + 'static,
    ) -> Self {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let value = f();
            let _ = tx.send(value);
            if let Some(ctx) = ctx.lock().unwrap_or_else(|err| err.into_inner()).as_ref() {
                ctx.request_repaint();
            }
        });
        Self { rx }
    }

    fn poll(&self) -> Option<T> {
        self.rx.try_recv().ok()
    }
}

/// Mod catalogue load tagged with the instance and epoch it was started for.
#[allow(dead_code)]
struct ModsJob {
    instance: String,
    epoch: u64,
    job: Job<ModsResult>,
}

/// One cached on-disk log tail.
#[allow(dead_code)]
struct LogCache {
    modified: SystemTime,
    len: u64,
    lines: Arc<Vec<String>>,
}

/// In-flight tail read, keyed by the metadata it was started with.
#[allow(dead_code)]
struct LogJob {
    path: PathBuf,
    modified: SystemTime,
    len: u64,
    job: Job<Arc<Vec<String>>>,
}

/// The real backend.
#[allow(dead_code)]
pub struct RealBackend {
    paths: Paths,
    settings: Settings,
    ui_ctx: Arc<Mutex<Option<egui::Context>>>,
    instances: Vec<Arc<InstanceEntry>>,
    load_errors: Vec<(String, String)>,
    updates: BTreeMap<String, Vec<RepoLog>>,
    refresh_job: Option<Job<CatalogResult>>,
    /// `refresh` was asked for while a scan was already running. Start another when it lands.
    refresh_again: bool,
    liveness_job: Option<Job<LivenessResult>>,
    mods_instance: Option<String>,
    /// Bumped by [`RealBackend::invalidate_mods`]. A job is tagged with the value at spawn.
    mods_epoch: u64,
    /// Epoch of the catalogue currently in `mods`, when it is [`Loaded::Ready`].
    mods_loaded_epoch: u64,
    mods: Loaded<Arc<(Vec<AvailableMod>, Vec<String>)>>,
    mods_job: Option<ModsJob>,
    /// The catalogue should reload once the in-flight catalog scan lands, so it sees the new card.
    reload_mods_after_refresh: bool,
    branches: Loaded<Vec<BranchInfo>>,
    branches_job: Option<Job<Result<Vec<BranchInfo>, String>>>,
    doctor: Loaded<Vec<Check>>,
    doctor_job: Option<Job<Vec<Check>>>,
    /// Launcher self-update. Not an op; see [`super::update`].
    update: super::update::LauncherUpdate,
    log_tails: HashMap<PathBuf, LogCache>,
    log_job: Option<LogJob>,
}

#[allow(dead_code)]
impl RealBackend {
    /// Load settings and do a first (background) catalog scan.
    pub fn new() -> anyhow::Result<Self> {
        let paths = Paths::from_env()?;
        let settings = Settings::load(&paths.settings_file)?;
        let check_launcher = settings.check_launcher_updates;
        let mut backend = Self {
            paths,
            settings,
            ui_ctx: Arc::new(Mutex::new(None)),
            instances: Vec::new(),
            load_errors: Vec::new(),
            updates: BTreeMap::new(),
            refresh_job: None,
            refresh_again: false,
            liveness_job: None,
            mods_instance: None,
            mods_epoch: 0,
            mods_loaded_epoch: 0,
            mods: Loaded::Idle,
            mods_job: None,
            reload_mods_after_refresh: false,
            branches: Loaded::Idle,
            branches_job: None,
            doctor: Loaded::Idle,
            doctor_job: None,
            update: super::update::LauncherUpdate::new(),
            log_tails: HashMap::new(),
            log_job: None,
        };
        let paths = backend.paths.clone();
        backend.update.start_check(&paths, false, check_launcher);
        backend.refresh();
        Ok(backend)
    }

    /// Start a mod-catalogue load for `instance` at the current epoch.
    ///
    /// `fresh` replaces whatever is showing (an instance switch). Otherwise the previous
    /// [`Loaded::Ready`] stays up until the new scan lands.
    fn begin_mods_load(&mut self, instance: &str, fresh: bool) {
        if fresh {
            self.mods = Loaded::Loading;
        }
        let Some(card) = self
            .instances
            .iter()
            .find(|entry| entry.card.name == instance)
            .map(|entry| entry.card.clone())
        else {
            self.mods = Loaded::Err("instance not found".to_owned());
            self.mods_job = None;
            return;
        };
        let epoch = self.mods_epoch;
        let paths = self.paths.clone();
        let settings = self.settings.clone();
        let ctx = Arc::clone(&self.ui_ctx);
        let instance_name = instance.to_owned();
        self.mods_job = Some(ModsJob {
            instance: instance_name,
            epoch,
            job: Job::spawn(ctx, move || {
                catalog::available_mods(&paths, &settings, &card).map_err(|e| e.to_string())
            }),
        });
    }
}

#[allow(dead_code)]
fn load_catalog(paths: &Paths) -> CatalogResult {
    let (cards, errors) = catalog::scan_instances(paths).map_err(|e| e.to_string())?;
    let mut entries = Vec::with_capacity(cards.len());
    for card in cards {
        let status = status::compute(paths, &card).map_err(|e| e.to_string())?;
        entries.push(Arc::new(InstanceEntry { card, status }));
    }
    Ok((entries, errors))
}

impl Backend for RealBackend {
    fn settings(&self) -> &Settings {
        &self.settings
    }

    fn save_settings(&mut self, settings: Settings) {
        self.settings = settings;
        if let Err(e) = self.settings.save(&self.paths.settings_file) {
            self.load_errors
                .push(("settings".to_owned(), e.to_string()));
        }
        self.invalidate_mods();
    }

    fn tick(&mut self) -> bool {
        let mut changed = false;
        // Poll into a local first. A let-chain would keep `&self.*_job` borrowed into the body,
        // and the body needs to take the job.
        let refresh_result = self.refresh_job.as_ref().and_then(Job::poll);
        if let Some(result) = refresh_result {
            self.refresh_job = None;
            match result {
                Ok((instances, errors)) => {
                    self.instances = instances;
                    self.load_errors = errors;
                }
                Err(e) => self.load_errors.push(("catalog".to_owned(), e)),
            }
            changed = true;
            if self.refresh_again {
                self.refresh();
            } else if self.reload_mods_after_refresh {
                self.reload_mods_after_refresh = false;
                self.mods_epoch = self.mods_epoch.wrapping_add(1);
                if let Some(instance) = self.mods_instance.clone() {
                    self.begin_mods_load(&instance, false);
                }
            }
        }
        let liveness_rows = self.liveness_job.as_ref().and_then(Job::poll);
        if let Some(rows) = liveness_rows {
            self.liveness_job = None;
            let mut flipped = false;
            for (name, busy, running) in rows {
                let Some(slot) = self
                    .instances
                    .iter_mut()
                    .find(|entry| entry.card.name == name)
                else {
                    continue;
                };
                if slot.status.busy != busy || slot.status.running != running {
                    let entry = Arc::make_mut(slot);
                    entry.status.busy = busy;
                    entry.status.running = running;
                    flipped = true;
                }
            }
            if flipped {
                self.refresh();
            }
            changed |= flipped;
        }
        let mods_result = self.mods_job.as_ref().and_then(|job| job.job.poll());
        if let Some(result) = mods_result {
            let ModsJob {
                instance, epoch, ..
            } = self.mods_job.take().expect("polled above");
            if self.mods_instance.as_deref() == Some(instance.as_str()) && epoch == self.mods_epoch
            {
                // An error counts as loaded too, or the next frame would rescan at once.
                // `refresh` and `invalidate_mods` retry it.
                self.mods_loaded_epoch = epoch;
                self.mods = match result {
                    Ok(value) => Loaded::Ready(Arc::new(value)),
                    Err(error) => Loaded::Err(error),
                };
                changed = true;
            }
        }
        let branches_result = self.branches_job.as_ref().and_then(Job::poll);
        if let Some(result) = branches_result {
            self.branches_job = None;
            self.branches = match result {
                Ok(v) => Loaded::Ready(v),
                Err(e) => Loaded::Err(e),
            };
            changed = true;
        }
        let doctor_result = self.doctor_job.as_ref().and_then(Job::poll);
        if let Some(result) = doctor_result {
            self.doctor_job = None;
            self.doctor = Loaded::Ready(result);
            changed = true;
        }
        changed |= self.update.tick();
        let log_lines = self.log_job.as_ref().and_then(|job| job.job.poll());
        if let Some(lines) = log_lines {
            let LogJob {
                path,
                modified,
                len,
                ..
            } = self.log_job.take().expect("log job was polled");
            self.log_tails.insert(
                path,
                LogCache {
                    modified,
                    len,
                    lines,
                },
            );
            changed = true;
        }
        changed
    }

    fn instances(&self) -> &[Arc<InstanceEntry>] {
        &self.instances
    }

    fn load_errors(&self) -> &[(String, String)] {
        &self.load_errors
    }

    fn updates_available(&self, instance: &str) -> Option<&[RepoLog]> {
        self.updates.get(instance).map(Vec::as_slice)
    }

    fn refresh(&mut self) {
        // A catalog rescan must not throw away the mod catalogue. Callers that changed mods,
        // the source, or settings call [`Backend::invalidate_mods`] themselves.
        if matches!(self.branches, Loaded::Err(_)) {
            self.branches = Loaded::Idle;
        }
        // A failed mod catalogue is retried the next time the Mods tab asks for it.
        if matches!(self.mods, Loaded::Err(_)) {
            self.mods_epoch = self.mods_epoch.wrapping_add(1);
        }
        if self.refresh_job.is_some() {
            self.refresh_again = true;
            return;
        }
        self.refresh_again = false;
        let paths = self.paths.clone();
        let ctx = Arc::clone(&self.ui_ctx);
        self.refresh_job = Some(Job::spawn(ctx, move || load_catalog(&paths)));
    }

    fn refresh_liveness(&mut self) {
        if self.refresh_job.is_some() || self.liveness_job.is_some() {
            return;
        }
        let dirs: Vec<(String, PathBuf)> = self
            .instances
            .iter()
            .map(|entry| (entry.card.name.clone(), entry.card.dir.clone()))
            .collect();
        if dirs.is_empty() {
            return;
        }
        let paths = self.paths.clone();
        let ctx = Arc::clone(&self.ui_ctx);
        self.liveness_job = Some(Job::spawn(ctx, move || {
            dirs.into_iter()
                .map(|(name, dir)| {
                    let live = status::liveness(&paths, &dir);
                    (name, live.busy, live.running)
                })
                .collect()
        }));
    }

    fn refresh_pending(&self) -> bool {
        self.refresh_job.is_some()
    }

    fn set_ui_context(&mut self, ctx: egui::Context) {
        *self.ui_ctx.lock().unwrap_or_else(|err| err.into_inner()) = Some(ctx);
    }

    fn edit_meta(&mut self, instance: &str, edit: MetaEdit) -> Result<Meta, String> {
        let dir = self.paths.instance_dir(instance);
        let meta = meta::update(&dir, |m| {
            match edit {
                MetaEdit::DisplayName(v) => m.display_name = (!v.trim().is_empty()).then_some(v),
                MetaEdit::Color(v) => m.color = (!v.trim().is_empty()).then_some(v),
                MetaEdit::Group(v) => m.group = (!v.trim().is_empty()).then_some(v),
                MetaEdit::Favorite(v) => m.favorite = v,
                MetaEdit::Notes(v) => m.notes = v,
            }
            Ok(())
        })
        .map_err(|e| e.to_string())?;
        if let Some(slot) = self
            .instances
            .iter_mut()
            .find(|entry| entry.card.name == instance)
        {
            Arc::make_mut(slot).card.meta = meta.clone();
        }
        Ok(meta)
    }

    fn available_mods(&mut self, instance: &str) -> Loaded<Arc<(Vec<AvailableMod>, Vec<String>)>> {
        if self.mods_instance.as_deref() != Some(instance) {
            self.mods_instance = Some(instance.to_owned());
            self.mods_epoch = self.mods_epoch.wrapping_add(1);
            self.begin_mods_load(instance, true);
        } else if self.mods_job.is_none() && self.mods_loaded_epoch != self.mods_epoch {
            self.begin_mods_load(instance, false);
        }
        self.mods.clone()
    }

    fn invalidate_mods(&mut self) {
        if self.refresh_job.is_some() {
            self.reload_mods_after_refresh = true;
            return;
        }
        self.mods_epoch = self.mods_epoch.wrapping_add(1);
        if let Some(instance) = self.mods_instance.clone() {
            self.begin_mods_load(&instance, false);
        }
    }

    fn log_tail(&mut self, path: &Path) -> Loaded<Arc<Vec<String>>> {
        let Ok(meta) = std::fs::metadata(path) else {
            return Loaded::Ready(Arc::new(Vec::new()));
        };
        let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let len = meta.len();
        if let Some(cache) = self.log_tails.get(path)
            && cache.modified == modified
            && cache.len == len
        {
            return Loaded::Ready(Arc::clone(&cache.lines));
        }
        let same_job = self
            .log_job
            .as_ref()
            .is_some_and(|job| job.path == path && job.modified == modified && job.len == len);
        if !same_job {
            let path_buf = path.to_owned();
            let ctx = Arc::clone(&self.ui_ctx);
            self.log_job = Some(LogJob {
                path: path_buf.clone(),
                modified,
                len,
                job: Job::spawn(ctx, move || read_log_tail(&path_buf)),
            });
        }
        if let Some(cache) = self.log_tails.get(path) {
            Loaded::Ready(Arc::clone(&cache.lines))
        } else {
            Loaded::Loading
        }
    }

    fn branches(&mut self) -> Loaded<Vec<BranchInfo>> {
        if matches!(self.branches, Loaded::Idle) && self.branches_job.is_none() {
            self.branches = Loaded::Loading;
            let mirror = self.paths.mirror(Repo::Game);
            let ctx = Arc::clone(&self.ui_ctx);
            self.branches_job = Some(Job::spawn(ctx, move || {
                git::branches(&mirror).map_err(|e| e.to_string())
            }));
        }
        self.branches.clone()
    }

    fn doctor(&mut self) -> Loaded<Vec<Check>> {
        if matches!(self.doctor, Loaded::Idle) {
            self.run_doctor();
        }
        self.doctor.clone()
    }

    fn run_doctor(&mut self) {
        if self.doctor_job.is_some() {
            return;
        }
        self.doctor = Loaded::Loading;
        let paths = self.paths.clone();
        let ctx = Arc::clone(&self.ui_ctx);
        self.doctor_job = Some(Job::spawn(ctx, move || doctor::run(&paths)));
    }

    fn start_op(&mut self, request: OpRequest) -> RunningOp {
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("watt-start"));
        match watt_core::client::OpHandle::spawn_with_nice(
            &exe,
            &request,
            Some(self.settings.build_nice),
        ) {
            Ok(handle) => RunningOp::Real(handle),
            Err(e) => RunningOp::Demo(super::demo::DemoOp::immediate_failure(
                request.kind(),
                request.instance().map(str::to_owned),
                e.to_string(),
            )),
        }
    }

    fn open_path(&self, path: &Path) {
        if let Err(err) = open_in_file_manager(path) {
            eprintln!("cannot open {}: {err}", path.display());
        }
    }

    fn loading(&self) -> bool {
        self.refresh_job.is_some()
            || self.mods_job.is_some()
            || self.branches_job.is_some()
            || self.doctor_job.is_some()
    }

    fn launcher_update(&self) -> &watt_core::selfupdate::UpdateStatus {
        self.update.status()
    }

    fn check_launcher_update(&mut self, force: bool) {
        let paths = self.paths.clone();
        let enabled = self.settings.check_launcher_updates;
        self.update.start_check(&paths, force, enabled);
    }

    fn apply_launcher_update(&mut self) {
        let paths = self.paths.clone();
        self.update.start_apply(&paths);
    }
}

fn open_in_file_manager(path: &Path) -> std::io::Result<std::process::Child> {
    // `dunce::simplified` strips a `\\?\` prefix so Explorer accepts the path. On Unix it is
    // the path unchanged.
    let path = dunce::simplified(path);
    #[cfg(windows)]
    {
        std::process::Command::new("explorer.exe").arg(path).spawn()
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(path).spawn()
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        std::process::Command::new("xdg-open").arg(path).spawn()
    }
}
