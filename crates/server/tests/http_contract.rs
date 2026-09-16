//! Preserved HTTP authentication, file containment and metadata contracts.
mod common;
use axum::http::{header, StatusCode};
use common::{json_body, TestBackend};
use serde_json::json;

fn urlencoding(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}
#[tokio::test]
async fn bootstrap_sets_a_hardened_cookie() {
    let backend = TestBackend::start().await;

    let response = backend.get("/api/v1/auth/token").await;
    assert_eq!(response.status(), StatusCode::OK);

    let cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .expect("Set-Cookie header missing")
        .to_str()
        .unwrap()
        .to_string();

    assert!(cookie.contains("sylph_session="));
    // HttpOnly keeps the token away from page scripts; SameSite=Strict stops a
    // third-party page from riding the cookie on a cross-site request.
    assert!(cookie.contains("HttpOnly"), "cookie not HttpOnly: {cookie}");
    assert!(
        cookie.contains("SameSite=Strict"),
        "cookie not SameSite=Strict: {cookie}"
    );
}

#[tokio::test]
async fn bootstrap_is_disabled_after_registration() {
    let backend = TestBackend::start().await;
    let registered = backend
        .post(
            "/api/v1/auth/register",
            serde_json::json!({"name": "Alice"}),
        )
        .await;
    assert_eq!(registered.status(), StatusCode::CREATED);

    let response = backend.get("/api/v1/auth/token").await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(response.headers().get(header::SET_COOKIE).is_none());
    let body = json_body(response).await;
    assert_eq!(body["reason"], "authentication_required");
}

#[tokio::test]
async fn protected_routes_require_the_cookie() {
    let backend = TestBackend::start().await;

    for path in ["/api/v1/diagnostics", "/api/v1/sessions", "/api/v1/agents"] {
        let response = backend.get(path).await;
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{path} should require auth"
        );
        let body = json_body(response).await;
        assert_eq!(body["error"]["code"], "UNAUTHORIZED");
    }
}

#[tokio::test]
async fn unmatched_routes_answer_with_the_error_envelope() {
    let backend = TestBackend::start().await;

    let response = backend.get_authed("/api/v1/not-an-endpoint").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let body = json_body(response).await;
    assert_eq!(
        body["error"]["code"], "NOT_FOUND",
        "a router 404 must be distinguishable from FILE_NOT_FOUND"
    );
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|m| !m.is_empty()),
        "the envelope must explain itself, not just carry a code"
    );
}

#[tokio::test]
async fn wrong_method_answers_405_with_the_error_envelope() {
    let backend = TestBackend::start().await;

    let response = backend.post_authed("/api/v1/health", json!({})).await;
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(
        json_body(response).await["error"]["code"],
        "METHOD_NOT_ALLOWED"
    );
}

#[tokio::test]
async fn unmatched_routes_are_still_behind_auth() {
    let backend = TestBackend::start().await;

    let response = backend.get("/api/v1/not-an-endpoint").await;
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "the fallback must not answer before the auth layer runs"
    );
    assert_eq!(json_body(response).await["error"]["code"], "UNAUTHORIZED");
}

#[tokio::test]
async fn file_tree_lists_the_session_workdir() {
    let backend = TestBackend::start().await;
    let session_id = backend.create_session("agent").await;

    let root = backend.workdir();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("README.md"), "# Title\n").unwrap();
    std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();

    let body = json_body(
        backend
            .get_authed(&format!("/api/v1/sessions/{session_id}/files"))
            .await,
    )
    .await;

    let entries = body["entries"].as_array().unwrap();
    let names: Vec<&str> = entries
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"src"), "directory missing: {names:?}");
    assert!(names.contains(&"README.md"), "file missing: {names:?}");
    // Directories sort ahead of files, which is what makes a tree scannable.
    let src_at = names.iter().position(|n| *n == "src").unwrap();
    let readme_at = names.iter().position(|n| *n == "README.md").unwrap();
    assert!(src_at < readme_at, "directories must sort first: {names:?}");

    assert_eq!(entries[src_at]["isDir"], true);
    assert_eq!(entries[readme_at]["isDir"], false);
    assert_eq!(entries[readme_at]["size"], 8);
}

