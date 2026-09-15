//! Incremental execution projection. Original bodies stay in the event tiers.
//!
//! Read cost is proportional to requested children. Write cost follows changed
//! ancestor paths; no read endpoint folds a turn or counts a subtree.
use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use sqlx::{Row, SqliteConnection};

use crate::error::{Error, Result};
use crate::events::{BodyRef, StreamEvent};
use crate::models::Event;

pub const PAGE_NODES: i64 = 40;
pub const PAGE_BYTES: usize = 128 * 1024;

pub const CHILDREN_SQL: &str = "SELECT * FROM timeline_nodes WHERE session_id=? AND parent_id=? AND (ordinal,order_tie,id)<(?,?,?) ORDER BY ordinal DESC,order_tie DESC,id DESC LIMIT ?";
pub const CHANGES_SQL: &str = "SELECT * FROM timeline_nodes WHERE session_id=? AND parent_id=? AND (revision,id)>(?,?) ORDER BY revision,id LIMIT ?";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Node {
    pub id: String,
    pub removed: bool,
    pub structure_error: Option<String>,
    pub parent_id: String,
    pub turn_id: String,
    pub ordinal: i64,
    pub order_tie: i64,
    pub revision: i64,
    pub body_revision: i64,
    #[serde(skip)]
    pub(super) body_start_seq: i32,
    pub event: StreamEvent,
    pub trajectory: Option<StreamEvent>,
    pub children: i64,
    pub tools: i64,
    pub reads: i64,
    pub commands: i64,
    pub thinking: i64,
    pub failed: i64,
    pub running: i64,
    pub waiting: i64,
    pub unfinished: i64,
    #[serde(skip)]
    scope_id: String,
    #[serde(skip)]
    own_unfinished: bool,
    #[serde(skip)]
    own_activity: String,
    pub activity: String,
    #[serde(skip)]
    own_running: i64,
    #[serde(skip)]
    child_running: i64,
    #[serde(skip)]
    own_failed: i64,
    #[serde(skip)]
    declared: bool,
}

fn decode(row: sqlx::sqlite::SqliteRow) -> Result<Node> {
    Ok(Node {
        id: row.try_get("id")?,
        removed: false,
        structure_error: row.try_get("structure_error")?,
        parent_id: row.try_get("parent_id")?,
        turn_id: row.try_get("turn_id")?,
        ordinal: row.try_get("ordinal")?,
        order_tie: row.try_get("order_tie")?,
        revision: row.try_get("revision")?,
        body_revision: row.try_get("body_revision")?,
        body_start_seq: row.try_get("body_start_seq")?,
        event: serde_json::from_str(row.try_get("event_json")?)
            .map_err(|e| Error::Internal(format!("invalid timeline projection: {e}")))?,
        trajectory: row
            .try_get::<Option<String>, _>("trajectory_json")?
            .map(|s| serde_json::from_str(&s).map_err(|e| Error::Internal(e.to_string())))
            .transpose()?,
        children: row.try_get("children")?,
        tools: row.try_get("tools")?,
        reads: row.try_get("reads")?,
        commands: row.try_get("commands")?,
        thinking: row.try_get("thinking")?,
        failed: row.try_get("failed")?,
        running: row.try_get("running")?,
        waiting: row.try_get("waiting")?,
        unfinished: row.try_get("unfinished")?,
        scope_id: row.try_get("scope_id")?,
        own_unfinished: row.try_get("own_unfinished")?,
        own_activity: row.try_get("own_activity")?,
        activity: row.try_get("activity")?,
        own_running: row.try_get("own_running")?,
        child_running: row.try_get("child_running")?,
        own_failed: row.try_get("own_failed")?,
        declared: row.try_get("declared")?,
    })
}

pub(crate) async fn get(
    conn: &mut SqliteConnection,
    session: &str,
    id: &str,
) -> Result<Option<Node>> {
    sqlx::query("SELECT * FROM timeline_nodes WHERE session_id = ? AND id = ?")
        .bind(session)
        .bind(id)
        .fetch_optional(conn)
        .await?
        .map(decode)
        .transpose()
}

