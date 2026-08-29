//! The OpenAPI schema describes exactly the routes the router registers.
//!
//! # Why this is a test and not a script
//!
//! `docs/roadmap.md` recorded route coverage as checked by "a ten-line script diffing
//! `.route(...)` calls against the spec", run by hand. It was not run, and
//! `POST /sessions/{id}/queue` shipped implemented, called by the frontend, backed by
//! `Capability::Queue`, and absent from the schema, `api-design.md`,
//! `session-protocol.md`, and `architecture.md`. A check nobody runs is not a check.
//!
//! # Both sides are derived, neither is listed
//!
//! `AGENTS.md` § A hand-maintained list inside a test defeated the test is the reason
//! this parses `lib.rs` rather than enumerating expected routes:
//! `every_emitted_code_is_published_in_the_openapi_enum` read the schema from disk —
//! correctly — and still missed six codes, because the list of *variants* it compared
//! against was itself maintained by hand. A route-coverage test that names the routes it
//! expects reproduces that exactly: the route someone forgot to document is the route
//! they would also forget to add here.
//!
//! So the implemented side comes from the router source and the documented side from the
//! schema, and the assertion is that the two sets are equal. Parsing Rust with a regex is
//! ordinarily a poor idea; it is sound here because the input is one function written in
//! one shape, and because a parse that silently found nothing would fail the count
//! assertion below rather than pass.

use std::collections::{BTreeMap, BTreeSet};

/// Path parameters are named differently in the two sources — `{id}` in the router,
/// `{sessionId}`/`{turnId}`/`{requestId}` in the schema — so a comparison has to be
/// blind to the name. The *position* is what matters, and that is preserved.
fn normalize(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut in_param = false;
    for ch in path.chars() {
        match ch {
            '{' => {
                in_param = true;
                out.push_str("{}");
            }
            '}' => in_param = false,
            _ if in_param => {}
            _ => out.push(ch),
        }
    }
    out
}

/// Every `<METHOD> <path>` pair the router registers.
fn implemented() -> BTreeSet<String> {
    let source = include_str!("../src/lib.rs");
    let router = source
        .split_once("pub fn router")
        .expect("lib.rs declares `pub fn router`")
        .1;

    let mut pairs = BTreeSet::new();
    // `.route("<path>", <handler chain>)` — the chain runs to the next `.route(` or to
    // the end of the builder, and carries one or more `get(...)`/`post(...)` calls.
    for chunk in router.split(".route(").skip(1) {
        let Some(open) = chunk.find('"') else { continue };
        let Some(len) = chunk[open + 1..].find('"') else {
            continue;
        };
        let path = &chunk[open + 1..open + 1 + len];
        if !path.starts_with("/api/") {
            continue;
        }

        for verb in ["get", "post", "patch", "delete", "put"] {
            // `.get(` and `get(` both appear — the first in a chained pair such as
            // `post(attach).delete(detach)`. A bare `get` inside a handler name would
            // be a false positive, so the paren is required.
            let needle = format!("{verb}(");
            if chunk[open + 1 + len..].contains(&needle) {
                pairs.insert(format!("{} {}", verb.to_uppercase(), normalize(path)));
            }
        }
    }
    pairs
}

/// Every `<METHOD> <path>` pair the schema declares.
fn documented() -> BTreeSet<String> {
    let raw = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/api-v1.openapi.json"),
    )
    .expect("the OpenAPI schema is part of the repo");
    let schema: serde_json::Value =
        serde_json::from_str(&raw).expect("the OpenAPI schema is valid JSON");

    let mut pairs = BTreeSet::new();
    for (path, operations) in schema["paths"]
        .as_object()
        .expect("the schema declares paths")
    {
        for method in operations
            .as_object()
            .expect("a path holds operations")
            .keys()
        {
            if matches!(method.as_str(), "get" | "post" | "patch" | "delete" | "put") {
                pairs.insert(format!("{} {}", method.to_uppercase(), normalize(path)));
            }
        }
    }
    pairs
}

