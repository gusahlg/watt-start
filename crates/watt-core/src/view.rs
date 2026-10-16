//! [`OpView`]: the state of a running or finished op, folded from its events. Front ends draw
//! this; they never interpret raw events themselves.

use std::collections::VecDeque;

use crate::events::{Change, Envelope, Event, OpKind, OpOutcome, Phase, PhaseOutcome};

/// How many log lines a view keeps (the full log is in the op's log file).
pub const LOG_LINES: usize = 2000;

/// One phase as shown in a progress display.
#[derive(Clone, Debug, PartialEq)]
pub struct PhaseView {
    /// Which.
    pub phase: Phase,
    /// Title.
    pub title: String,
    /// Where it stands.
    pub state: PhaseState,
    /// Milliseconds it took (finished phases).
    pub duration_ms: u64,
}

/// Where a phase stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhaseState {
    /// Planned, not started.
    Pending,
    /// Running.
    Running,
    /// Finished.
    Done(PhaseOutcome),
}

/// Everything known about one op run.
#[derive(Clone, Debug, Default)]
pub struct OpView {
    /// Op id.
    pub op_id: String,
    /// Which op (None until `op-started` arrived).
    pub op: Option<OpKind>,
    /// Instance.
    pub instance: Option<String>,
    /// Phases: the planned ones, plus any that started without being planned.
    pub phases: Vec<PhaseView>,
    /// The running phase.
    pub current: Option<Phase>,
    /// Latest progress of the running phase: (done, total).
    pub progress: Option<(u64, Option<u64>)>,
    /// Latest progress detail ("voxel_engine, project_watt_cubed").
    pub detail: String,
    /// Recent log lines (oldest first), at most [`LOG_LINES`].
    pub log: VecDeque<String>,
    /// Warnings, in order.
    pub warnings: Vec<String>,
    /// State changes, in order.
    pub changes: Vec<Change>,
    /// How it ended (None while running).
    pub outcome: Option<OpOutcome>,
    /// Milliseconds since the op started, as of the latest event.
    pub t_ms: u64,
    /// The op's log file.
    pub log_file: Option<std::path::PathBuf>,
}

impl OpView {
    /// Fold one envelope in.
    pub fn apply(&mut self, envelope: &Envelope) {
        self.op_id.clone_from(&envelope.op_id);
        self.t_ms = self.t_ms.max(envelope.t_ms);
        match &envelope.event {
            Event::OpStarted {
                op,
                instance,
                phases,
                log,
            } => {
                self.op = Some(*op);
                self.instance.clone_from(instance);
                self.log_file.clone_from(log);
                self.phases = phases
                    .iter()
                    .map(|&phase| PhaseView {
                        phase,
                        title: phase.title().to_owned(),
                        state: PhaseState::Pending,
                        duration_ms: 0,
                    })
                    .collect();
            }
            Event::PhaseStarted { phase, title, .. } => {
                let view = self.phase_mut(*phase);
                view.title.clone_from(title);
                view.state = PhaseState::Running;
                self.current = Some(*phase);
                self.progress = None;
                self.detail.clear();
            }
            Event::Progress {
                phase,
                done,
                total,
                detail,
            } => {
                if self.current == Some(*phase) {
                    self.progress = Some((*done, *total));
                    if !detail.is_empty() {
                        self.detail.clone_from(detail);
                    }
                }
            }
            Event::Log { line, .. } => {
                if self.log.len() == LOG_LINES {
                    self.log.pop_front();
                }
                self.log.push_back(line.clone());
            }
            Event::Warning { message } => self.warnings.push(message.clone()),
            Event::Changed { change } => self.changes.push(change.clone()),
            Event::PhaseFinished {
                phase,
                outcome,
                duration_ms,
            } => {
                let view = self.phase_mut(*phase);
                view.state = PhaseState::Done(*outcome);
                view.duration_ms = *duration_ms;
                if self.current == Some(*phase) {
                    self.current = None;
                    self.progress = None;
                }
            }
            Event::OpFinished { outcome, .. } => {
                self.outcome = Some(outcome.clone());
                self.current = None;
                for view in &mut self.phases {
                    if view.state == PhaseState::Running {
                        view.state = PhaseState::Done(match outcome {
                            OpOutcome::Ok { .. } => PhaseOutcome::Ok,
                            _ => PhaseOutcome::Failed,
                        });
                    }
                }
            }
        }
    }