#[tokio::test]
async fn file_tree_omits_vcs_and_build_directories() {
    let backend = TestBackend::start().await;
    let session_id = backend.create_session("agent").await;

    let root = backend.workdir();
    for skipped in [".git", "node_modules", "target", ".next"] {
        std::fs::create_dir_all(root.join(skipped)).unwrap();
    }
    std::fs::create_dir_all(root.join("src")).unwrap();

    let body = json_body(
        backend
            .get_authed(&format!("/api/v1/sessions/{session_id}/files"))
            .await,
    )
    .await;

    let names: Vec<&str> = body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec!["src"],
        "only real source dirs belong: {names:?}"
    );
}

#[tokio::test]
async fn file_access_refuses_paths_outside_the_workdir() {
    let backend = TestBackend::start().await;
    let session_id = backend.create_session("agent").await;

    // A real secret outside the root, so a successful escape would be visible
    // rather than merely a 404 from a nonexistent target.
    let outside = backend
        .workdir()
        .parent()
        .unwrap()
        .join("outside-secret.txt");
    std::fs::write(&outside, "do not serve me").unwrap();

    for attempt in [
        "../outside-secret.txt",
        "./../outside-secret.txt",
        "src/../../outside-secret.txt",
        "/etc/hosts",
    ] {
        let response = backend
            .get_authed(&format!(
                "/api/v1/sessions/{session_id}/files/content?path={}",
                urlencoding(attempt)
            ))
            .await;
        let status = response.status();
        let body = json_body(response).await;
        assert!(
            status == StatusCode::FORBIDDEN || status == StatusCode::NOT_FOUND,
            "{attempt} was not refused: {status} {body}"
        );
        assert_ne!(
            body["content"].as_str().unwrap_or_default(),
            "do not serve me",
            "{attempt} escaped the workdir"
        );
    }
}

#[tokio::test]
async fn symlinks_leaving_the_workdir_are_not_served() {
    let backend = TestBackend::start().await;
    let session_id = backend.create_session("agent").await;

    let outside = backend
        .workdir()
        .parent()
        .unwrap()
        .join("linked-secret.txt");
    std::fs::write(&outside, "secret via symlink").unwrap();
    let link = backend.workdir().join("escape.txt");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, &link).unwrap();

    let response = backend
        .get_authed(&format!(
            "/api/v1/sessions/{session_id}/files/content?path=escape.txt"
        ))
        .await;
    let status = response.status();
    let body = json_body(response).await;
    assert_ne!(
        body["content"].as_str().unwrap_or_default(),
        "secret via symlink",
        "a symlink served content from outside the workdir (status {status})"
    );

    // It is also hidden from the listing, so it is never offered in the tree.
    let tree = json_body(
        backend
            .get_authed(&format!("/api/v1/sessions/{session_id}/files"))
            .await,
    )
    .await;
    let names: Vec<&str> = tree["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert!(
        !names.contains(&"escape.txt"),
        "escaping symlink listed: {names:?}"
    );
}

#[tokio::test]
async fn file_content_reports_encoding_from_the_bytes() {
    let backend = TestBackend::start().await;
    let session_id = backend.create_session("agent").await;

    let root = backend.workdir();
    std::fs::write(root.join("notes.md"), "# Hello\n\nBody text.\n").unwrap();
    // A `.log` holding invalid UTF-8: extension says text, content says binary.
    std::fs::write(root.join("blob.log"), [0xff, 0xfe, 0x00, 0x01]).unwrap();
    // 1x1 transparent GIF.
    let gif: [u8; 14] = [
        0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x01, 0x00, 0x01, 0x00, 0x80, 0x00, 0x00, 0x00,
    ];
    std::fs::write(root.join("pixel.gif"), gif).unwrap();

    let text = json_body(
        backend
            .get_authed(&format!(
                "/api/v1/sessions/{session_id}/files/content?path=notes.md"
            ))
            .await,
    )
    .await;
    assert_eq!(text["encoding"], "text");
    assert_eq!(text["mimeType"], "text/markdown");
    assert!(text["content"].as_str().unwrap().contains("# Hello"));
    assert_eq!(text["truncated"], false);

    let binary = json_body(
        backend
            .get_authed(&format!(
                "/api/v1/sessions/{session_id}/files/content?path=blob.log"
            ))
            .await,
    )
    .await;
    assert_eq!(binary["encoding"], "binary");
    assert!(binary["content"].is_null(), "binary bytes must not be sent");

    let image = json_body(
        backend
            .get_authed(&format!(
                "/api/v1/sessions/{session_id}/files/content?path=pixel.gif"
            ))
            .await,
    )
    .await;
    assert_eq!(image["encoding"], "image");
    assert_eq!(image["mimeType"], "image/gif");
    // Base64 of the GIF header, so the client can build a data: URL.
    assert!(image["content"].as_str().unwrap().starts_with("R0lGODlh"));
}

