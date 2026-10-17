//! Background check and install for the launcher itself.
//!
//! This is not an op: it runs on a thread in the GUI process and reports through a channel
//! that [`RealBackend::tick`](super::RealBackend::tick) drains. The interval is decided on the
//! GUI thread; the worker then forces the check so it does not skip a second time.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};

use watt_core::paths::{Paths, now_secs};
use watt_core::selfupdate::{self, CheckState, Phase, RestartPlan, UpdateStatus, check_is_due};

/// Messages from the worker thread.
enum UpdateMsg {
    /// Bytes written so far.
    Progress { downloaded: u64, total: Option<u64> },
    /// A check finished. Replaces [`LauncherUpdate::status`].
    Checked(Box<UpdateStatus>),
    /// A replace finished. `Ok` carries the path captured before the rename.
    Applied {
        result: Result<(), String>,
        exe: PathBuf,
        from_version: String,
    },
}

/// The real backend's launcher-update state and its in-flight job.
pub(crate) struct LauncherUpdate {
    status: UpdateStatus,
    rx: Option<Receiver<UpdateMsg>>,
}

impl LauncherUpdate {
    /// The running install, before any check. Does not use the network.
    pub(crate) fn new() -> Self {
        Self {
            status: UpdateStatus::current(),
            rx: None,
        }
    }

    pub(crate) fn status(&self) -> &UpdateStatus {
        &self.status
    }

    /// Start a check when one is due.
    ///
    /// `force` is "Check now" and the CLI. The automatic startup check passes `force: false`
    /// and `auto_enabled` from the settings. `WATT_START_NO_UPDATE_CHECK=1` does nothing.
    pub(crate) fn start_check(&mut self, paths: &Paths, force: bool, auto_enabled: bool) {
        if selfupdate::update_check_disabled() || self.rx.is_some() || self.status.restart.is_some()
        {
            return;
        }
        if !force && !auto_enabled {
            return;
        }
        let state = CheckState::load(&paths.update_state()).unwrap_or_default();
        if state.last_checked > 0 {
            self.status.last_checked = Some(state.last_checked);
        }
        if !check_is_due(&state, now_secs(), force) {
            return;
        }
        self.status.phase = Phase::Checking;
        self.status.error = None;
        self.status.apply_failed = false;
        let state_path = paths.update_state();
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        std::thread::spawn(move || {
            // `force` was already applied above. Always force here so a check that was due
            // a moment ago is not skipped if the clock moves, and so the worker does not
            // read the interval a second time.
            let status = selfupdate::check_and_record(&state_path, true);
            let _ = tx.send(UpdateMsg::Checked(Box::new(status)));
        });
    }

    /// Download the executable asset and replace this process's file.
    ///
    /// The executable path is captured before the replace. On Linux `current_exe` afterwards
    /// follows the renamed `-prev` file.
    pub(crate) fn start_apply(&mut self, paths: &Paths) {
        if self.rx.is_some() || self.status.restart.is_some() || !self.status.can_apply {
            return;
        }
        let Some(asset) = self.status.asset.clone() else {
            return;
        };
        let Some(exe) = self.status.install.exe().map(Path::to_path_buf) else {
            return;
        };
        let version = self.status.latest_version.clone().unwrap_or_default();
        let from_version = self.status.current_version.clone();
        self.status.phase = Phase::Downloading;
        self.status.progress = 0.0;
        self.status.downloaded = 0;
        self.status.total = Some(asset.size);
        self.status.error = None;
        self.status.apply_failed = false;
        let updates_dir = paths.updates();
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        std::thread::spawn(move || {
            let tx_progress = tx.clone();
            let result = selfupdate::download_and_install(
                &updates_dir,
                &asset,
                &version,
                &exe,
                &mut |downloaded, total| {
                    let _ = tx_progress.send(UpdateMsg::Progress { downloaded, total });
                },
            );
            let _ = tx.send(UpdateMsg::Applied {
                result: result.map_err(|e| format!("{e:#}")),
                exe,
                from_version,
            });
        });
    }

    /// Apply whatever the worker has sent. `true` when the status changed.
    pub(crate) fn tick(&mut self) -> bool {
        let Some(rx) = self.rx.take() else {
            return false;
        };
        let mut changed = false;
        let mut finished = false;
        loop {
            match rx.try_recv() {
                Ok(msg) => {
                    changed = true;
                    if apply_msg(&mut self.status, msg) {
                        finished = true;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if !finished {
                        mark_stopped(&mut self.status);
                        changed = true;
                    }
                    finished = true;
                    break;
                }
            }
        }
        if !finished {
            self.rx = Some(rx);
        }
        changed
    }
}

/// `true` when this message ends the job.
fn apply_msg(status: &mut UpdateStatus, msg: UpdateMsg) -> bool {
    match msg {
        UpdateMsg::Progress { downloaded, total } => {
            status.downloaded = downloaded;
            if total.is_some() {
                status.total = total;
            }
            status.progress = match status.total {
                Some(total) if total > 0 => (downloaded as f32 / total as f32).clamp(0.0, 1.0),
                _ => 0.0,
            };
            false
        }
        UpdateMsg::Checked(next) => {
            *status = *next;
            true
        }
        UpdateMsg::Applied {
            result,
            exe,
            from_version,
        } => {
            match result {
                Ok(()) => {
                    status.phase = Phase::Available;
                    status.progress = 1.0;
                    status.can_apply = false;
                    status.error = None;
                    status.apply_failed = false;
                    status.restart = Some(RestartPlan { exe, from_version });
                }
                Err(message) => {
                    status.phase = Phase::Failed;
                    status.error = Some(message);
                    status.apply_failed = true;
                    status.can_apply = status.asset.is_some() && status.install.can_replace();
                }
            }
            true
        }
    }
}

fn mark_stopped(status: &mut UpdateStatus) {
    if status.phase == Phase::Downloading {
        status.apply_failed = true;
    }
    status.phase = Phase::Failed;
    if status.error.is_none() {
        status.error = Some("the update task stopped".to_owned());
    }
}
