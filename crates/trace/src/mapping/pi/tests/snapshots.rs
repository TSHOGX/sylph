use super::*;
use crate::reducer::Resolved;

#[derive(Default)]
struct Harness {
    state: MapperState,
    archive: ArchiveContext,
    outputs: Vec<MapOutput>,
    ordinal: u64,
    generation: u64,
}
impl Harness {
    fn feed(&mut self, outgoing: bool, frame: Value) {
        let mut capture = capture(self.ordinal, true);
        capture.source_position.generation = self.generation;
        self.ordinal += 1;
        if outgoing {
            capture.transport_kind = TransportKind::LiveStdin;
        }
        let output = PiMapper.map(MapInput {
            capture: &capture,
            canonical_observation: &capture.observation_ref,
            raw: &serde_json::to_vec(&frame).unwrap(),
            state: &self.state,
            archive: &self.archive,
        });
        self.state = serde_json::from_slice(&serde_json::to_vec(&output.state).unwrap()).unwrap();
        self.archive.observations.insert(capture.observation_ref);
        self.outputs.push(output);
    }
    fn request(&mut self, id: &str, since: Option<&str>) {
        let mut frame = serde_json::json!({"id":id,"type":"get_entries"});
        if let Some(since) = since {
            frame["since"] = since.into();
        }
        self.feed(true, frame);
    }
    fn response(&mut self, id: &str, entries: Vec<Value>, leaf: &str) {
        self.feed(false, serde_json::json!({"id":id,"type":"response","command":"get_entries","success":true,"data":{"entries":entries,"leafId":leaf}}));
    }
    fn selected(&self) -> Vec<String> {
        let reduced = reduce(&self.outputs);
        let owner = self
            .outputs
            .iter()
            .find_map(|output| output.state.governing_agent.clone())
            .unwrap();
        let history = &reduced.histories[&owner];
        let selection = history.selection.known().unwrap();
        history
            .selected_entries
            .as_ref()
            .unwrap()
            .iter()
            .map(|entry| {
                let content = reduced.entry_contents(entry, Some(selection));
                let [HistoryContent::Message(revision)] = content
                    .slots
                    .get(&0)
                    .and_then(Resolved::known)
                    .expect("selected ancestor lost its observed content")
                    .as_slice()
                else {
                    panic!("expected selected message");
                };
                let TraceObject::MessageRevision(revision) = reduced
                    .object(&AnyObjectRef::MessageRevision(revision.clone()))
                    .unwrap()
                    .declaration
                    .known()
                    .unwrap()
                else {
                    unreachable!()
                };
                let payload = &revision.parts[0].payload;
                let bytes = &self
                    .outputs
                    .iter()
                    .flat_map(|output| &output.payloads)
                    .find(|body| &body.reference == payload)
                    .unwrap()
                    .bytes;
                String::from_utf8(bytes.clone()).unwrap()
            })
            .collect()
    }
    fn scope(&self) -> &HistoryContentScope {
        self.outputs
            .last()
            .unwrap()
            .claims
            .iter()
            .find_map(|claim| match &claim.fact {
                Fact::Declare {
                    object: TraceObject::HistorySelection(selection),
                } => Some(&selection.content_scope),
                _ => None,
            })
            .expect("snapshot selection")
    }
    fn assert_unpaired(&self) {
        assert!(
            matches!(self.scope(), HistoryContentScope::ObservationSet { .. }),
            "an invalid continuation cannot inherit a baseline"
        );
        let reduced = reduce(&self.outputs);
        let owner = self.state.governing_agent.as_ref().unwrap();
        let selection = reduced.histories[owner].selection.known().unwrap();
        let ancestor = EntryRef::native(owner.clone(), "pi.entry", vec!["a".into()]).unwrap();
        assert!(
            reduced
                .entry_contents(&ancestor, Some(selection))
                .slots
                .is_empty(),
            "unpaired response borrowed the baseline body"
        );
    }
}
fn entry(id: &str, parent: Option<&str>, body: &str) -> Value {
    serde_json::json!({"type":"message","id":id,"parentId":parent,"message":{"role":"user","content":body}})
}
fn baseline() -> Harness {
    let mut h = Harness::default();
    h.feed(false, state());
    h.request("base", None);
    h.response("base", vec![entry("a", None, "baseline bytes")], "a");
    h
}