#[test]
fn every_registered_route_is_documented_and_every_documented_route_exists() {
    let implemented = implemented();
    let documented = documented();

    // A parse that found nothing would otherwise make both diffs empty and pass. The
    // floor is deliberately far below the real count: this guards against a broken
    // parser, not against the route surface changing size.
    assert!(
        implemented.len() > 30,
        "parsed only {} routes from lib.rs, so the parser is broken rather than the \
         schema being complete",
        implemented.len()
    );

    let undocumented: Vec<_> = implemented.difference(&documented).collect();
    let unimplemented: Vec<_> = documented.difference(&implemented).collect();

    assert!(
        undocumented.is_empty(),
        "these routes are registered and absent from docs/api-v1.openapi.json: {undocumented:#?}"
    );
    assert!(
        unimplemented.is_empty(),
        "docs/api-v1.openapi.json declares these routes and the router does not register \
         them, so a generated client would emit a 405: {unimplemented:#?}"
    );
}

/// A session id in a path is the identity triple, so it is not a UUID.
///
/// Every one of these carried `format: uuid`, inherited from the server-minted id the
/// identity triple replaced (`docs/architecture.md` § Identity). A generated client
/// validating against that format would reject every real id it was handed — and the
/// encoded form can contain `/` besides, since a native id is the vendor's own value.
/// This is the one field where a wrong `format` is worse than none: it turns a correct
/// id into a client-side error nothing on the server can see.
#[test]
fn no_session_id_parameter_claims_to_be_a_uuid() {
    let raw = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/api-v1.openapi.json"),
    )
    .expect("the OpenAPI schema is part of the repo");
    let schema: serde_json::Value =
        serde_json::from_str(&raw).expect("the OpenAPI schema is valid JSON");

    let mut offenders = Vec::new();
    let mut checked = 0;
    for (path, operations) in schema["paths"].as_object().expect("paths") {
        for (method, operation) in operations.as_object().expect("operations") {
            for parameter in operation["parameters"].as_array().unwrap_or(&Vec::new()) {
                if parameter["name"] != "sessionId" {
                    continue;
                }
                checked += 1;
                if parameter["schema"]["format"] == "uuid" {
                    offenders.push(format!("{} {path}", method.to_uppercase()));
                }
            }
        }
    }

    assert!(checked > 0, "no sessionId parameters found, so this test verified nothing");
    assert!(
        offenders.is_empty(),
        "a session id is the encoded identity triple, not a UUID: {offenders:#?}"
    );
}

/// Every `$ref` resolves, and every declared schema is reachable.
///
/// A dangling `$ref` makes a generator fail outright; an orphaned schema is the quieter
/// half, and it is how a request or response shape outlives the endpoint it described —
/// which is what `DELETE /sessions/{id}` had done, declared in the schema for an
/// operation `api-design.md` documents as removed.
#[test]
fn every_ref_resolves_and_every_schema_is_referenced() {
    let raw = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/api-v1.openapi.json"),
    )
    .expect("the OpenAPI schema is part of the repo");
    let schema: serde_json::Value =
        serde_json::from_str(&raw).expect("the OpenAPI schema is valid JSON");

    /// Every `$ref` target anywhere in the document, and how often each is named.
    fn collect(value: &serde_json::Value, seen: &mut BTreeMap<String, usize>) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, child) in map {
                    if key == "$ref" {
                        if let Some(target) = child.as_str() {
                            *seen.entry(target.to_string()).or_default() += 1;
                        }
                    } else {
                        collect(child, seen);
                    }
                }
            }
            serde_json::Value::Array(items) => items.iter().for_each(|item| collect(item, seen)),
            _ => {}
        }
    }

    let mut refs = BTreeMap::new();
    collect(&schema, &mut refs);
    assert!(!refs.is_empty(), "no $refs found, so this test verified nothing");

    let declared: BTreeSet<String> = schema["components"]["schemas"]
        .as_object()
        .expect("the schema declares components")
        .keys()
        .cloned()
        .collect();

    let dangling: Vec<_> = refs
        .keys()
        .filter_map(|target| {
            let name = target.strip_prefix("#/components/schemas/")?;
            (!declared.contains(name)).then_some(target.as_str())
        })
        .collect();
    assert!(dangling.is_empty(), "these $refs resolve to nothing: {dangling:#?}");

    let referenced: BTreeSet<String> = refs
        .keys()
        .filter_map(|target| {
            target
                .strip_prefix("#/components/schemas/")
                .map(str::to_string)
        })
        .collect();
    let orphaned: Vec<_> = declared.difference(&referenced).collect();
    assert!(
        orphaned.is_empty(),
        "these schemas are declared and referenced by nothing, so they describe an \
         endpoint that no longer exists: {orphaned:#?}"
    );
}
