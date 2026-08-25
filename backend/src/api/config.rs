//! `GET /api/v1/config` — what configuration resolved to, and who said so.
//!
//! The same report `sylph-web-backend doctor` renders, served to the settings
//! panel. One producer for both, because a panel that derived its own view of
//! configuration would be a second answer free to disagree with the authoritative
//! one — and the disagreement surfaces as a user fixing a path that was never the
//! one being read.
//!
//! # Read-only, deliberately
//!
//! There is no `PUT`. Two of these values are security-relevant: `browse_roots`
//! bounds what any client of this backend may enumerate, and `agents_dir` names
//! directories from which this server executes programs. A loopback HTTP surface
//! that could rewrite either is a materially larger threat-model question than one
//! that displays them, and the point of the file is that it is the durable,
//! reviewable, hand-edited record. So the panel shows the resolved values and the
//! path of the file to open.
//!
//! # Why it is authenticated
//!
//! Every value here is an absolute local filesystem path. `/health` stays
//! unauthenticated for supervisor probes and reports booleans only.

use axum::{extract::State, Json};

use crate::{config::Report, error::Result, AppState};

/// GET /api/v1/config
pub async fn get(State(state): State<AppState>) -> Result<Json<Report>> {
    Ok(Json(state.config.report()))
}