#[test]
fn paired_append_and_empty_snapshots_preserve_baseline_content_and_sibling_navigation() {
    let mut h = baseline();
    assert_eq!(h.selected(), ["baseline bytes"]);
    h.request("append", Some("a"));
    h.response("append", vec![entry("b", Some("a"), "branch B")], "b");
    assert_eq!(h.selected(), ["baseline bytes", "branch B"]);
    h.request("empty", Some("b"));
    h.response("empty", vec![], "b");
    assert_eq!(h.selected(), ["baseline bytes", "branch B"]);
    h.request("branch", Some("b"));
    h.response("branch", vec![entry("c", Some("a"), "branch C")], "c");
    assert_eq!(h.selected(), ["baseline bytes", "branch C"]);
    h.request("navigate", Some("c"));
    h.response("navigate", vec![], "b");
    assert_eq!(h.selected(), ["baseline bytes", "branch B"]);
    assert!(
        !h.outputs
            .iter()
            .flat_map(|output| &output.claims)
            .any(|claim| matches!(
                claim.fact,
                Fact::Declare {
                    object: TraceObject::Delivery(_) | TraceObject::Activation(_)
                }
            ))
    );
}

#[test]
fn snapshot_continuation_requires_its_request_cursor_and_uninterrupted_source() {
    for fault in [
        "missing",
        "wrong-id",
        "wrong-cursor",
        "gap",
        "generation",
        "duplicate",
        "rejected",
        "switched",
        "overlap",
    ] {
        let mut h = baseline();
        match fault {
            "missing" => {}
            "wrong-cursor" => h.request("next", Some("unobserved")),
            _ => h.request("next", Some("a")),
        }
        match fault {
            "gap" => h.ordinal += 1,
            "generation" => h.generation += 1,
            "duplicate" => h.request("next", Some("a")),
            "rejected" => h.feed(false, serde_json::json!({"id":"next","type":"response","command":"get_entries","success":false,"error":"missing cursor"})),
            "switched" => h.feed(true, serde_json::json!({"id":"switch","type":"switch_session","sessionPath":"other"})),
            "overlap" => {
                h.request("other", Some("a"));
                h.response("other", vec![entry("x", Some("a"), "overlapping reply")], "x");
            },
            _ => {},
        }
        h.response(
            if fault == "wrong-id" {
                "unpaired"
            } else {
                "next"
            },
            vec![entry("b", Some("a"), "new bytes")],
            "b",
        );
        h.assert_unpaired();
        h.request("recover", None);
        h.response(
            "recover",
            vec![
                entry("a", None, "fresh baseline"),
                entry("b", Some("a"), "new bytes"),
            ],
            "b",
        );
        assert_eq!(h.selected(), ["fresh baseline", "new bytes"], "{fault}");
    }
}

#[test]
fn complete_snapshots_reset_content_and_tree_order_cannot_supply_an_append_cursor() {
    let mut h = baseline();
    h.request("full", None);
    h.response("full", vec![entry("a", None, "changed baseline")], "a");
    assert_eq!(h.selected(), ["changed baseline"]);
    assert!(matches!(
        h.scope(),
        HistoryContentScope::Snapshot { previous: None, .. }
    ));
    h.feed(true, serde_json::json!({"id":"tree","type":"get_tree"}));
    h.feed(false, serde_json::json!({"id":"tree","type":"response","command":"get_tree","success":true,"data":{"tree":[{"entry":entry("a",None,"tree bytes"),"children":[]}],"leafId":"a"}}));
    assert_eq!(h.selected(), ["tree bytes"]);
    h.request("no-cursor", Some("unobserved"));
    h.response("no-cursor", vec![entry("b", Some("a"), "new bytes")], "b");
    h.assert_unpaired();
}

#[test]
fn snapshot_checkpoints_and_claims_do_not_accumulate_prior_poll_contents() {
    let mut h = baseline();
    let initial_bytes = serde_json::to_vec(&h.state).unwrap().len();
    for index in 0..200 {
        h.request(&format!("empty-{index:04}"), Some("a"));
        h.response(&format!("empty-{index:04}"), vec![], "a");
        assert!(serde_json::to_vec(&h.state).unwrap().len() <= initial_bytes + 256);
        assert!(serde_json::to_vec(h.scope()).unwrap().len() < 2048);
    }
    assert_eq!(h.selected(), ["baseline bytes"]);
    for index in 0..super::super::snapshots::MAX_PENDING * 3 {
        h.request(&format!("unanswered-{index:04}"), Some("a"));
        let state: PiState = serde_json::from_value(h.state.values["pi"].clone()).unwrap();
        assert!(
            serde_json::to_value(&state.snapshots).unwrap()["pending"]
                .as_object()
                .unwrap()
                .len()
                <= super::super::snapshots::MAX_PENDING
        );
    }
}
