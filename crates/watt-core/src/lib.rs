//! watt-core: the launcher's engine, without any GUI.
//!
//! - [`paths`], [`settings`], [`meta`], [`locks`]: where state lives and its formats.
//! - [`binding`]: an instance's sources → what pwc's libraries take.
//! - [`git`], [`snapshots`]: mirrors and immutable, content-addressed source snapshots.
//! - [`build`], [`progress`], [`process`]: locking and building with pwc, tool output → progress.
//! - [`ops`]: everything that changes state, as phase-structured, crash-safe operations.
//! - [`events`], [`view`]: the op event protocol and its folded view for front ends.
//! - [`catalog`], [`status`], [`doctor`]: read-only views for front ends.
//! - [`client`]: running ops as child processes and reading their events.
//!
//! See ARCHITECTURE.md at the repository root.

pub mod binding;
pub mod build;
pub mod catalog;
pub mod client;
pub mod doctor;
pub mod events;
pub mod git;
pub mod locks;
pub mod meta;
pub mod ops;
pub mod paths;
pub mod process;
pub mod progress;
pub mod settings;
pub mod snapshots;
pub mod status;
pub mod view;