fn clip(value: &str, limit: usize) -> String {
    let mut end = value.len().min(limit);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

fn bounded(event: &Event, old: Option<&Node>) -> (StreamEvent, String) {
    let mut out = StreamEvent::from_row(event);
    let mut text_bytes = event.text.len() as u64;
    if event.phase.as_deref() == Some("delta") && event.kind != "subagent" {
        if let Some(old) = old {
            text_bytes += old
                .event
                .body_ref
                .as_ref()
                .map_or(old.event.text.len() as u64, |b| b.text_bytes);
            out.text = clip(
                &format!("{}{}", old.event.text, clip(&event.text, 1024)),
                1024,
            );
        }
    }
    let mut data_bytes = out
        .data
        .as_ref()
        .map_or(0, |data| data.to_string().len() as u64);
    if event.phase.as_deref() == Some("delta") && event.kind != "subagent" && out.data.is_none() {
        if let Some(old) = old {
            out.data = old.event.data.clone();
            data_bytes = old.event.body_ref.as_ref().map_or_else(
                || {
                    old.event
                        .data
                        .as_ref()
                        .map_or(0, |data| data.to_string().len() as u64)
                },
                |reference| reference.data_bytes,
            );
        }
    }
    if out.name.is_none() {
        out.name = old.and_then(|n| n.event.name.clone());
    }
    out.tool_summary =
        (event.kind == "tool_call").then(|| crate::events::tool_summary::describe(&out));
    let mut summary = activity(&out);
    if event.kind == "tool_call" && event.phase.as_deref() == Some("delta") {
        if let Some(old) = old.filter(|n| !n.activity.is_empty()) {
            if summary == out.name.as_deref().unwrap_or_default() {
                summary = old.activity.clone();
            }
        }
    }
    out.text = clip(&out.text, 1024);
    if data_bytes > 4096 {
        out.data = crate::storage::history::data_preview(out.data.as_ref());
    }
    out.name = out.name.map(|s| clip(&s, 256));
    if text_bytes > out.text.len() as u64 || data_bytes > 4096 {
        out.body_ref = Some(BodyRef {
            seq: event.seq,
            text_bytes,
            data_bytes,
        });
    }
    out.started_at_ms = old.and_then(|n| n.event.started_at_ms).or_else(|| {
        matches!(event.phase.as_deref(), Some("started" | "delta")).then_some(event.arrival_at_ms)
    });
    if let Some(old) = old {
        out.interior_state = match (
            old.trajectory.as_ref().unwrap_or(&old.event).interior_state,
            out.interior_state,
        ) {
            (Some(current), Some(incoming)) => Some(current.merge(incoming)),
            (Some(current), None) => Some(current),
            (_, incoming) => incoming,
        };
        if out.name.is_none() {
            out.name = old.event.name.clone();
        }
        if out.span.is_none() {
            out.span = old.event.span.clone();
        }
    }
    (out, summary)
}

fn category(event: &StreamEvent) -> (i64, i64) {
    if event.kind != "tool_call" {
        return (0, 0);
    }
    let kind = event
        .tool_summary
        .as_ref()
        .map(|summary| summary.kind.as_str());
    (
        i64::from(kind == Some("read")),
        i64::from(kind == Some("shell")),
    )
}

fn activity(event: &StreamEvent) -> String {
    if event.kind == "subagent" && event.phase.as_deref() == Some("delta") && !event.text.is_empty()
    {
        return clip(&event.text, 120);
    }
    if event.kind == "thinking" {
        return "Thinking".into();
    }
    if let Some(summary) = &event.tool_summary {
        if let Some(command) = &summary.command {
            return format!("Running {command}");
        }
        if let Some(path) = &summary.path {
            return format!("{} {path}", summary.name);
        }
        return summary.summary.clone();
    }
    event.name.clone().unwrap_or_else(|| clip(&event.text, 120))
}

fn failure(event: &StreamEvent) -> i64 {
    let data = event.data.as_ref();
    if data
        .and_then(|d| d.get("closedBy"))
        .and_then(|v| v.as_str())
        == Some("server")
    {
        return 0;
    }
    i64::from(
        event.kind == "error"
            || event.is_error
            || (event.kind == "tool_result"
                && data
                    .and_then(|d| d.get("exitCode"))
                    .and_then(|v| v.as_i64())
                    .is_some_and(|code| code != 0)),
    )
}

async fn refresh_activity(
    conn: &mut SqliteConnection,
    session: &str,
    node: &mut Node,
) -> Result<()> {
    node.activity=sqlx::query_scalar("SELECT activity FROM timeline_nodes WHERE session_id=? AND parent_id=? AND running>0 ORDER BY ordinal,id LIMIT 1")
        .bind(session).bind(&node.id).fetch_optional(conn).await?.unwrap_or_else(||if node.own_running>0 {node.own_activity.clone()}else{String::new()});
    Ok(())
}

async fn save(conn: &mut SqliteConnection, session: &str, node: &Node) -> Result<()> {
    let event = serde_json::to_string(&node.event).map_err(|e| Error::Internal(e.to_string()))?;
    sqlx::query("INSERT INTO timeline_nodes
        (session_id,id,parent_id,turn_id,ordinal,revision,event_json,children,tools,thinking,failed,running,waiting,activity,own_running,declared,trajectory_json,stream_id,kind,own_failed,child_running,reads,commands,structure_error,body_revision,order_tie,body_start_seq,scope_id,unfinished,own_unfinished,own_activity)
        VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(session_id,id) DO UPDATE SET
        parent_id=excluded.parent_id,ordinal=excluded.ordinal,revision=excluded.revision,event_json=excluded.event_json,
        children=excluded.children,tools=excluded.tools,thinking=excluded.thinking,
        failed=excluded.failed,running=excluded.running,waiting=excluded.waiting,activity=excluded.activity,own_running=excluded.own_running,declared=excluded.declared,trajectory_json=excluded.trajectory_json,stream_id=excluded.stream_id,kind=excluded.kind,own_failed=excluded.own_failed,child_running=excluded.child_running,reads=excluded.reads,commands=excluded.commands,structure_error=excluded.structure_error,body_revision=excluded.body_revision,order_tie=excluded.order_tie,body_start_seq=excluded.body_start_seq,scope_id=excluded.scope_id,unfinished=excluded.unfinished,own_unfinished=excluded.own_unfinished,own_activity=excluded.own_activity")
        .bind(session).bind(&node.id).bind(&node.parent_id).bind(&node.turn_id)
        .bind(node.ordinal).bind(node.revision).bind(event).bind(node.children)
        .bind(node.tools).bind(node.thinking).bind(node.failed).bind(node.running)
        .bind(node.waiting).bind(&node.activity).bind(node.own_running).bind(node.declared).bind(node.trajectory.as_ref().map(serde_json::to_string).transpose().map_err(|e| Error::Internal(e.to_string()))?).bind(&node.event.stream_id).bind(&node.event.kind).bind(node.own_failed).bind(node.child_running).bind(node.reads).bind(node.commands).bind(&node.structure_error).bind(node.body_revision).bind(node.order_tie).bind(node.body_start_seq).bind(&node.scope_id).bind(node.unfinished).bind(node.own_unfinished).bind(&node.own_activity).execute(conn).await?;
    Ok(())
}

// The vector is a difference, so a corrected successful result removes a previous
// failure instead of permanently poisoning every ancestor.
async fn propagate(
    conn: &mut SqliteConnection,
    session: &str,
    parent: &str,
    mut delta: [i64; 8],
    child_delta: i64,
    revision: i64,
) -> Result<()> {
    let mut parent = parent.to_owned();
    let mut seen = HashSet::new();
    let mut direct = child_delta;
    while !parent.is_empty() && seen.insert(parent.clone()) {
        let Some(mut node) = get(conn, session, &parent).await? else {
            break;
        };
        node.tools += delta[0];
        node.reads += delta[5];
        node.commands += delta[6];
        node.thinking += delta[1];
        node.failed += delta[2];
        let prior_running = node.running;
        node.child_running += delta[3];
        node.running = node.own_running.max(node.child_running);
        delta[3] = node.running - prior_running;
        node.waiting += delta[4];
        node.unfinished += delta[7];
        node.children += direct;
        node.revision = revision;
        refresh_activity(conn, session, &mut node).await?;
        save(conn, session, &node).await?;
        parent = node.parent_id;
        direct = 0;
    }
    Ok(())
}

fn counts(node: &Node) -> [i64; 8] {
    [
        node.tools,
        node.thinking,
        node.failed,
        node.running,
        node.waiting,
        node.reads,
        node.commands,
        node.unfinished,
    ]
}
fn difference(a: [i64; 8], b: [i64; 8]) -> [i64; 8] {
    std::array::from_fn(|i| a[i] - b[i])
}

async fn insert_shell(
    conn: &mut SqliteConnection,
    session: &str,
    id: String,
    parent: String,
    event: &Event,
    kind: &str,
    revision: i64,
) -> Result<Node> {
    if let Some(node) = get(conn, session, &id).await? {
        return Ok(node);
    }
    let (mut preview, _) = bounded(event, None);
    preview.kind = kind.into();
    preview.text.clear();
    preview.data = None;
    preview.body_ref = None;
    preview.name = if kind == "tool_call" {
        event.name.clone()
    } else {
        None
    };
    preview.phase = None;
    let node = Node {
        id,
        removed: false,
        structure_error: None,
        parent_id: parent,
        turn_id: event.turn_id.clone().unwrap_or_default(),
        ordinal: i64::from(event.seq) * 2,
        order_tie: revision,
        revision,
        body_revision: revision,
        body_start_seq: event.seq,
        event: preview,
        trajectory: None,
        children: 0,
        tools: 0,
        reads: 0,
        commands: 0,
        thinking: 0,
        failed: 0,
        running: 0,
        waiting: 0,
        unfinished: 0,
        scope_id: String::new(),
        own_unfinished: false,
        own_activity: String::new(),
        activity: String::new(),
        own_running: 0,
        child_running: 0,
        own_failed: 0,
        declared: false,
    };
    save(conn, session, &node).await?;
    propagate(conn, session, &node.parent_id, [0; 8], 1, revision).await?;
    Ok(node)
}

async fn address_event(
    conn: &mut SqliteConnection,
    session: &str,
    seq: i32,
    node: &str,
) -> Result<()> {
    sqlx::query("INSERT INTO timeline_event_nodes VALUES (?,?,?) ON CONFLICT(session_id,seq) DO UPDATE SET node_id=excluded.node_id")
        .bind(session).bind(seq).bind(node).execute(conn).await?;
    Ok(())
}

async fn trajectory_address(
    conn: &mut SqliteConnection,
    session: &str,
    stream: &str,
    event: &Event,
    revision: i64,
) -> Result<String> {
    if let Some(id) = sqlx::query_scalar(
        "SELECT node_id FROM timeline_trajectories WHERE session_id=? AND stream_id=?",
    )
    .bind(session)
    .bind(stream)
    .fetch_optional(&mut *conn)
    .await?
    {
        return Ok(id);
    }
    let host: Option<String> = sqlx::query_scalar("SELECT id FROM timeline_nodes WHERE session_id=? AND stream_id=? AND kind='tool_call' ORDER BY ordinal DESC LIMIT 1")
        .bind(session).bind(stream).fetch_optional(&mut *conn).await?;
    let id = host.unwrap_or_else(|| format!("s:{stream}"));
    insert_shell(
        conn,
        session,
        id.clone(),
        String::new(),
        event,
        "subagent",
        revision,
    )
    .await?;
    sqlx::query("UPDATE timeline_nodes SET scope_id=id WHERE session_id=? AND id=?")
        .bind(session)
        .bind(&id)
        .execute(&mut *conn)
        .await?;
    sqlx::query("INSERT INTO timeline_trajectories VALUES (?,?,?)")
        .bind(session)
        .bind(stream)
        .bind(&id)
        .execute(conn)
        .await?;
    Ok(id)
}

async fn execution_group(
    conn: &mut SqliteConnection,
    session: &str,
    parent: &str,
    event: &Event,
    revision: i64,
) -> Result<String> {
    let turn = event.turn_id.as_deref().unwrap_or_default();
    if let Some(group) = sqlx::query_scalar(
        "SELECT group_id FROM timeline_lanes WHERE session_id=? AND turn_id=? AND parent_id=?",
    )
    .bind(session)
    .bind(turn)
    .bind(parent)
    .fetch_optional(&mut *conn)
    .await?
    {
        return Ok(group);
    }
    let group = format!("g:{}", event.seq);
    insert_shell(
        conn,
        session,
        group.clone(),
        parent.to_owned(),
        event,
        "execution",
        revision,
    )
    .await?;
    sqlx::query("INSERT INTO timeline_lanes VALUES (?,?,?,?)")
        .bind(session)
        .bind(turn)
        .bind(parent)
        .bind(&group)
        .execute(conn)
        .await?;
    Ok(group)
}

/// Close only streams whose lifetime ended. Delegated scopes are independent of
/// their arrival turn, so ending a root turn never sweeps a background subtree.
async fn seal_scope(
    conn: &mut SqliteConnection,
    session: &str,
    scope: &str,
    turn: Option<&str>,
    revision: i64,
) -> Result<()> {
    let rows=sqlx::query("SELECT * FROM timeline_nodes WHERE session_id=? AND scope_id=? AND own_running>0 AND id<>? AND (? IS NULL OR turn_id=?)")
        .bind(session).bind(scope).bind(scope).bind(turn).bind(turn).fetch_all(&mut *conn).await?;
    for row in rows {
        let mut node = decode(row)?;
        let before = counts(&node);
        node.own_running = 0;
        node.running = node.child_running;
        node.own_unfinished = true;
        node.unfinished += 1;
        node.revision = revision;
        refresh_activity(conn, session, &mut node).await?;
        save(conn, session, &node).await?;
        propagate(
            conn,
            session,
            &node.parent_id,
            difference(counts(&node), before),
            0,
            revision,
        )
        .await?;
    }
    Ok(())
}

pub(super) async fn seal_turn(conn: &mut SqliteConnection, turn: &str) -> Result<()> {
    let session:Option<String>=sqlx::query_scalar("SELECT session_id FROM timeline_nodes WHERE turn_id=? AND scope_id='' AND own_running>0 LIMIT 1")
        .bind(turn).fetch_optional(&mut *conn).await?;
    if let Some(session) = session {
        let revision = sqlx::query_scalar(
            "UPDATE timeline_clock SET revision=revision+1 WHERE id=1 RETURNING revision",
        )
        .fetch_one(&mut *conn)
        .await?;
        seal_scope(conn, &session, "", Some(turn), revision).await?;
    }
    Ok(())
}

/// Called inside every transaction that accepts an event, before its receipt commits.
pub async fn project(conn: &mut SqliteConnection, event: &Event) -> Result<()> {
    let session = event.session_id.encode();
    let revision: i64 = sqlx::query_scalar(
        "UPDATE timeline_clock SET revision=revision+1 WHERE id=1 RETURNING revision",
    )
    .fetch_one(&mut *conn)
    .await?;
    if event.kind == "turn" {
        return Ok(());
    }
    if event.kind == "user" && event.span.is_none() {
        if let Some(turn) = event.turn_id.as_deref() {
            let matches: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM turns WHERE id=? AND request_text=?)",
            )
            .bind(turn)
            .bind(&event.text)
            .fetch_one(&mut *conn)
            .await?;
            if matches {
                address_event(conn, &session, event.seq, &format!("p:{turn}")).await?;
                return Ok(());
            }
        }
    }
    let mut parent = match event.span.as_deref() {
        Some(span) if event.stream_id.as_deref() != Some(span) => {
            trajectory_address(conn, &session, span, event, revision).await?
        }
        _ => String::new(),
    };
    let lifetime_scope = parent.clone();
    let owner = if parent.is_empty() {
        event.turn_id.clone().unwrap_or_default()
    } else {
        get(conn, &session, &parent)
            .await?
            .map(|n| n.turn_id)
            .unwrap_or_default()
    };
    let mut logical = match (event.kind.as_str(), event.stream_id.as_deref()) {
        ("subagent", Some(stream)) => {
            trajectory_address(conn, &session, stream, event, revision).await?
        }
        (_, Some(stream)) => format!("n:{}:{owner}:{stream}", owner.len()),
        _ => format!("e:{}", event.seq),
    };
    if matches!(event.kind.as_str(), "tool_call" | "tool_result") {
        if let Some(stream) = event.stream_id.as_deref() {
            if let Some(alias)=sqlx::query_scalar("SELECT a.node_id FROM timeline_trajectories a JOIN timeline_nodes n ON n.session_id=a.session_id AND n.id=a.node_id WHERE a.session_id=? AND a.stream_id=? AND n.turn_id=?")
                .bind(&session).bind(stream).bind(&owner).fetch_optional(&mut *conn).await? { logical=alias; }
        }
    }
    let id = if event.kind == "tool_result" {
        format!("r:{logical}")
    } else {
        logical.clone()
    };
    address_event(conn, &session, event.seq, &id).await?;
    let old = get(conn, &session, &id).await?;
    if old.as_ref().is_some_and(|n| {
        n.declared
            && n.trajectory
                .as_ref()
                .map_or(n.event.seq, |t| t.seq.max(n.event.seq))
                > event.seq
    }) {
        return Ok(());
    }
    // A content completion seals its stream, including an unfinished stream
    // sealed by its owner. Preserve the canonical late row, but do not let its
    // delta revise the body or restart activity. Completed corrections and
    // independent trajectory progress have different semantics and still apply.
    if event.kind != "subagent"
        && event.phase.as_deref() == Some("delta")
        && old.as_ref().is_some_and(|node| {
            node.own_unfinished || node.event.phase.as_deref() == Some("completed")
        })
    {
        return Ok(());
    }
    if event.kind == "tool_result" {
        if get(conn, &session, &logical).await?.is_none() {
            parent = execution_group(conn, &session, &parent, event, revision).await?;
        }
        let host = insert_shell(
            conn,
            &session,
            logical.clone(),
            parent.clone(),
            event,
            "tool_call",
            revision,
        )
        .await?;
        parent = host.id;
    } else if let Some(old) = old.as_ref().filter(|n| n.declared) {
        parent = old.parent_id.clone();
    } else if matches!(
        event.kind.as_str(),
        "assistant" | "user" | "status" | "error" | "usage"
    ) {
        sqlx::query("DELETE FROM timeline_lanes WHERE session_id=? AND turn_id=? AND parent_id=?")
            .bind(&session)
            .bind(event.turn_id.as_deref().unwrap_or_default())
            .bind(&parent)
            .execute(&mut *conn)
            .await?;
    } else {
        parent = execution_group(conn, &session, &parent, event, revision).await?;
    }
    let (mut preview, activity_text) = bounded(event, old.as_ref());
    let promoted = event.kind == "subagent"
        && old
            .as_ref()
            .is_some_and(|n| n.declared && n.event.kind == "tool_call");
    let result_received = matches!(event.kind.as_str(), "tool_call" | "subagent")
        && get(conn, &session, &format!("r:{logical}"))
            .await?
            .is_some();
    let abandoned = preview
        .data
        .as_ref()
        .and_then(|d| d.get("closedBy"))
        .and_then(|v| v.as_str())
        == Some("server");
    let report_closed = old
        .as_ref()
        .and_then(|n| n.trajectory.as_ref())
        .is_some_and(|t| t.phase.as_deref() == Some("completed"));
    let own_running = i64::from(
        !abandoned
            && match event.kind.as_str() {
                "tool_call" => !result_received && !report_closed,
                "thinking" | "subagent" => event.phase.as_deref() != Some("completed"),
                _ => false,
            },
    );
    let own_failed = failure(&preview);
    let mut node = old.clone().unwrap_or(Node {
        id,
        removed: false,
        structure_error: None,
        parent_id: parent.clone(),
        turn_id: event.turn_id.clone().unwrap_or_default(),
        ordinal: i64::from(event.seq) * 2,
        order_tie: revision,
        revision,
        body_revision: revision,
        body_start_seq: event.seq,
        event: preview.clone(),
        trajectory: None,
        children: 0,
        tools: 0,
        reads: 0,
        commands: 0,
        thinking: 0,
        failed: 0,
        running: 0,
        waiting: 0,
        unfinished: 0,
        scope_id: String::new(),
        own_unfinished: false,
        own_activity: String::new(),
        activity: String::new(),
        own_running: 0,
        child_running: 0,
        own_failed: 0,
        declared: false,
    });
    // Result nodes own failures; tool invocations own running state. This avoids
    // counting a failed call twice when its result is also a child.
    let unfinished = node.own_unfinished
        && event.phase.as_deref() != Some("completed")
        && event.kind != "subagent";
    node.unfinished += i64::from(unfinished) - i64::from(node.own_unfinished);
    node.own_unfinished = unfinished;
    node.own_running = if node.own_unfinished { 0 } else { own_running };
    node.running = node.own_running.max(node.child_running);
    node.failed += own_failed - node.own_failed;
    node.own_failed = own_failed;
    node.tools += i64::from(event.kind == "tool_call" || promoted)
        - old
            .as_ref()
            .map_or(0, |n| i64::from(n.declared && n.event.kind == "tool_call"));
    node.thinking += i64::from(event.kind == "thinking")
        - old
            .as_ref()
            .map_or(0, |n| i64::from(n.declared && n.event.kind == "thinking"));
    let own_category = category(if promoted {
        &old.as_ref().unwrap().event
    } else {
        &preview
    });
    let previous_category = old
        .as_ref()
        .filter(|n| n.declared)
        .map_or((0, 0), |n| category(&n.event));
    node.reads += own_category.0 - previous_category.0;
    node.commands += own_category.1 - previous_category.1;
    node.own_activity = activity_text;
    node.scope_id = if event.kind == "subagent" || node.scope_id == node.id {
        node.id.clone()
    } else {
        lifetime_scope
    };
    if node.scope_id.is_empty() && node.own_running > 0 {
        let sealed:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM turns WHERE id=? AND status IN ('completed','failed','cancelled','interrupted'))")
            .bind(&node.turn_id).fetch_one(&mut *conn).await?;
        if sealed {
            node.own_running = 0;
            node.running = node.child_running;
            node.own_unfinished = true;
            node.unfinished += 1;
        }
    }
    if promoted {
        node.trajectory = Some(preview);
        preview = old.as_ref().unwrap().event.clone();
    }
    if old.as_ref().is_some_and(|n| n.parent_id != parent) {
        let mut ancestor = parent.clone();
        let mut visited = HashSet::new();
        while !ancestor.is_empty() {
            if ancestor == node.id || !visited.insert(ancestor.clone()) {
                parent.clear();
                node.structure_error =
                    Some("Cyclic delegation was attached to the conversation root.".into());
                break;
            }
            ancestor = get(conn, &session, &ancestor)
                .await?
                .map(|n| n.parent_id)
                .unwrap_or_default();
        }
    }
    if !promoted && (event.phase.as_deref() != Some("delta") || event.kind == "subagent") {
        node.body_start_seq = event.seq;
    }
    node.body_revision = revision;
    node.event = preview;
    node.declared = true;
    node.parent_id = parent.clone();
    node.revision = revision;
    refresh_activity(conn, &session, &mut node).await?;
    save(conn, &session, &node).await?;
    if let Some(previous) = old.as_ref().filter(|n| n.parent_id != parent) {
        let mut removed = previous.clone();
        removed.removed = true;
        removed.revision = revision;
        sqlx::query("INSERT INTO timeline_removals VALUES (?,?,?,?,?) ON CONFLICT(session_id,parent_id,id) DO UPDATE SET revision=excluded.revision,node_json=excluded.node_json")
            .bind(&session).bind(&previous.parent_id).bind(&previous.id).bind(revision)
            .bind(serde_json::to_string(&removed).map_err(|e| Error::Internal(e.to_string()))?).execute(&mut *conn).await?;
        propagate(
            conn,
            &session,
            &previous.parent_id,
            difference([0; 8], counts(previous)),
            -1,
            revision,
        )
        .await?;
        propagate(conn, &session, &parent, counts(&node), 1, revision).await?;
    } else {
        propagate(
            conn,
            &session,
            &parent,
            difference(counts(&node), old.as_ref().map_or([0; 8], counts)),
            i64::from(old.is_none()),
            revision,
        )
        .await?;
    }
    if event.kind == "subagent" && event.phase.as_deref() == Some("completed") {
        seal_scope(conn, &session, &node.id, None, revision).await?;
    }
    if event.kind == "tool_result" {
        if let Some(mut host) = get(conn, &session, &logical).await? {
            if host.own_running > 0 || host.own_unfinished {
                let next = i64::from(
                    host.trajectory
                        .as_ref()
                        .is_some_and(|t| t.phase.as_deref() != Some("completed")),
                );
                let completed = event.phase.as_deref() == Some("completed");
                if !completed {
                    return Ok(());
                }
                let unfinished_delta = if host.own_unfinished { -1 } else { 0 };
                host.unfinished += unfinished_delta;
                host.own_unfinished = false;
                let previous_running = host.running;
                host.own_running = next;
                host.running = host.own_running.max(host.child_running);
                let delta = host.running - previous_running;
                host.revision = revision;
                save(conn, &session, &host).await?;
                propagate(
                    conn,
                    &session,
                    &host.parent_id,
                    [0, 0, 0, delta, 0, 0, 0, unfinished_delta],
                    0,
                    revision,
                )
                .await?;
            }
        }
    }
    Ok(())
}

