//! Turn supervision.
//!
//! A turn is a durable resource whose lifetime is independent of the HTTP request
//! that created it (`docs/architecture.md`, "Turn Execution Model"). `POST /turns`
//! records the turn and returns `202`; [`turn::LinkedSupervisor`] then drives it
//! to completion in a detached task, publishing events through the event hub so
//! any number of SSE clients can attach, detach, and reattach meanwhile.
//!
//! A turn runs on a live *link* rather than in a process of its own
//! (`docs/session-protocol.md`). The one-shot runner supervisor that used to live
//! here is gone: it spawned a process per turn and read its exit code, which is
//! the model that made approvals and interrupt unreachable.

pub mod closer;
pub mod reconcile;
pub mod turn;

pub use closer::TurnCloser;
pub use reconcile::{TrajectoryOutcome, TrajectoryReconciler};
pub use turn::{LinkedSupervisor, Outcome, TurnRequest};
