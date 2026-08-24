//! Link tests against a scripted adapter.
//!
//! The fake adapter is a real process speaking real NDJSON over a real duplex
//! pipe, not a mock of the transport. The bug this design exists to fix was
//! precisely a transport bug — closing stdin — and a test that stubs the
//! transport cannot see it.

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use tokio::sync::mpsc::Receiver;

use super::link::{Link, LinkConfig, LinkEvent};
use super::wire::Capability;
use crate::env::LocalEnvironment;

/// Write an executable adapter from a Python body. Python rather than shell
/// because these scripts parse JSON and hold a read loop, which is exactly what
/// bash does badly.
fn write_adapter(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("adapter.sh");
    std::fs::write(&path, format!("#!/usr/bin/env python3\n{body}")).unwrap();
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    path
}

/// A well-behaved adapter: handshakes, opens, and answers `send`.
///
/// `extra` is spliced into the message loop so a test can add one behaviour
/// without restating the protocol.
fn cooperative_adapter(extra: &str) -> String {
    format!(
        r#"
import json, sys

def out(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()

for raw in sys.stdin:
    raw = raw.strip()
    if not raw:
        continue
    msg = json.loads(raw)
    t, mid = msg.get("t"), msg.get("id")

    if t == "hello":
        out({{"t": "ok", "id": mid, "adapter": "fake/1.0.0",
              "agent": {{"name": "fake", "version": "9.9"}},
              "supports": ["interrupt", "steer", "approval", "slash", "unknown_future_thing"],
              "flags": {{"model": {{"choices": ["fast", "slow"], "description": "which"}}}}}})
    elif t == "open":
        out({{"t": "ok", "id": mid, "sessionId": "native-abc",
              "commands": [{{"name": "verify", "description": "check it", "source": "skill"}},
                           {{"name": "compact", "description": "condense"}}],
              "models": [{{"id": "fast", "label": "Fast"}}],
              "permissionMode": msg.get("permissionMode") or "default",
              "contextWindow": 200000,
              "echoSoul": msg.get("soul", "<unset>"),
              "echoSessionId": msg.get("sessionId")}})
    elif t == "close":
        out({{"t": "ok", "id": mid}})
        break
{extra}
    else:
        out({{"t": "ok", "id": mid}})
"#
    )
}

async fn establish(adapter: PathBuf, config: impl FnOnce(&mut LinkConfig)) -> (Link, Receiver<LinkEvent>) {
    let env = LocalEnvironment::new();
    let mut link_config = LinkConfig {
        adapter_path: adapter,
        workdir: "/tmp".to_string(),
        session_id: None,
        soul: None,
        flags: HashMap::new(),
        permission_mode: None,
        env: HashMap::new(),
    };
    config(&mut link_config);
    Link::establish(&env, link_config).await.expect("link must establish")
}

#[tokio::test]
async fn the_handshake_reports_the_agent_and_its_declared_capabilities() {
    let tmp = TempDir::new().unwrap();
    let adapter = write_adapter(tmp.path(), &cooperative_adapter(""));

    let (link, _events) = establish(adapter, |_| {}).await;

    let identity = link.identity();
    assert_eq!(identity.agent_name, "fake");
    assert_eq!(identity.agent_version.as_deref(), Some("9.9"));
    assert_eq!(identity.adapter, "fake/1.0.0");
    assert!(identity.can(Capability::Interrupt));
    assert!(identity.can(Capability::Approval));
    // Declared nothing about forking, so it must not be assumed capable.
    assert!(!identity.can(Capability::Fork));
    assert_eq!(identity.flags["model"].choices, vec!["fast", "slow"]);
}

/// One unmodelled capability name must not discard the ones beside it, or a
/// newer adapter would lose abilities by mentioning a newer one.
#[tokio::test]
async fn an_unknown_capability_name_does_not_discard_the_known_ones() {
    let tmp = TempDir::new().unwrap();
    let adapter = write_adapter(tmp.path(), &cooperative_adapter(""));

    let (link, _events) = establish(adapter, |_| {}).await;

    assert!(link.identity().can(Capability::Steer));
    assert!(link.identity().can(Capability::Slash));
}

/// The payoff: a live command list from the agent, not from a descriptor an
/// adapter author maintained by hand.
#[tokio::test]
async fn opening_a_session_yields_the_agents_own_command_list() {
    let tmp = TempDir::new().unwrap();
    let adapter = write_adapter(tmp.path(), &cooperative_adapter(""));

    let (link, _events) = establish(adapter, |_| {}).await;

    let caps = link.capabilities().await;
    let names: Vec<&str> = caps.commands.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, vec!["verify", "compact"]);
    assert_eq!(caps.commands[0].source.as_deref(), Some("skill"));
    assert_eq!(caps.context_window, Some(200000));
    assert_eq!(link.native_session_id().await.as_deref(), Some("native-abc"));
}