pub struct ReadWork {
    pub pool_wait_us: u128,
    pub projection_us: u128,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    #[serde(skip)]
    pub work: ReadWork,
    pub parent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_node: Option<Node>,
    pub target_id: Option<String>,
    pub nodes: Vec<Node>,
    pub snapshot: i64,
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

pub(super) async fn body_snapshot(
    conn: &mut SqliteConnection,
    session: &sylph_protocol::SessionId,
    id: &str,
) -> Result<(Node, Vec<Event>, Vec<i32>)> {
    let node = get(conn, &session.encode(), id)
        .await?
        .ok_or(Error::SessionNotFound)?;
    let seqs:Vec<i32>=sqlx::query_scalar("SELECT seq FROM timeline_event_nodes WHERE session_id=? AND node_id=? AND seq>=? AND seq<=? ORDER BY seq")
            .bind(session.encode()).bind(id).bind(node.body_start_seq).bind(node.event.seq).fetch_all(&mut *conn).await?;
    let events=sqlx::query_as::<_,Event>("SELECT e.* FROM timeline_event_nodes m JOIN events e ON e.seq=m.seq AND e.session_env=? AND e.session_agent=? AND e.session_native=? WHERE m.session_id=? AND m.node_id=? AND m.seq>=? AND m.seq<=? AND e.kind=? ORDER BY m.seq")
            .bind(session.environment.as_str()).bind(session.agent.as_str()).bind(session.native_id.as_str()).bind(session.encode()).bind(id).bind(node.body_start_seq).bind(node.event.seq).bind(&node.event.kind).fetch_all(&mut *conn).await?;
    Ok((node, events, seqs))
}

impl super::JournalStore {
    /// Take the logical address, hot fragments and historical addresses from one
    /// journal snapshot. A concurrent archive can duplicate these rows, never
    /// create a hole between reading the node and reading its hot prefix.
    pub async fn node_body_snapshot(
        &self,
        session: &sylph_protocol::SessionId,
        id: &str,
    ) -> Result<(Node, Vec<Event>, Vec<i32>)> {
        let mut tx = self.readers.begin().await?;
        let snapshot = body_snapshot(&mut tx, session, id).await?;
        tx.commit().await?;
        Ok(snapshot)
    }