#[tokio::test]
async fn missing_files_are_404_and_directories_are_not_content() {
    let backend = TestBackend::start().await;
    let session_id = backend.create_session("agent").await;
    std::fs::create_dir_all(backend.workdir().join("src")).unwrap();

    let missing = backend
        .get_authed(&format!(
            "/api/v1/sessions/{session_id}/files/content?path=nope.txt"
        ))
        .await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    assert_eq!(json_body(missing).await["error"]["code"], "FILE_NOT_FOUND");

    let directory = backend
        .get_authed(&format!(
            "/api/v1/sessions/{session_id}/files/content?path=src"
        ))
        .await;
    assert_eq!(directory.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn file_tree_follows_symlinks_to_their_target_kind() {
    let backend = TestBackend::start().await;
    let session_id = backend.create_session("agent").await;

    let root = backend.workdir();
    std::fs::create_dir_all(root.join("real/nested")).unwrap();
    std::fs::write(root.join("real/inside.txt"), "contents\n").unwrap();
    std::os::unix::fs::symlink(root.join("real"), root.join("linked-dir")).unwrap();
    std::os::unix::fs::symlink(root.join("real/inside.txt"), root.join("linked-file")).unwrap();

    let body = json_body(
        backend
            .get_authed(&format!("/api/v1/sessions/{session_id}/files"))
            .await,
    )
    .await;
    let entries = body["entries"].as_array().unwrap();
    let find = |name: &str| {
        entries
            .iter()
            .find(|e| e["name"] == name)
            .unwrap_or_else(|| panic!("{name} missing from {entries:?}"))
    };

    let linked_dir = find("linked-dir");
    assert_eq!(
        linked_dir["isDir"], true,
        "a symlink to a directory must report isDir, or the tree cannot expand it"
    );
    assert!(
        linked_dir["size"].is_null(),
        "a directory carries no size, however it was reached"
    );
    assert_eq!(
        linked_dir["isSymlink"], true,
        "the link is still flagged as one"
    );

    // The target's size, not the link's own byte length.
    let linked_file = find("linked-file");
    assert_eq!(linked_file["isDir"], false);
    assert_eq!(linked_file["size"], 9);
    assert_eq!(linked_file["isSymlink"], true);

    // And it actually expands: the whole point of reporting it as a directory.
    let nested = json_body(
        backend
            .get_authed(&format!(
                "/api/v1/sessions/{session_id}/files?path=linked-dir"
            ))
            .await,
    )
    .await;
    let names: Vec<&str> = nested["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"inside.txt"),
        "link did not expand: {names:?}"
    );
    assert!(names.contains(&"nested"), "link did not expand: {names:?}");
}

#[tokio::test]
async fn file_tree_skips_symlinked_build_directories() {
    let backend = TestBackend::start().await;
    let session_id = backend.create_session("agent").await;

    let root = backend.workdir();
    std::fs::create_dir_all(root.join("vendor-store")).unwrap();
    std::os::unix::fs::symlink(root.join("vendor-store"), root.join("node_modules")).unwrap();

    let body = json_body(
        backend
            .get_authed(&format!("/api/v1/sessions/{session_id}/files"))
            .await,
    )
    .await;
    let names: Vec<&str> = body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert!(
        !names.contains(&"node_modules"),
        "symlinked node_modules listed: {names:?}"
    );
}

#[tokio::test]
async fn file_tree_omits_dangling_symlinks() {
    let backend = TestBackend::start().await;
    let session_id = backend.create_session("agent").await;

    let root = backend.workdir();
    std::os::unix::fs::symlink(root.join("never-existed"), root.join("dangling")).unwrap();
    std::fs::write(root.join("real.txt"), "ok\n").unwrap();

    let body = json_body(
        backend
            .get_authed(&format!("/api/v1/sessions/{session_id}/files"))
            .await,
    )
    .await;
    let names: Vec<&str> = body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert!(
        !names.contains(&"dangling"),
        "dangling link listed: {names:?}"
    );
    // A broken link must not take the rest of the listing down with it.
    assert!(
        names.contains(&"real.txt"),
        "listing lost entries: {names:?}"
    );
}

#[tokio::test]
async fn file_endpoints_require_auth_and_a_real_session() {
    let backend = TestBackend::start().await;
    let session_id = backend.create_session("agent").await;

    let unauthed = backend
        .get(&format!("/api/v1/sessions/{session_id}/files"))
        .await;
    assert_eq!(unauthed.status(), StatusCode::UNAUTHORIZED);

    let unknown = backend
        .get_authed("/api/v1/sessions/env7fk2%3Amock%3Adoes-not-exist/files")
        .await;
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        json_body(unknown).await["error"]["code"],
        "SESSION_NOT_FOUND"
    );
}

#[tokio::test]
async fn upload_refuses_to_overwrite_unless_asked() {
    let backend = TestBackend::start().await;
    let session_id = backend.create_session("agent").await;
    std::fs::write(backend.workdir().join("keep.txt"), "original").unwrap();

    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session_id}/files"),
            json!({ "path": "", "name": "keep.txt", "data": "bmV3" }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        std::fs::read_to_string(backend.workdir().join("keep.txt")).unwrap(),
        "original",
        "the existing file must survive a refused upload"
    );

    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session_id}/files"),
            json!({ "path": "", "name": "keep.txt", "data": "bmV3", "overwrite": true }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(
        std::fs::read_to_string(backend.workdir().join("keep.txt")).unwrap(),
        "new",
        "an explicit overwrite must take effect"
    );
}

