//! Machine-level directory browsing delegated to the selected environment envagent.

use crate::{
    error::{Error, Result},
    AppState,
};
use axum::{
    extract::{Extension, Query, State},
    Json,
};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Root {
    pub name: String,
    pub path: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RootsResponse {
    pub roots: Vec<Root>,
    pub home: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirEntryOut {
    pub path: String,
    pub name: String,
    pub modified_at_ms: i64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_symlink: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowseResponse {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    pub entries: Vec<DirEntryOut>,
    pub truncated: bool,
}

#[derive(Deserialize, Default)]
pub struct BrowseEnvironmentQuery {
    pub environment: Option<String>,
}

#[derive(Deserialize, Default)]
pub struct BrowseQuery {
    #[serde(default)]
    pub path: String,
    pub environment: Option<String>,
}

async fn select_environment(
    state: &AppState,
    subject: &crate::middleware::auth::SubjectId,
    raw: Option<String>,
) -> Result<sylph_protocol::EnvironmentId> {
    if let Some(raw) = raw.filter(|value| !value.trim().is_empty()) {
        let id = sylph_protocol::EnvironmentId::parse(raw.trim())
            .map_err(|error| Error::InvalidRequest(error.to_string()))?;
        if !state.env_bridge.environment_connected(&id).await {
            return Err(Error::EnvironmentUnreachable(id.to_string()));
        }
        if !state
            .store
            .has_capability(&subject.0, id.as_str(), "observe")
            .await?
        {
            return Err(Error::Forbidden("observe capability required".into()));
        }
        return Ok(id);
    }
    let environments = state
        .env_bridge
        .list_environments()
        .await
        .map_err(|error| Error::Internal(format!("{error:#}")))?;
    for environment in environments {
        if environment.connected
            && state
                .store
                .has_capability(&subject.0, environment.id.as_str(), "observe")
                .await?
        {
            return Ok(environment.id);
        }
    }
    Err(Error::EnvironmentUnreachable(
        "no connected environment".into(),
    ))
}

fn map_remote_browse_error(error: anyhow::Error) -> Error {
    let text = format!("{error:#}");
    if text.contains("POLICY_VIOLATION") || text.contains("outside") || text.contains("escapes") {
        Error::Forbidden("That path is outside the permitted directory.".into())
    } else if text.contains("FILE_NOT_FOUND")
        || text.contains("path does not exist")
        || text.contains("No such file")
    {
        Error::FileNotFound
    } else if text.contains("not currently available") {
        Error::EnvironmentUnreachable("selected environment".into())
    } else {
        Error::Internal(text)
    }
}

/// GET /api/v1/fs/roots?environment=...
pub async fn roots(
    State(state): State<AppState>,
    Extension(subject): Extension<crate::middleware::auth::SubjectId>,
    Query(query): Query<BrowseEnvironmentQuery>,
) -> Result<Json<RootsResponse>> {
    let environment = select_environment(&state, &subject, query.environment).await?;
    let value = state
        .env_bridge
        .remote_browse(
            &environment,
            sylph_protocol::EnvironmentCommand::BrowseRoots,
        )
        .await
        .map_err(map_remote_browse_error)?;
    serde_json::from_value(value)
        .map(Json)
        .map_err(|error| Error::Internal(format!("invalid browse roots response: {error}")))
}

/// GET /api/v1/fs/tree?path=...&environment=...
pub async fn tree(
    State(state): State<AppState>,
    Extension(subject): Extension<crate::middleware::auth::SubjectId>,
    Query(query): Query<BrowseQuery>,
) -> Result<Json<BrowseResponse>> {
    let environment = select_environment(&state, &subject, query.environment).await?;
    let value = state
        .env_bridge
        .remote_browse(
            &environment,
            sylph_protocol::EnvironmentCommand::BrowseTree {
                path: (!query.path.trim().is_empty()).then(|| query.path.trim().to_string()),
            },
        )
        .await
        .map_err(map_remote_browse_error)?;
    serde_json::from_value(value)
        .map(Json)
        .map_err(|error| Error::Internal(format!("invalid browse tree response: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_browse_errors_preserve_policy_and_missing_codes() {
        assert!(matches!(
            map_remote_browse_error(anyhow::anyhow!("POLICY_VIOLATION: outside root")),
            Error::Forbidden(_)
        ));
        assert!(matches!(
            map_remote_browse_error(anyhow::anyhow!("FILE_NOT_FOUND: path does not exist")),
            Error::FileNotFound
        ));
    }
}
