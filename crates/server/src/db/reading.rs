//! Bounded reading addresses over the existing execution projection.
use super::timeline::{PAGE_BYTES, PAGE_NODES};
use crate::{
    error::{Error, Result},
    models::Turn,
};
use serde_json::{json, Value};
use sqlx::{Row, SqliteConnection};

fn role(node: &Value) -> &'static str {
    if node["event"]["kind"] != "assistant" {
        return "unknown";
    }
    match node["event"]["data"]["phase"]
        .as_str()
        .or(node["event"]["data"]["presentationRole"].as_str())
    {
        Some("commentary" | "progress") => "progress",
        Some("final_answer" | "answer") => "answer",
        _ => "unknown",
    }
}
async fn owner(conn: &mut SqliteConnection, turn: &str) -> Result<Value> {
    let row = sqlx::query_as::<_, Turn>("SELECT * FROM turns WHERE id=?")
        .bind(turn)
        .fetch_optional(conn)
        .await?;
    Ok(row
        .map(|t| {
            json!({"id":t.id,"origin":t.origin,"status":t.status,
        "startedAtMs":t.started_at_ms,"completedAtMs":t.completed_at_ms,
        "closeReason":t.close_reason,"errorMessage":t.error_message})
        })
        .unwrap_or(Value::Null))
}
async fn node_value(
    conn: &mut SqliteConnection,
    session: &str,
    id: &str,
    work: bool,
) -> Result<Value> {
    let mut value = if work {
        let r = sqlx::query("SELECT * FROM conversation_work WHERE session_id=? AND id=?")
            .bind(session)
            .bind(id)
            .fetch_one(&mut *conn)
            .await?;
        let mut n = json!({"id":id,"parentId":r.get::<String,_>("parent_id"),"turnId":r.get::<String,_>("turn_id"),
            "ordinal":r.get::<i64,_>("ordinal"),"orderTie":r.get::<i64,_>("order_tie"),"revision":r.get::<i64,_>("revision"),
            "bodyRevision":0,"activity":"","event":{"seq":-1,"kind":"work","stability":"stable","source":"server","createdAtMs":0}});
        for k in [
            "children",
            "tools",
            "reads",
            "commands",
            "thinking",
            "failed",
            "running",
            "waiting",
            "unfinished",
        ] {
            n[k] = json!(r.get::<i64, _>(k));
        }
        n["removed"] = json!(r.get::<i64, _>("children") == 0);
        n
    } else {
        serde_json::to_value(
            super::timeline::get(conn, session, id)
                .await?
                .ok_or(Error::SessionNotFound)?,
        )
        .map_err(|e| Error::Internal(e.to_string()))?
    };
    value["readingRole"] = json!(role(&value));
    value["turn"] = owner(conn, value["turnId"].as_str().unwrap_or("")).await?;
    // A delegated scope can outlive the turn that first recorded it. Its own
    // report (or promoted host result) owns the status and clock endpoints.
    if work {
        let scope = value["parentId"].as_str().unwrap_or("");
        if !scope.is_empty() {
            if let Some(parent) = super::timeline::get(conn, session, scope).await? {
                let result = if parent.event.kind == "tool_call" && parent.trajectory.is_none() {
                    super::timeline::get(conn, session, &format!("r:{scope}")).await?
                } else {
                    None
                };
                let report = parent
                    .trajectory
                    .as_ref()
                    .or(result.as_ref().map(|r| &r.event))
                    .unwrap_or(&parent.event);
                let closed_by_server = report
                    .data
                    .as_ref()
                    .and_then(|d| d.get("closedBy"))
                    .and_then(Value::as_str)
                    == Some("server");
                let terminal = report.phase.as_deref() == Some("completed")
                    && (parent.event.kind != "tool_call"
                        || parent.trajectory.is_some()
                        || result.is_some());
                let status = if closed_by_server {
                    "interrupted"
                } else if terminal {
                    if report.is_error {
                        "failed"
                    } else {
                        "completed"
                    }
                } else if matches!(report.phase.as_deref(), Some("started" | "delta"))
                    || parent.running > 0
                {
                    "running"
                } else {
                    "unknown"
                };
                value["turn"]["status"] = json!(status);
                value["turn"]["startedAtMs"] = json!(parent.event.started_at_ms);
                value["turn"]["completedAtMs"] = if terminal {
                    json!(report.arrival_at_ms)
                } else {
                    Value::Null
                };
                value["turn"]["closeReason"] = report
                    .data
                    .as_ref()
                    .and_then(|d| d.get("reason"))
                    .cloned()
                    .unwrap_or(Value::Null);
                value["turn"]["errorMessage"] = Value::Null;
            }
        }
    }
    Ok(value)
}