#[tokio::test]
async fn delete_refuses_a_non_empty_directory_unless_recursive() {
    let backend = TestBackend::start().await;
    let session_id = backend.create_session("agent").await;
    std::fs::create_dir_all(backend.workdir().join("tree/nested")).unwrap();
    std::fs::write(backend.workdir().join("tree/nested/deep.txt"), "x").unwrap();

    let response = backend
        .delete_authed(&format!("/api/v1/sessions/{session_id}/files?path=tree"))
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(backend.workdir().join("tree/nested/deep.txt").exists());

    // An empty directory needs no flag: there is nothing to lose.
    std::fs::create_dir_all(backend.workdir().join("blank")).unwrap();
    let response = backend
        .delete_authed(&format!("/api/v1/sessions/{session_id}/files?path=blank"))
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let response = backend
        .delete_authed(&format!(
            "/api/v1/sessions/{session_id}/files?path=tree&recursive=true"
        ))
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(!backend.workdir().join("tree").exists());
}

#[tokio::test]
async fn delete_refuses_to_escape_the_workdir() {
    let backend = TestBackend::start().await;
    let session_id = backend.create_session("agent").await;
    let outside = backend.workdir().parent().unwrap().join("outside.txt");
    std::fs::write(&outside, "x").unwrap();

    let response = backend
        .delete_authed(&format!(
            "/api/v1/sessions/{session_id}/files?path=../outside.txt"
        ))
        .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(outside.exists(), "a path outside the root must survive");
    std::fs::remove_file(&outside).ok();
}

