//! `cargo run -p sylph-docs-check` — list every pointer that does not resolve.
//!
//! The same findings the test gate asserts on. A binary as well as a test because
//! the two are used differently: the test says whether the tree is clean, and this
//! says what to fix, without a test harness truncating the list.

fn main() {
    let root = sylph_docs_check::repo_root();
    let findings = sylph_docs_check::check(&root);

    for finding in &findings {
        println!("{finding}");
    }

    if findings.is_empty() {
        println!("every pointer resolves");
    } else {
        eprintln!("\n{} unresolved pointer(s)", findings.len());
        std::process::exit(1);
    }
}
