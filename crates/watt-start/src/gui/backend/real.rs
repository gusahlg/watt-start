//! The real backend: a thin front end over `watt-core`. Every slow call runs on a background
//! thread; results are delivered through a channel and picked up by [`RealBackend::tick`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};

use watt_core::catalog::{self, AvailableMod};
use watt_core::doctor::{self, Check};
use watt_core::events::RepoLog;
use watt_core::git::{self, BranchInfo};
use watt_core::meta::{self, Meta};
use watt_core::ops::OpRequest;
use watt_core::paths::{Paths, Repo};
use watt_core::settings::Settings;
use watt_core::status;

use super::{Backend, InstanceEntry, Loaded, MetaEdit, RunningOp};

/// `catalog::list_instances` + `status::compute` for every instance, plus the unreadable ones.
type CatalogResult = Result<(Vec<InstanceEntry>, Vec<(String, String)>), String>;
/// `catalog::available_mods`.
type ModsResult = Result<(Vec<AvailableMod>, Vec<String>), String>;

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
    fn spawn(f: impl FnOnce() -> T + Send + 'static) -> Self {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
        Self { rx }
    }

    fn poll(&self) -> Option<T> {
        self.rx.try_recv().ok()
    }
}

/// The real backend.
#[allow(dead_code)]
pub struct RealBackend {
    paths: Paths,
    settings: Settings,
    instances: Vec<InstanceEntry>,
    load_errors: Vec<(String, String)>,
    updates: BTreeMap<String, Vec<RepoLog>>,
    refresh_job: Option<Job<CatalogResult>>,
    /// `refresh` was asked for while a scan was already running. Start another when it lands.
    refresh_again: bool,
    mods_instance: Option<String>,
    mods: Loaded<(Vec<AvailableMod>, Vec<String>)>,
    mods_job: Option<Job<ModsResult>>,
    branches: Loaded<Vec<BranchInfo>>,
    branches_job: Option<Job<Result<Vec<BranchInfo>, String>>>,
    doctor: Loaded<Vec<Check>>,
    doctor_job: Option<Job<Vec<Check>>>,
}

#[allow(dead_code)]
impl RealBackend {
    /// Load settings and do a first (background) catalog scan.
    pub fn new() -> anyhow::Result<Self> {
        let paths = Paths::from_env()?;
        let settings = Settings::load(&paths.settings_file)?;
        let mut backend = Self {
            paths,
            settings,
            instances: Vec::new(),
            load_errors: Vec::new(),
            updates: BTreeMap::new(),
            refresh_job: None,
            refresh_again: false,
            mods_instance: None,
            mods: Loaded::Idle,
            mods_job: None,
            branches: Loaded::Idle,
            branches_job: None,
            doctor: Loaded::Idle,
            doctor_job: None,
        };
        backend.refresh();
        Ok(backend)
    }
}

#[allow(dead_code)]
fn load_catalog(paths: &Paths) -> CatalogResult {
    let cards = catalog::list_instances(paths).map_err(|e| e.to_string())?;
    let mut entries = Vec::with_capacity(cards.len());
    for card in cards {
        let status = status::compute(paths, &card).map_err(|e| e.to_string())?;
        entries.push(InstanceEntry { card, status });
    }
    let errors = catalog::list_errors(paths);
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
    }

    fn tick(&mut self) -> bool {
        let mut changed = false;
        if let Some(job) = &self.refresh_job
            && let Some(result) = job.poll()
        {
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
            }
        }
        if let Some(job) = &self.mods_job
            && let Some(result) = job.poll()
        {
            self.mods_job = None;
            self.mods = match result {
                Ok(v) => Loaded::Ready(v),
                Err(e) => Loaded::Err(e),
            };
            changed = true;
        }
        if let Some(job) = &self.branches_job
            && let Some(result) = job.poll()
        {
            self.branches_job = None;
            self.branches = match result {
                Ok(v) => Loaded::Ready(v),
                Err(e) => Loaded::Err(e),
            };
            changed = true;
        }
        if let Some(job) = &self.doctor_job
            && let Some(result) = job.poll()
        {
            self.doctor_job = None;
            self.doctor = Loaded::Ready(result);
            changed = true;
        }
        changed
    }

    fn instances(&self) -> &[InstanceEntry] {
        &self.instances
    }

    fn load_errors(&self) -> &[(String, String)] {
        &self.load_errors
    }

    fn updates_available(&self, instance: &str) -> Option<&[RepoLog]> {
        self.updates.get(instance).map(Vec::as_slice)
    }

    fn refresh(&mut self) {
        // What an instance can add depends on its source and the settings' repositories, which
        // ops change: load it again next time it is asked for. Failed branch lists get retried.
        if self.mods_job.is_none() {
            self.mods_instance = None;
        }
        if matches!(self.branches, Loaded::Err(_)) {
            self.branches = Loaded::Idle;
        }
        if self.refresh_job.is_some() {
            self.refresh_again = true;
            return;
        }
        self.refresh_again = false;
        let paths = self.paths.clone();
        self.refresh_job = Some(Job::spawn(move || load_catalog(&paths)));
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
        if let Some(entry) = self
            .instances
            .iter_mut()
            .find(|entry| entry.card.name == instance)
        {
            entry.card.meta = meta.clone();
        }
        Ok(meta)
    }

    fn available_mods(&mut self, instance: &str) -> Loaded<(Vec<AvailableMod>, Vec<String>)> {
        if self.mods_instance.as_deref() != Some(instance) {
            self.mods_instance = Some(instance.to_owned());
            self.mods = Loaded::Loading;
            let paths = self.paths.clone();
            let settings = self.settings.clone();
            if let Some(entry) = self.instances.iter().find(|e| e.card.name == instance) {
                let card = entry.card.clone();
                self.mods_job = Some(Job::spawn(move || {
                    catalog::available_mods(&paths, &settings, &card).map_err(|e| e.to_string())
                }));
            } else {
                self.mods = Loaded::Err("instance not found".to_owned());
            }
        }
        self.mods.clone()
    }

    fn branches(&mut self) -> Loaded<Vec<BranchInfo>> {
        if matches!(self.branches, Loaded::Idle) && self.branches_job.is_none() {
            self.branches = Loaded::Loading;
            let mirror = self.paths.mirror(Repo::Game);
            self.branches_job = Some(Job::spawn(move || {
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
        self.doctor_job = Some(Job::spawn(move || doctor::run(&paths)));
    }

    fn start_op(&mut self, request: OpRequest) -> RunningOp {
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("watt-start"));
        match watt_core::client::OpHandle::spawn(&exe, &request) {
            Ok(handle) => RunningOp::Real(handle),
            Err(e) => RunningOp::Demo(super::demo::DemoOp::immediate_failure(
                request.kind(),
                request.instance().map(str::to_owned),
                e.to_string(),
            )),
        }
    }

    fn open_path(&self, path: &Path) {
        open_in_file_manager(path);
    }

    fn loading(&self) -> bool {
        self.refresh_job.is_some()
            || self.mods_job.is_some()
            || self.branches_job.is_some()
            || self.doctor_job.is_some()
    }
}

#[allow(dead_code)]
#[cfg(target_os = "macos")]
fn open_in_file_manager(path: &Path) {
    let _ = std::process::Command::new("open").arg(path).spawn();
}

#[allow(dead_code)]
#[cfg(not(target_os = "macos"))]
fn open_in_file_manager(path: &Path) {
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
}
