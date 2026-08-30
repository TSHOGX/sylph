# fts5-jieba

A SQLite FTS5 tokenizer: jieba segmentation for Chinese, plus folding and Porter stemming
for English, so one tokenizer serves both languages.

**Why this is vendored here.** It originated in `trace-db` (MIT, same author) and is copied
into this repo rather than depended on by path: a path dependency into a sibling checkout
compiles on exactly one machine, which is a failure mode `AGENTS.md` records paying for.

**Why the server links it rather than loading it.** `docs/storage.md` § Tokenization has the
argument. In short: an index built with one tokenizer and queried with another silently
returns nothing, so a missing loadable extension would make the server answer *wrongly*
rather than worse. Linking removes that state — there is no file to find, and a registration
failure is fatal.

`crate-type` keeps `cdylib` alongside `rlib` so the same source still builds as a loadable
extension for the `sqlite3` CLI, which is how you inspect an existing index by hand:

```sh
cargo build --release
sqlite3 history.db  # then: .load target/release/libfts5jieba sqlite3_fts5jieba_init
```

## Tests

`cargo test -p fts5-jieba` covers segmentation directly and through real FTS5 tables
(`rusqlite` with a bundled SQLite): Chinese word segmentation, CJK/English boundaries,
UTF-8 byte offsets, diacritic folding, stemming and `nostem`, and mixed-script input.

The offset tests matter most. FTS5 takes **byte** offsets into the original input, so an
off-by-one there corrupts every `snippet()` and highlight rather than failing loudly.

## Attribution

The `sqlite3_api_routines` approach — describing enough of that table by field order to
reach `fts5_api` — is the one documented way for a loadable extension to obtain the FTS5
API. `src/ffi.rs` notes Signal-FTS5-Extension (AGPL) as where the approach was read; the
bindings here are an independent reimplementation of the published C ABI, not copied code.