    pub(crate) async fn native_sync_fault(
        &self,
        session: &sylph_protocol::SessionId,
    ) -> Result<bool> {
        Ok(sqlx::query_scalar::<_,bool>("SELECT COALESCE(json_extract(event_json,'$.isError'),0) FROM timeline_nodes WHERE session_id=? AND id='n:0::native-sync'")
            .bind(session.encode()).fetch_optional(&self.readers).await?.unwrap_or(false))
    }

    pub async fn node_page(
        &self,
        session: &str,
        parent: &str,
        cursor: Option<&str>,
        changes: bool,
        limit: i64,
    ) -> Result<Page> {
        self.node_page_inner(session, parent, cursor, changes, limit, None)
            .await
    }
    pub async fn node_window(&self, session: &str, seq: i32) -> Result<Page> {
        self.node_page_inner(session, "", None, false, PAGE_NODES, Some(seq))
            .await
    }
    async fn node_page_inner(
        &self,
        session: &str,
        parent: &str,
        cursor: Option<&str>,
        changes: bool,
        limit: i64,
        around: Option<i32>,
    ) -> Result<Page> {
        let waiting = std::time::Instant::now();
        let mut tx = self.readers.begin().await?;
        let pool_wait_us = waiting.elapsed().as_micros();
        let reading = std::time::Instant::now();
        let snapshot: i64 = sqlx::query_scalar("SELECT revision FROM timeline_clock WHERE id=1")
            .fetch_one(&mut *tx)
            .await?;
        let target: Option<(String, String, i64)> = if let Some(seq) = around {
            Some(sqlx::query_as("SELECT n.id,n.parent_id,n.ordinal FROM timeline_event_nodes m JOIN timeline_nodes n ON n.session_id=m.session_id AND n.id=m.node_id WHERE m.session_id=? AND m.seq=?")
                .bind(session).bind(seq).fetch_optional(&mut *tx).await?.ok_or(Error::SessionNotFound)?)
        } else {
            None
        };
        let target_cursor = target
            .as_ref()
            .map(|(_, _, ordinal)| format!("{}:{}:", ordinal + 1, i64::MAX));
        let parent = target
            .as_ref()
            .map_or(parent, |(_, parent, _)| parent.as_str());
        let cursor = target_cursor.as_deref().or(cursor);
        let parent_node = if parent.is_empty() {
            None
        } else {
            get(&mut tx, session, parent).await?
        };
        let invalid = || Error::InvalidRequest("invalid node cursor".into());
        let (key, tie, id) = if let Some(cursor) = cursor {
            let (key, rest) = cursor.split_once(':').ok_or_else(invalid)?;
            let key = key.parse::<i64>().map_err(|_| invalid())?;
            if changes {
                (key, 0, rest.to_owned())
            } else {
                let (tie, id) = rest.split_once(':').ok_or_else(invalid)?;
                (
                    key,
                    tie.parse::<i64>().map_err(|_| invalid())?,
                    id.to_owned(),
                )
            }
        } else {
            (if changes { 0 } else { i64::MAX }, i64::MAX, String::new())
        };
        let sql = if changes { CHANGES_SQL } else { CHILDREN_SQL };
        let limit = limit.clamp(1, PAGE_NODES) as usize;
        let query = sqlx::query(sql).bind(session).bind(parent).bind(key);
        let query = if changes { query } else { query.bind(tie) };
        let rows = query
            .bind(&id)
            .bind(limit as i64 + 1)
            .fetch_all(&mut *tx)
            .await?;
        let mut candidates = rows.into_iter().map(decode).collect::<Result<Vec<_>>>()?;
        if changes {
            let removals: Vec<String> = sqlx::query_scalar("SELECT node_json FROM timeline_removals WHERE session_id=? AND parent_id=? AND (revision,id)>(?,?) ORDER BY revision,id LIMIT ?")
                .bind(session).bind(parent).bind(key).bind(&id).bind(limit as i64+1).fetch_all(&mut *tx).await?;
            for json in removals {
                candidates.push(
                    serde_json::from_str(&json)
                        .map_err(|e| Error::Internal(format!("invalid timeline removal: {e}")))?,
                );
            }
            candidates.sort_by(|a, b| (a.revision, &a.id).cmp(&(b.revision, &b.id)));
        }
        let mut nodes = Vec::new();
        let mut bytes = 256
            + serde_json::to_vec(&parent_node)
                .map_err(|e| Error::Internal(e.to_string()))?
                .len()
            + serde_json::to_vec(parent)
                .map_err(|e| Error::Internal(e.to_string()))?
                .len()
            + serde_json::to_vec(&target.as_ref().map(|(id, _, _)| id))
                .map_err(|e| Error::Internal(e.to_string()))?
                .len();
        let mut has_more = false;
        for node in candidates {
            let size = serde_json::to_vec(&node)
                .map_err(|e| Error::Internal(e.to_string()))?
                .len()
                + 1;
            let cursor_bytes = serde_json::to_vec(&format!(
                "{}:{}:{}",
                node.revision.max(node.ordinal),
                node.order_tie,
                node.id
            ))
            .map_err(|e| Error::Internal(e.to_string()))?
            .len();
            if nodes.len() >= limit || bytes + size + cursor_bytes > PAGE_BYTES {
                if nodes.is_empty() {
                    return Err(Error::Internal("timeline node exceeds page budget".into()));
                }
                has_more = true;
                break;
            }
            bytes += size;
            nodes.push(node);
        }
        let next_cursor = if has_more {
            nodes.last().map(|n| {
                if changes {
                    format!("{}:{}", n.revision, n.id)
                } else {
                    format!("{}:{}:{}", n.ordinal, n.order_tie, n.id)
                }
            })
        } else if changes {
            Some(format!("{}:", snapshot + 1))
        } else {
            None
        };
        // The next change starts after the complete revision, including any id.
        // Use snapshot+1 with an empty id; no node can have that revision yet in
        // this read snapshot, and the strict tuple comparison includes future ids.
        if !changes {
            nodes.reverse();
        }
        tx.commit().await?;
        Ok(Page {
            work: ReadWork {
                pool_wait_us,
                projection_us: reading.elapsed().as_micros(),
            },
            parent: parent.to_owned(),
            parent_node,
            target_id: target.map(|(id, _, _)| id),
            nodes,
            snapshot,
            next_cursor,
            has_more,
        })
    }
}