/// An adapter that records every line it received, so a test can assert on what
/// actually crossed the wire rather than on a value read back through an API
/// that might normalize it.
fn recording_adapter(log: &Path) -> String {
    let mut body = cooperative_adapter("");
    body = body.replace(
        "    msg = json.loads(raw)",
        &format!(
            "    msg = json.loads(raw)\n    \
             open({:?}, 'a').write(raw + chr(10))",
            log.to_string_lossy()
        ),
    );
    // Also record the adapter's environment once, since identity injection is
    // carried both as a field and as SYLPH_SOUL.
    body.replace(
        "import json, sys",
        &format!(
            "import json, os, sys\n\
             open({:?}, 'a').write(json.dumps({{'env_soul': os.environ.get('SYLPH_SOUL', '<unset>')}}) + chr(10))",
            log.to_string_lossy()
        ),
    )
}

fn recorded(log: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// The native-session invariant, enforced on the wire: an empty soul must reach
/// the adapter as an empty string, which the protocol reserves for "inject
/// nothing". An omitted field would mean "resolve your default", which for a
/// preserved transcript is the difference between honouring the invariant and
/// silently violating it.
#[tokio::test]
async fn an_empty_soul_reaches_the_adapter_as_an_empty_string_not_as_an_absence() {
    let tmp = TempDir::new().unwrap();
    let log = tmp.path().join("received.jsonl");
    let adapter = write_adapter(tmp.path(), &recording_adapter(&log));

    let (link, _events) = establish(adapter, |config| {
        config.soul = Some(String::new());
    })
    .await;
    link.close().await;

    let lines = recorded(&log);
    let open = lines
        .iter()
        .find(|l| l.get("t").and_then(|t| t.as_str()) == Some("open"))
        .expect("the open message must have been received");
    assert_eq!(open["soul"], "", "an empty soul is a value, not a missing field");

    // Carried in the environment too, matching the v1 convention so an adapter
    // author has one rule rather than two.
    let env_line = lines.iter().find(|l| l.get("env_soul").is_some()).unwrap();
    assert_eq!(env_line["env_soul"], "");
}

#[tokio::test]
async fn a_resumed_session_sends_the_native_id_and_a_fresh_one_sends_null() {
    let tmp = TempDir::new().unwrap();
    let log = tmp.path().join("received.jsonl");
    let adapter = write_adapter(tmp.path(), &recording_adapter(&log));

    let (link, _events) = establish(adapter.clone(), |config| {
        config.session_id = Some("resume-me".into());
    })
    .await;
    link.close().await;

    let open = recorded(&log)
        .into_iter()
        .find(|l| l.get("t").and_then(|t| t.as_str()) == Some("open"))
        .unwrap();
    assert_eq!(open["sessionId"], "resume-me");

    let fresh_log = tmp.path().join("fresh.jsonl");
    let fresh_adapter = write_adapter(
        &std::fs::create_dir(tmp.path().join("fresh")).map(|_| tmp.path().join("fresh")).unwrap(),
        &recording_adapter(&fresh_log),
    );
    let (link, _events) = establish(fresh_adapter, |_| {}).await;
    link.close().await;

    let open = recorded(&fresh_log)
        .into_iter()
        .find(|l| l.get("t").and_then(|t| t.as_str()) == Some("open"))
        .unwrap();
    assert!(open["sessionId"].is_null(), "a fresh session is an explicit null");
}

async fn next_event(events: &mut Receiver<LinkEvent>) -> LinkEvent {
    tokio::time::timeout(std::time::Duration::from_secs(10), events.recv())
        .await
        .expect("an event must arrive")
        .expect("the channel must stay open")
}

/// The whole reason for v2. A turn raises an approval, the server answers, and
/// the turn proceeds — impossible on a pipe whose stdin was closed to signal the
/// end of the prompt.
#[tokio::test]
async fn a_turn_can_raise_an_approval_be_answered_and_then_continue() {
    let tmp = TempDir::new().unwrap();
    let adapter = write_adapter(
        tmp.path(),
        &cooperative_adapter(
            r#"
    elif t == "send":
        out({"t": "ok", "id": mid})
        out({"t": "request", "id": "r1", "kind": "approval", "tool": "Bash",
             "title": "Run a shell command",
             "detail": {"command": "rm -rf build"},
             "options": [{"id": "allow", "label": "Allow once"},
                         {"id": "deny", "label": "Deny"}],
             "default": "deny", "risk": "destructive"})
    elif t == "resolve":
        # The answer arrives on the same held-open stdin the prompt came in on.
        out({"t": "event", "kind": "tool_result", "phase": "completed",
             "text": "resolved as " + msg.get("option", "?"),
             "echoedInput": msg.get("input")})
        out({"t": "event", "kind": "turn", "phase": "completed"})
"#,
        ),
    );

    let (link, mut events) = establish(adapter, |_| {}).await;
    link.send("clean the build".into(), vec![], None, HashMap::new())
        .await
        .unwrap();

    let LinkEvent::Request { id, request } = next_event(&mut events).await else {
        panic!("an approval must arrive as a request");
    };
    assert_eq!(request.kind, "approval");
    assert_eq!(request.tool.as_deref(), Some("Bash"));
    assert_eq!(request.risk.as_deref(), Some("destructive"));
    assert_eq!(request.detail.as_ref().unwrap()["command"], "rm -rf build");

    // Answered with edited input: "allow, but change the command" is a real
    // terminal affordance, and a yes/no dialog would drop it.
    link.resolve(&id, "allow", Some(serde_json::json!({"command": "rm -rf build/tmp"})))
        .await
        .unwrap();

    let LinkEvent::Event(body) = next_event(&mut events).await else {
        panic!("the turn must continue after the answer");
    };
    assert_eq!(body["text"], "resolved as allow");
    assert_eq!(body["echoedInput"]["command"], "rm -rf build/tmp");
}

/// A capability list that changes while the session runs — a skill edited, a
/// plugin loaded — is pushed, not polled. A polled list is wrong for however
/// long the poll interval is.
#[tokio::test]
async fn a_capability_change_updates_the_cached_surface_and_is_published() {
    let tmp = TempDir::new().unwrap();
    let adapter = write_adapter(
        tmp.path(),
        &cooperative_adapter(
            r#"
    elif t == "send":
        out({"t": "ok", "id": mid})
        out({"t": "event", "kind": "capabilities",
             "capabilities": {"commands": [{"name": "newly-added"}],
                              "permissionMode": "plan"}})
"#,
        ),
    );

    let (link, mut events) = establish(adapter, |_| {}).await;
    assert_eq!(link.capabilities().await.commands.len(), 2);

    link.send("go".into(), vec![], None, HashMap::new()).await.unwrap();

    let LinkEvent::Capabilities(surface) = next_event(&mut events).await else {
        panic!("a capability change must surface as its own variant");
    };
    assert_eq!(surface.commands[0].name, "newly-added");
    assert_eq!(surface.permission_mode.as_deref(), Some("plan"));
    // Cached too, so a client asking later gets the new list without waiting for
    // another push.
    assert_eq!(link.capabilities().await.commands[0].name, "newly-added");
}

/// Capability is a promise the server holds the adapter to. Sending interrupt to
/// an adapter that never declared it would block on a reply that never comes, so
/// it is refused locally instead.
#[tokio::test]
async fn interrupt_is_refused_when_the_adapter_never_declared_it() {
    let tmp = TempDir::new().unwrap();
    let mut body = cooperative_adapter("");
    body = body.replace(
        r#""supports": ["interrupt", "steer", "approval", "slash", "unknown_future_thing"],"#,
        r#""supports": [],"#,
    );
    let adapter = write_adapter(tmp.path(), &body);

    let (link, _events) = establish(adapter, |_| {}).await;

    let error = link.interrupt().await.expect_err("must refuse");
    assert!(
        format!("{error:#}").contains("cannot interrupt"),
        "the refusal should name what is unsupported: {error:#}"
    );
}

#[tokio::test]
async fn an_adapter_that_exits_mid_turn_reports_a_close_with_its_stderr_tail() {
    let tmp = TempDir::new().unwrap();
    let adapter = write_adapter(
        tmp.path(),
        &cooperative_adapter(
            r#"
    elif t == "send":
        out({"t": "ok", "id": mid})
        sys.stderr.write("fatal: the model went away\n")
        sys.stderr.flush()
        sys.exit(7)
"#,
        ),
    );

    let (link, mut events) = establish(adapter, |_| {}).await;
    link.send("go".into(), vec![], None, HashMap::new()).await.unwrap();

    let LinkEvent::Closed { exit_code, stderr_tail } = next_event(&mut events).await else {
        panic!("the link must report its own death");
    };
    assert_eq!(exit_code, Some(7));
    // Without the tail, "the agent stopped" and "the agent finished" are
    // indistinguishable in a transcript.
    assert!(
        stderr_tail.contains("the model went away"),
        "stderr is the only diagnostic left: {stderr_tail}"
    );
}

/// A caller waiting on a reply must not wait forever because the process died.
#[tokio::test]
async fn a_command_outstanding_when_the_adapter_dies_fails_rather_than_hanging() {
    let tmp = TempDir::new().unwrap();
    let adapter = write_adapter(
        tmp.path(),
        &cooperative_adapter(
            r#"
    elif t == "send":
        # Accept nothing, just die: the caller is left waiting on a reply.
        sys.exit(1)
"#,
        ),
    );

    let (link, _events) = establish(adapter, |_| {}).await;

    let error = link
        .send("go".into(), vec![], None, HashMap::new())
        .await
        .expect_err("a dead adapter cannot accept a turn");
    let message = format!("{error:#}");
    assert!(
        message.contains("exited") || message.contains("no longer accepting"),
        "the failure should say the adapter is gone: {message}"
    );
}

#[tokio::test]
async fn an_adapter_that_never_handshakes_fails_to_establish_and_leaves_no_process() {
    let tmp = TempDir::new().unwrap();
    // Reads its stdin forever and answers nothing.
    let adapter = write_adapter(
        tmp.path(),
        "import sys\nfor line in sys.stdin:\n    pass\n",
    );

    let env = LocalEnvironment::new();
    let result = Link::establish(
        &env,
        LinkConfig {
            adapter_path: adapter,
            workdir: "/tmp".into(),
            session_id: None,
            soul: None,
            flags: HashMap::new(),
            permission_mode: None,
            env: HashMap::new(),
        },
    )
    .await;

    assert!(result.is_err(), "a silent adapter must not yield a usable link");
}

#[tokio::test]
async fn an_adapter_whose_handshake_omits_its_agent_name_is_rejected() {
    let tmp = TempDir::new().unwrap();
    let body = cooperative_adapter("").replace(
        r#""agent": {"name": "fake", "version": "9.9"},"#,
        r#""agent": {},"#,
    );
    let adapter = write_adapter(tmp.path(), &body);

    let env = LocalEnvironment::new();
    let result = Link::establish(
        &env,
        LinkConfig {
            adapter_path: adapter,
            workdir: "/tmp".into(),
            session_id: None,
            soul: None,
            flags: HashMap::new(),
            permission_mode: None,
            env: HashMap::new(),
        },
    )
    .await;

    let error = result.err().expect("must reject a nameless agent");
    assert!(format!("{error:#}").contains("agent name"));
}

/// Diagnostics on stdout are allowed by the protocol, so a chatty adapter must
/// not break correlation.
#[tokio::test]
async fn plain_diagnostic_output_on_stdout_does_not_disturb_the_protocol() {
    let tmp = TempDir::new().unwrap();
    let mut body = cooperative_adapter("");
    body = body.replace(
        "    if t == \"hello\":",
        "    print('loading configuration...', flush=True)\n    if t == \"hello\":",
    );
    let adapter = write_adapter(tmp.path(), &body);

    let (link, _events) = establish(adapter, |_| {}).await;
    assert_eq!(link.identity().agent_name, "fake");
}