#[tokio::test]
async fn delete_unlinks_a_symlink_without_touching_its_target() {
    let backend = TestBackend::start().await;
    let session_id = backend.create_session("agent").await;
    let real = backend.workdir().join("real");
    std::fs::create_dir_all(&real).unwrap();
    std::fs::write(real.join("keep.txt"), "x").unwrap();
    std::os::unix::fs::symlink(&real, backend.workdir().join("link")).unwrap();

    let response = backend
        .delete_authed(&format!("/api/v1/sessions/{session_id}/files?path=link"))
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(!backend.workdir().join("link").exists());
    assert!(
        real.join("keep.txt").exists(),
        "the link's target must be untouched"
    );
}

#[tokio::test]
async fn upload_rejects_malformed_base64() {
    let backend = TestBackend::start().await;
    let session_id = backend.create_session("agent").await;

    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session_id}/files"),
            json!({ "path": "", "name": "bad.bin", "data": "not base64!!" }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn config_requires_auth() {
    let backend = TestBackend::start().await;
    assert_eq!(
        backend.get("/api/v1/config").await.status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn config_is_read_only() {
    let backend = TestBackend::start().await;

    for method in ["PUT", "PATCH", "POST", "DELETE"] {
        let response = backend.request_authed(method, "/api/v1/config", None).await;
        assert_eq!(
            response.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "{method} /config must not be accepted"
        );
    }
}

#[tokio::test]
async fn patch_updates_the_title() {
    let backend = TestBackend::start().await;
    let session_id = backend.create_session("agent").await;

    let body = json_body(
        backend
            .patch_authed(
                &format!("/api/v1/sessions/{session_id}"),
                json!({ "title": "  Renamed  " }),
            )
            .await,
    )
    .await;
    assert_eq!(body["session"]["title"], "Renamed", "titles are trimmed");
}

#[tokio::test]
async fn patch_on_a_missing_session_is_not_found() {
    let backend = TestBackend::start().await;

    let response = backend
        .patch_authed(
            "/api/v1/sessions/env7fk2%3Amock%3Adoes-not-exist",
            json!({ "title": "x" }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn retained_reprojection_status_is_hidden_from_non_admin_subjects() {
    let backend = TestBackend::start().await;
    let _admin = backend
        .post("/api/v1/auth/register", json!({"name":"trace-admin"}))
        .await;
    let registration = backend
        .post("/api/v1/auth/register", json!({"name":"trace-reader"}))
        .await;
    let cookie = registration.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        backend
            .get_with_cookie("/api/v1/trace/reprojection", &cookie)
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        backend
            .request("POST", "/api/v1/trace/reprojection", None, Some(&cookie))
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    for method in ["GET", "DELETE"] {
        assert_eq!(
            backend
                .request(
                    method,
                    "/api/v1/trace/reprojection/unknown",
                    None,
                    Some(&cookie)
                )
                .await
                .status(),
            StatusCode::FORBIDDEN
        );
    }
}

#[tokio::test]
async fn retained_reprojection_is_an_admin_action_with_durable_status() {
    let backend = TestBackend::start().await;
    let status = backend.get_authed("/api/v1/trace/reprojection").await;
    assert_eq!(status.status(), StatusCode::OK);
    let queued = backend
        .post_authed("/api/v1/trace/reprojection", json!({}))
        .await;
    assert_eq!(queued.status(), StatusCode::ACCEPTED);
    let receipt = json_body(queued).await;
    assert!(receipt["job"]["jobId"].is_string());
    assert_eq!(receipt["job"]["state"], "queued");
    assert!(receipt["job"].get("relativeFile").is_none());
    let id = receipt["job"]["jobId"].as_str().unwrap();
    let route = format!("/api/v1/trace/reprojection/{id}");
    assert_eq!(backend.get_authed(&route).await.status(), StatusCode::OK);
    let cancelled = backend.delete_authed(&route).await;
    assert!(matches!(
        cancelled.status(),
        StatusCode::OK | StatusCode::ACCEPTED
    ));
    let status = backend.get_authed("/api/v1/trace/reprojection").await;
    assert_eq!(status.status(), StatusCode::OK);
    let status = json_body(status).await;
    assert!(status["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|job| job["jobId"] == id));
}
