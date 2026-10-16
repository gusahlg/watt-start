//! Running an op as a child process and reading its events: what the GUI uses.
//!
//! [`OpHandle::spawn`] starts `<exe> op --events json --request <json>` in a new process group
//! with stdout piped (events) and stderr piped (anything the worker printed before its own
//! capture started, or after it broke: forwarded as log lines). A reader thread parses envelopes
//! into a channel; [`OpHandle::poll`] drains it into an [`OpView`]. [`OpHandle::cancel`] kills
//! the whole process group (git, nix, cargo, rustc included).

use std::path::Path;
use std::sync::mpsc::Receiver;

use anyhow::Result;

use crate::events::Envelope;
use crate::ops::OpRequest;
use crate::view::OpView;

/// A running (or finished) op child.
pub struct OpHandle {
    _rx: Receiver<Envelope>,
}

impl OpHandle {
    /// Spawn the worker. `exe` is normally `std::env::current_exe()`.
    pub fn spawn(exe: &Path, request: &OpRequest) -> Result<Self> {
        let _ = (exe, request);
        todo!("worker C")
    }

    /// Apply every envelope that arrived to `view`; returns whether anything changed. When the
    /// child has exited and its stream is drained without `op-finished`, marks the view died
    /// (cancelled if [`OpHandle::cancel`] was called).
    pub fn poll(&mut self, view: &mut OpView) -> bool {
        let _ = view;
        todo!("worker C")
    }

    /// Kill the op's process group (SIGTERM, SIGKILL after 3 s) on a background thread.
    pub fn cancel(&mut self) {
        todo!("worker C")
    }

    /// Whether the child is still running.
    pub fn running(&mut self) -> bool {
        todo!("worker C")
    }
}