impl super::JournalStore {
    /// Initial and change windows use the same stable ordering and revision axes.
    pub async fn conversation_page(
        &self,
        session: &str,
        parent: &str,
        cursor: Option<&str>,
        changes: bool,
        limit: i64,
    ) -> Result<Value> {
        let mut tx = self.readers.begin().await?;
        let snapshot: i64 = sqlx::query_scalar("SELECT revision FROM timeline_clock WHERE id=1")
            .fetch_one(&mut *tx)
            .await?;
        let work: Option<(String, String)> = sqlx::query_as(
            "SELECT parent_id,turn_id FROM conversation_work WHERE session_id=? AND id=?",
        )
        .bind(session)
        .bind(parent)
        .fetch_optional(&mut *tx)
        .await?;
        let invalid = || Error::InvalidRequest("invalid conversation cursor".into());
        let (key, tie, id) = if let Some(c) = cursor {
            let (k, rest) = c.split_once(':').ok_or_else(invalid)?;
            let key = k.parse::<i64>().map_err(|_| invalid())?;
            if changes {
                (key, 0, rest.to_owned())
            } else {
                let (t, id) = rest.split_once(':').ok_or_else(invalid)?;
                (key, t.parse::<i64>().map_err(|_| invalid())?, id.to_owned())
            }
        } else {
            (if changes { 0 } else { i64::MAX }, i64::MAX, String::new())
        };
        let mut source = if work.is_some() {
            "SELECT id,ordinal,order_tie,revision,0 AS work FROM conversation_entries WHERE session_id=? AND parent_id=? AND turn_id=? AND process=1".to_owned()
        } else {
            "SELECT id,ordinal,order_tie,revision,0 AS work FROM conversation_entries WHERE session_id=? AND parent_id=? AND process=0 UNION ALL SELECT id,ordinal,order_tie,revision,1 AS work FROM conversation_work WHERE session_id=? AND parent_id=?".to_owned()
        };
        if !changes && work.is_none() {
            source.push_str(" AND children>0");
        }
        if changes {
            source.push_str(" UNION ALL SELECT id,0 AS ordinal,0 AS order_tie,revision,2 AS work FROM conversation_removals WHERE session_id=? AND parent_id=?");
        }
        let filter = if changes {
            "(revision,id)>(?,?) ORDER BY revision,id"
        } else {
            "(ordinal,order_tie,id)<(?,?,?) ORDER BY ordinal DESC,order_tie DESC,id DESC"
        };
        let sql = format!("SELECT * FROM ({source}) WHERE {filter} LIMIT ?");
        let scope = work.as_ref().map_or(parent, |w| w.0.as_str());
        let query = sqlx::query(&sql).bind(session).bind(scope);
        let query = if let Some((_, turn)) = &work {
            query.bind(turn)
        } else {
            query.bind(session).bind(scope)
        };
        let query = if changes {
            query.bind(session).bind(parent)
        } else {
            query
        };
        let query = query.bind(key);
        let query = if changes { query } else { query.bind(tie) };
        let limit = limit.clamp(1, PAGE_NODES) as usize;
        let rows = query
            .bind(id)
            .bind(limit as i64 + 1)
            .fetch_all(&mut *tx)
            .await?;
        let parent_node = if work.is_some() {
            Some(node_value(&mut tx, session, parent, true).await?)
        } else if !parent.is_empty() {
            Some(node_value(&mut tx, session, parent, false).await?)
        } else {
            None
        };
        let mut nodes = Vec::new();
        let mut bytes = 512 + parent_node.as_ref().map_or(0, |p| p.to_string().len());
        let mut more = false;
        for row in rows {
            let n = if row.get::<i64, _>("work") == 2 {
                let raw:String=sqlx::query_scalar("SELECT node_json FROM conversation_removals WHERE session_id=? AND parent_id=? AND id=?").bind(session).bind(parent).bind(row.get::<String,_>("id")).fetch_one(&mut *tx).await?;
                serde_json::from_str(&raw).map_err(|e| Error::Internal(e.to_string()))?
            } else {
                node_value(
                    &mut tx,
                    session,
                    &row.get::<String, _>("id"),
                    row.get::<bool, _>("work"),
                )
                .await?
            };
            let size = n.to_string().len() + n["id"].as_str().unwrap_or("").len() + 96;
            if nodes.len() == limit || bytes + size > PAGE_BYTES {
                if nodes.is_empty() {
                    return Err(Error::Internal("reading node exceeds page budget".into()));
                }
                more = true;
                break;
            }
            bytes += size;
            nodes.push(n);
        }
        let next = if more {
            nodes.last().map(|n| {
                if changes {
                    format!("{}:{}", n["revision"], n["id"].as_str().unwrap())
                } else {
                    format!(
                        "{}:{}:{}",
                        n["ordinal"],
                        n["orderTie"],
                        n["id"].as_str().unwrap()
                    )
                }
            })
        } else if changes {
            Some(format!("{}:", snapshot + 1))
        } else {
            None
        };
        if !changes {
            nodes.reverse();
        }
        tx.commit().await?;
        Ok(
            json!({"parent":parent,"parentNode":parent_node,"nodes":nodes,"snapshot":snapshot,"nextCursor":next,"hasMore":more,"targetId":null}),
        )
    }
}