    /// Mark the op dead: its stream ended without `op-finished` (crash, kill).
    pub fn mark_died(&mut self, cancelled: bool) {
        if self.outcome.is_none() {
            self.apply(&Envelope {
                protocol: crate::events::PROTOCOL,
                op_id: self.op_id.clone(),
                seq: u64::MAX,
                t_ms: self.t_ms,
                event: Event::OpFinished {
                    outcome: if cancelled {
                        OpOutcome::Cancelled
                    } else {
                        OpOutcome::Failed {
                            phase: self.current,
                            error:
                                "the launcher's worker process stopped unexpectedly; see the log"
                                    .to_owned(),
                            recovery: vec![crate::events::Recovery::Retry],
                        }
                    },
                    duration_ms: self.t_ms,
                },
            });
        }
    }

    /// Whether the op is still running.
    pub fn running(&self) -> bool {
        self.outcome.is_none()
    }

    /// Overall completion in 0..=1, for progress bars and animations: finished phases count
    /// fully, the running phase by its own progress. Compile and Link dominate a build, so they
    /// weigh more than the quick phases.
    pub fn fraction(&self) -> f32 {
        fn weight(phase: Phase) -> f32 {
            match phase {
                Phase::Compile => 12.0,
                Phase::Link => 4.0,
                Phase::Fetch | Phase::Checkout => 2.0,
                _ => 0.5,
            }
        }
        if matches!(self.outcome, Some(OpOutcome::Ok { .. })) {
            return 1.0;
        }
        let total: f32 = self.phases.iter().map(|p| weight(p.phase)).sum();
        if total <= 0.0 {
            return 0.0;
        }
        let mut done = 0.0;
        for view in &self.phases {
            match view.state {
                PhaseState::Done(_) => done += weight(view.phase),
                PhaseState::Running => {
                    if let Some((d, Some(t))) = self.progress
                        && t > 0
                    {
                        done += weight(view.phase) * (d as f32 / t as f32).min(1.0);
                    }
                }
                PhaseState::Pending => {}
            }
        }
        (done / total).clamp(0.0, 1.0)
    }

    /// The running phase's own progress in 0..=1, when it is determinate.
    pub fn phase_fraction(&self) -> Option<f32> {
        match self.progress {
            Some((done, Some(total))) if total > 0 => Some((done as f32 / total as f32).min(1.0)),
            _ => None,
        }
    }

    fn phase_mut(&mut self, phase: Phase) -> &mut PhaseView {
        if let Some(i) = self.phases.iter().position(|p| p.phase == phase) {
            return &mut self.phases[i];
        }
        self.phases.push(PhaseView {
            phase,
            title: phase.title().to_owned(),
            state: PhaseState::Pending,
            duration_ms: 0,
        });
        self.phases.last_mut().expect("just pushed")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{CancelToken, Reporter, VecSink};
    use std::sync::Arc;

    #[test]
    fn folds_a_build() {
        let sink = Arc::new(VecSink::default());
        let r = Reporter::new(sink.clone(), "op", CancelToken::default());
        r.emit(Event::OpStarted {
            op: OpKind::Build,
            instance: Some("default".into()),
            phases: vec![Phase::Lock, Phase::Compile, Phase::Link],
            log: None,
        });
        r.phase(Phase::Lock, false);
        r.phase(Phase::Compile, true);
        r.progress(50, Some(100), "a, b");
        let mut view = OpView::default();
        for e in sink.0.lock().unwrap().iter() {
            view.apply(e);
        }
        assert_eq!(view.current, Some(Phase::Compile));
        assert_eq!(view.detail, "a, b");
        let f = view.fraction();
        // Lock (0.5) done + half of Compile (6) out of 16.5.
        assert!((f - 6.5 / 16.5).abs() < 1e-4, "{f}");
        view.mark_died(true);
        assert_eq!(view.outcome, Some(OpOutcome::Cancelled));
        assert!(!view.running());
    }
}