impl super::JournalStore {
    /// Walk recorded session edges only; trajectory membership is already indexed.
    /// The access boundary is inside the recursion, before counts and pagination.
    pub async fn delegation_page(
        &self,
        session: &str,
        subject: &str,
        query: &str,
        cursor: Option<&str>,
        limit: i64,
    ) -> Result<Value> {
        let environments = self.capable_environments(subject, "observe").await?;
        let allowed =
            serde_json::to_string(&environments).map_err(|e| Error::Internal(e.to_string()))?;
        let cte = r#"WITH RECURSIVE reachable(id) AS (
          SELECT ? UNION SELECT e.child_env||':'||e.child_agent||':'||e.child_native
          FROM session_edges e JOIN reachable r ON r.id=e.parent_env||':'||e.parent_agent||':'||e.parent_native
          WHERE e.relation='delegation' AND e.child_env IN (SELECT value FROM json_each(?))
        ), catalogue AS (
          SELECT n.session_id||'/'||n.id AS key,n.session_id,n.id AS node_id,n.parent_id,
            (SELECT COALESCE(json_extract(p.trajectory_json,'$.name'),json_extract(p.event_json,'$.name')) FROM timeline_trajectories a JOIN timeline_nodes p ON p.session_id=a.session_id AND p.id=a.node_id WHERE a.session_id=n.session_id AND a.stream_id=COALESCE(json_extract(n.trajectory_json,'$.span'),json_extract(n.event_json,'$.span')) LIMIT 1) AS parent_name,
            substr(COALESCE(json_extract(n.trajectory_json,'$.name'),json_extract(n.event_json,'$.name')),1,512) AS name,
            CASE WHEN n.waiting>0 THEN 'waiting' WHEN n.running>0 THEN 'running'
              WHEN COALESCE(json_extract(n.trajectory_json,'$.data.closedBy'),json_extract(n.event_json,'$.data.closedBy'))='server' THEN 'unfinished'
              WHEN COALESCE(json_extract(n.trajectory_json,'$.phase'),json_extract(n.event_json,'$.phase'))='completed' OR EXISTS(SELECT 1 FROM timeline_nodes result WHERE result.session_id=n.session_id AND result.id='r:'||n.id) THEN CASE WHEN n.failed>0 THEN 'failed' ELSE 'completed' END
              ELSE 'unknown' END AS status,
            COALESCE(json_extract(n.trajectory_json,'$.interiorState'),json_extract(n.event_json,'$.interiorState')) AS interior,
            n.ordinal AS created,n.revision,
            COALESCE(json_extract(n.trajectory_json,'$.arrivalAtMs'),json_extract(n.event_json,'$.arrivalAtMs'),0) AS ended,
            s.fidelity,s.workdir
          FROM timeline_nodes n JOIN reachable r ON r.id=n.session_id
          JOIN sessions s ON n.session_id=s.environment||':'||s.agent||':'||s.native_id
          WHERE n.scope_id=n.id
          UNION ALL
          SELECT s.environment||':'||s.agent||':'||s.native_id,
            s.environment||':'||s.agent||':'||s.native_id,NULL,NULL,NULL,substr(s.title,1,512),
            CASE WHEN s.status='running' THEN 'running' ELSE COALESCE((SELECT status FROM turns t WHERE t.session_env=s.environment AND t.session_agent=s.agent AND t.session_native=s.native_id ORDER BY created_at_ms DESC,id DESC LIMIT 1),'unknown') END,
            NULL,s.created_at_ms,s.updated_at_ms,s.updated_at_ms,s.fidelity,s.workdir
          FROM sessions s JOIN reachable r ON r.id=s.environment||':'||s.agent||':'||s.native_id WHERE r.id<>?
        ), ordered AS (
          SELECT *,CASE WHEN status IN ('running','waiting','queued','stopping','settling') THEN 0 WHEN status='unknown' THEN 2 ELSE 1 END AS lane,
          CASE WHEN status IN ('running','waiting','queued','stopping','settling','unknown') THEN created ELSE -ended END AS stamp
          FROM catalogue WHERE ?='' OR instr(lower(COALESCE(name,'')||' '||session_id),lower(?))>0
        ) "#;
        let mut tx = self.readers.begin().await?;
        let count_sql = format!("{cte} SELECT lane,count(*) AS count FROM ordered GROUP BY lane");
        let counts = sqlx::query(&count_sql)
            .bind(session)
            .bind(&allowed)
            .bind(session)
            .bind(query)
            .bind(query)
            .fetch_all(&mut *tx)
            .await?;
        let mut totals = json!({"active":0,"finished":0,"unknown":0});
        for r in counts {
            let key = match r.get::<i64, _>("lane") {
                0 => "active",
                1 => "finished",
                _ => "unknown",
            };
            totals[key] = json!(r.get::<i64, _>("count"));
        }
        let (lane, stamp, key): (i64, i64, String) = cursor
            .map(|c| {
                serde_json::from_str(c)
                    .map_err(|_| Error::InvalidRequest("invalid delegation cursor".into()))
            })
            .transpose()?
            .unwrap_or((-1, 0, String::new()));
        let sql=format!("{cte} SELECT * FROM ordered WHERE (lane,stamp,key)>(?,?,?) ORDER BY lane,stamp,key LIMIT ?");
        let limit = limit.clamp(1, PAGE_NODES) as usize;
        let rows = sqlx::query(&sql)
            .bind(session)
            .bind(&allowed)
            .bind(session)
            .bind(query)
            .bind(query)
            .bind(lane)
            .bind(stamp)
            .bind(key)
            .bind(limit as i64 + 1)
            .fetch_all(&mut *tx)
            .await?;
        let mut items = Vec::new();
        let mut next = None;
        let mut bytes = 512;
        let mut more = false;
        for r in rows {
            let key = r.get::<String, _>("key");
            let session_id = r.get::<String, _>("session_id");
            let node = r.get::<Option<String>, _>("node_id");
            let item = json!({"id":key,"target":{"sessionId":session_id,"nodeId":node},"parentId":r.get::<Option<String>,_>("parent_id"),
                "parentName":r.get::<Option<String>,_>("parent_name"),"name":r.get::<Option<String>,_>("name"),"status":r.get::<String,_>("status"),"interiorState":r.get::<Option<String>,_>("interior"),
                "revision":r.get::<i64,_>("revision"),"fidelity":r.get::<String,_>("fidelity"),"workdir":r.get::<String,_>("workdir")});
            let size = item.to_string().len() + 256;
            if items.len() == limit || bytes + size > PAGE_BYTES {
                more = true;
                break;
            }
            bytes += size;
            next =
                Some(json!([r.get::<i64, _>("lane"), r.get::<i64, _>("stamp"), key]).to_string());
            items.push(item);
        }
        let snapshot: i64 = sqlx::query_scalar("SELECT revision FROM timeline_clock WHERE id=1")
            .fetch_one(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(
            json!({"items":items,"counts":totals,"snapshot":snapshot,"hasMore":more,"nextCursor":if more{next}else{None}}),
        )
    }
}
