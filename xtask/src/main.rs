//! Developer tasks for `nessemble-rs`.
//!
//! The centerpiece is the **parity harness**: it runs the imported reference
//! corpus (`tests/corpus/`) through either `nessemble-rs` or the official
//! v1.1.1 release binary (the "oracle") and compares the produced bytes against
//! the committed golden `.rom` files.
//!
//! Commands:
//!   fetch-oracle [--i386]   Download & extract the v1.1.1 release binary.
//!   verify-goldens          Confirm the oracle reproduces every committed golden.
//!   parity [--release]      Run nessemble-rs over the corpus and report parity.
//!   wasm                    Build the WebAssembly assembler bundle (wasm-bindgen).
//!   changeset <sub>         Changeset-driven release versioning (add/check/status/version).
//!   help                    Show this help.
//!
//! It shells out to `curl`, `dpkg-deb`/`ar`/`tar`, `cargo`, and `mdbook`; its
//! only crate dependency is `nessemble-core`, whose lexer/classifier the `dist`
//! command uses to syntax-highlight the docs' static ` ```nessemble ` code blocks
//! (the same tokenizer the editor and language server use).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use nessemble_core::tooling::{self, LexKind, TokenClass};

mod changeset;

const REFERENCE_VERSION: &str = "1.1.1";
const CORPUS_GROUPS: [&str; 4] = ["opcodes", "examples", "nerdy-nights", "errors"];

/// Base URL the mdBook documentation is served from on GitHub Pages. Used to
/// build the absolute links in the generated `llms.txt` (kept in step with the
/// project site URL referenced elsewhere, e.g. the README).
const DOCS_BASE_URL: &str = "https://kevinselwyn.github.io/nessemble-rs/docs/";

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map_or("help", String::as_str);
    let rest = args.get(1..).unwrap_or(&[]);

    let result = match cmd {
        "fetch-oracle" => fetch_oracle(rest),
        "verify-goldens" => verify_goldens(),
        "parity" => parity(rest),
        "wasm" => wasm(),
        "dist" => dist(),
        "changeset" => changeset::run(rest),
        "help" | "-h" | "--help" => {
            print_help();
            Ok(())
        }
        other => Err(format!("unknown command `{other}` (try `help`)")),
    };

    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn print_help() {
    println!(
        "xtask — nessemble-rs developer tasks\n\n\
         USAGE:\n\
         \x20 cargo run -p xtask -- <command>\n\n\
         COMMANDS:\n\
         \x20 fetch-oracle [--i386]   Download & extract the v{REFERENCE_VERSION} release binary\n\
         \x20 verify-goldens          Confirm the oracle reproduces every committed golden\n\
         \x20 parity [--release]      Run nessemble-rs over the corpus and report parity\n\
         \x20 wasm                    Build the WebAssembly assembler bundle (needs the wasm32 target + wasm-bindgen)\n\
         \x20 dist                    Build the GitHub Pages site (website + mdBook docs + llms.txt)\n\
         \x20 changeset <sub>         Changeset-driven release versioning: add | check | status | version\n\
         \x20 help                    Show this help"
    );
}

// ---------------------------------------------------------------------------
// wasm — build the WebAssembly assembler bundle
// ---------------------------------------------------------------------------

/// Build the `nessemble-wasm` crate to a browser-ready ES-module bundle in
/// `crates/nessemble-wasm/pkg/` (`nessemble.js` + `nessemble_bg.wasm`).
///
/// Compiles the cdylib to `wasm32-unknown-unknown` and runs `wasm-bindgen`
/// directly (the pieces `wasm-pack` orchestrates) — no extra tool to install,
/// and it matches the `wasm-bindgen` version pinned by the crate. Requires the
/// `wasm32-unknown-unknown` target and `wasm-bindgen` on `PATH`.
fn wasm() -> Result<(), String> {
    let root = repo_root();
    run_tool(
        "cargo",
        &[
            "build",
            "-p",
            "nessemble-wasm",
            "--release",
            "--target",
            "wasm32-unknown-unknown",
        ],
        Some(&root),
    )?;

    let wasm_in = root.join("target/wasm32-unknown-unknown/release/nessemble_wasm.wasm");
    let out_dir = root.join("crates/nessemble-wasm/pkg");
    run_tool(
        "wasm-bindgen",
        &[
            "--target",
            "web",
            "--no-typescript",
            "--out-dir",
            &out_dir.to_string_lossy(),
            "--out-name",
            "nessemble",
            &wasm_in.to_string_lossy(),
        ],
        None,
    )?;

    println!("Built wasm bundle at {}", out_dir.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// dist — assemble the GitHub Pages site
// ---------------------------------------------------------------------------

/// Build the static site into `site/`: the marketing website at the root, with
/// the mdBook documentation under `site/docs/` and a generated `llms.txt` index
/// at the docs root. Requires `mdbook` on `PATH`.
fn dist() -> Result<(), String> {
    let root = repo_root();

    // Build the wasm bundle and stage it (with the assembler component) where
    // the docs and the marketing site can each serve it.
    wasm()?;
    stage_web_assets(&root.join("docs/src/nessemble"))?;
    stage_web_assets(&root.join("website/static/nessemble"))?;

    let site = root.join("site");
    let _ = std::fs::remove_dir_all(&site);
    std::fs::create_dir_all(&site).map_err(|e| e.to_string())?;

    // Marketing website (index.html + static/, including the staged assembler)
    // at the site root.
    copy_dir(&root.join("website"), &site)?;
    cache_bust(&site.join("index.html"))?;
    cache_bust(&site.join("static/nessemble/nessemble-assembler.js"))?;

    // Documentation under /docs. Build from a transformed *copy* of the book so
    // the committed sources stay clean: `highlight_fences` rewrites each
    // ` ```nessemble ` code block into pre-highlighted HTML (via the shared
    // lexer), which mdBook passes through verbatim — the static blocks share the
    // editor's `na-tok-*` classes without a second grammar. mdBook copies the
    // staged `src/nessemble/` assets into the book, and `theme/head.hbs` loads
    // the component (and its CSS) on every page.
    let build_docs = root.join("target/docs-build");
    let _ = std::fs::remove_dir_all(&build_docs);
    copy_dir(&root.join("docs"), &build_docs)?;
    let _ = std::fs::remove_dir_all(build_docs.join("book"));
    highlight_fences(&build_docs.join("src"))?;
    // Reshape each flat chapter `foo.md` into `foo/index.md` so mdBook renders it
    // to `foo/index.html`, served at the extensionless URL `/docs/foo/`. mdBook
    // computes all assets, sidebar, nav, and the search index for the new depth;
    // `clean_doc_urls` then trims `index.html` from the generated links so they
    // read as `/docs/foo/` too.
    directorify_chapters(&build_docs.join("src"))?;
    // Cache-bust before mdBook copies these into the book, so every docs page
    // requests the versioned asset URLs.
    cache_bust(&build_docs.join("theme/head.hbs"))?;
    cache_bust(&build_docs.join("src/nessemble/nessemble-assembler.js"))?;
    run_tool("mdbook", &["build", &build_docs.to_string_lossy()], None)?;
    copy_dir(&build_docs.join("book"), &site.join("docs"))?;
    clean_doc_urls(&site.join("docs"))?;

    // Emit `llms.txt` at the docs root, derived from the book's own `SUMMARY.md`
    // and page leads so it can't drift from the actual page set as the docs
    // change (see `write_llms_txt`). Read from the clean sources, not the
    // fence-highlighted build copy.
    write_llms_txt(&root, &site.join("docs"))?;

    println!("Built site at {}", site.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// Extensionless URLs (/docs/foo/ instead of /docs/foo.html)
// ---------------------------------------------------------------------------

/// Reshape each flat top-level chapter `foo.md` in `src` into `foo/index.md`
/// (the introduction `index.md` and `SUMMARY.md` stay put), so mdBook renders it
/// to `foo/index.html` — served at the extensionless URL `/docs/foo/`. The flat
/// `foo.md` cross-references authored in the sources are rewritten to resolve
/// against the new one-level-deeper layout.
fn directorify_chapters(src: &Path) -> Result<(), String> {
    // Chapters = top-level `*.md` other than `SUMMARY.md` and the introduction.
    let mut chapters: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(src).map_err(|e| format!("read {}: {e}", src.display()))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.is_file() && path.extension().is_some_and(|e| e == "md") {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name != "SUMMARY.md" && name != "index.md" {
                if let Some(stem) = name.strip_suffix(".md") {
                    chapters.push(stem.to_string());
                }
            }
        }
    }

    // Files that stay at the root keep their depth (`in_chapter_dir = false`).
    for name in ["SUMMARY.md", "index.md"] {
        let path = src.join(name);
        if path.is_file() {
            let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
            std::fs::write(&path, rewrite_chapter_links(&text, false))
                .map_err(|e| e.to_string())?;
        }
    }

    // Move each chapter one level deeper, rewriting its links for the new depth.
    for stem in &chapters {
        let from = src.join(format!("{stem}.md"));
        let text = std::fs::read_to_string(&from).map_err(|e| e.to_string())?;
        let dir = src.join(stem);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        std::fs::write(dir.join("index.md"), rewrite_chapter_links(&text, true))
            .map_err(|e| e.to_string())?;
        std::fs::remove_file(&from).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Rewrite the flat `](foo.md)` chapter links in `text` for the directorified
/// layout. `in_chapter_dir` is true for a page that itself moved into `foo/`,
/// so its links need an extra `../`.
fn rewrite_chapter_links(text: &str, in_chapter_dir: bool) -> String {
    let b = text.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < text.len() {
        if b[i] == b']' && b.get(i + 1) == Some(&b'(') {
            if let Some(rel) = text[i + 2..].find(')') {
                let close = i + 2 + rel;
                out.push_str("](");
                out.push_str(&rewrite_link_target(&text[i + 2..close], in_chapter_dir));
                out.push(')');
                i = close + 1;
                continue;
            }
        }
        let ch = text[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Map a single link target to the directorified layout, preserving any
/// `#fragment`. Only flat internal `foo.md` targets are touched; external links,
/// anchors, and already-nested paths pass through unchanged.
fn rewrite_link_target(target: &str, in_chapter_dir: bool) -> String {
    let (path, frag) = match target.split_once('#') {
        Some((p, f)) => (p, Some(f)),
        None => (target, None),
    };
    // Only flat internal `foo.md` targets are rewritten; skip external links
    // (`scheme:`) and already-nested/relative paths.
    let stem = match path.strip_suffix(".md") {
        Some(stem) if !stem.is_empty() && !path.contains('/') && !path.contains(':') => stem,
        _ => return target.to_string(),
    };
    let new_path = match (stem, in_chapter_dir) {
        // The introduction stays at the root of the book.
        ("index", true) => "../index.md".to_string(),
        ("index", false) => "index.md".to_string(),
        (stem, true) => format!("../{stem}/index.md"),
        (stem, false) => format!("{stem}/index.md"),
    };
    match frag {
        Some(f) => format!("{new_path}#{f}"),
        None => new_path,
    }
}

/// Trim `index.html` out of the URLs mdBook generated, so the built links read
/// as `/docs/foo/` rather than `/docs/foo/index.html`. Applied to every `.html`
/// page and the search index under `dir`. `print.html`/`404.html` are real files
/// and keep their names — only `index.html` segments are removed.
fn clean_doc_urls(dir: &Path) -> Result<(), String> {
    for entry in std::fs::read_dir(dir).map_err(|e| format!("read {}: {e}", dir.display()))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.is_dir() {
            clean_doc_urls(&path)?;
            continue;
        }
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let is_html = path.extension().is_some_and(|e| e == "html");
        if is_html || name.starts_with("searchindex.") {
            let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
            std::fs::write(&path, strip_index_html(&text)).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Remove `index.html` from the quoted/anchored URLs in `text`. A bare
/// `"index.html"` (a page's link to the introduction from the book root) becomes
/// `"./"`; every `dir/index.html` becomes `dir/`.
fn strip_index_html(text: &str) -> String {
    text.replace("\"index.html#", "\"#")
        .replace("\"index.html\"", "\"./\"")
        .replace("index.html#", "#")
        .replace("index.html\"", "\"")
}

// ---------------------------------------------------------------------------
// llms.txt — machine-discoverable documentation index
// ---------------------------------------------------------------------------

/// A single documentation page in the generated `llms.txt`.
struct LlmsEntry {
    title: String,
    href: String,
    description: Option<String>,
}

/// Generate `llms.txt` at the docs root (`docs_out/llms.txt`), following the
/// [llms.txt convention](https://llmstxt.org/) so LLMs and agents can discover
/// the manual.
///
/// The index is derived entirely from the book: the H1/blockquote come from
/// `book.toml`, the link list from `SUMMARY.md` (in order), and each page's
/// description from its lead paragraph. Because it is regenerated from those
/// sources on every `dist`, it stays in step with the documentation — adding,
/// renaming, removing, or reordering a page is reflected automatically.
fn write_llms_txt(root: &Path, docs_out: &Path) -> Result<(), String> {
    let src = root.join("docs/src");
    let book_toml = std::fs::read_to_string(root.join("docs/book.toml"))
        .map_err(|e| format!("read book.toml: {e}"))?;
    let summary = std::fs::read_to_string(src.join("SUMMARY.md"))
        .map_err(|e| format!("read SUMMARY.md: {e}"))?;

    let (title, description) = book_meta(&book_toml);
    let entries: Vec<LlmsEntry> = parse_summary(&summary)
        .into_iter()
        .map(|(text, file)| {
            let description = std::fs::read_to_string(src.join(&file))
                .ok()
                .and_then(|c| lead_description(&c));
            LlmsEntry {
                title: text,
                href: md_to_url(&file),
                description,
            }
        })
        .collect();

    let out = render_llms_txt(&title, description.as_deref(), &entries);
    let path = docs_out.join("llms.txt");
    std::fs::write(&path, out).map_err(|e| format!("write {}: {e}", path.display()))?;
    println!("Wrote {}", path.display());
    Ok(())
}

/// Pull the `title` and `description` from the `[book]` table of `book.toml`
/// (a light line scan — no TOML dependency, matching the file's flat shape).
fn book_meta(book_toml: &str) -> (String, Option<String>) {
    let mut title = "nessemble".to_string();
    let mut description = None;
    for line in book_toml.lines() {
        let line = line.trim();
        // Stop at the next table so we only read keys under `[book]`.
        if line.starts_with('[') && line != "[book]" {
            break;
        }
        if let Some(v) = toml_str_value(line, "title") {
            title = v;
        } else if let Some(v) = toml_str_value(line, "description") {
            description = Some(v);
        }
    }
    (title, description)
}

/// If `line` is `key = "value"`, return the unquoted value.
fn toml_str_value(line: &str, key: &str) -> Option<String> {
    let rest = line.strip_prefix(key)?.trim_start();
    let rest = rest.strip_prefix('=')?.trim();
    let inner = rest.strip_prefix('"')?.strip_suffix('"')?;
    Some(inner.to_string())
}

/// Parse `SUMMARY.md` into ordered `(link text, relative .md path)` pairs. Only
/// links to local `.md` files are kept (external links and separators dropped).
fn parse_summary(summary: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in summary.lines() {
        if let Some((text, href)) = first_md_link(line) {
            out.push((text, href));
        }
    }
    out
}

/// Find the first `[text](path.md)` markdown link on `line`, returning its text
/// and path when the target is a local `.md` file.
fn first_md_link(line: &str) -> Option<(String, String)> {
    let open = line.find('[')?;
    let close = line[open..].find("](")? + open;
    let end = line[close + 2..].find(')')? + close + 2;
    let text = &line[open + 1..close];
    let href = &line[close + 2..end];
    let is_md = Path::new(href)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("md"));
    if is_md && !href.contains("://") {
        Some((text.to_string(), href.to_string()))
    } else {
        None
    }
}

/// Map a book-relative `.md` path to its absolute served URL. Pages are served
/// at extensionless directory URLs (`foo.md` → `foo/`, see `directorify_chapters`
/// and `clean_doc_urls`); the introduction `index.md` is the docs root itself.
fn md_to_url(file: &str) -> String {
    let stem = file.strip_suffix(".md").unwrap_or(file);
    if stem == "index" {
        DOCS_BASE_URL.to_string()
    } else {
        format!("{DOCS_BASE_URL}{stem}/")
    }
}

/// Derive a one-line description from a page's lead paragraph: the first prose
/// paragraph after the H1, flattened to a single line, with markdown links
/// reduced to their text and trimmed to the first sentence. Returns `None` when
/// the page has no prose lead (e.g. it opens with a code block).
fn lead_description(content: &str) -> Option<String> {
    let mut past_h1 = false;
    let mut in_fence = false;
    let mut para: Vec<&str> = Vec::new();

    for line in content.lines() {
        let t = line.trim();
        if !past_h1 {
            if t.starts_with("# ") {
                past_h1 = true;
            }
            continue;
        }
        if t.starts_with("```") {
            // A fence before any prose is skipped; one after ends the paragraph.
            if para.is_empty() {
                in_fence = !in_fence;
                continue;
            }
            break;
        }
        if in_fence {
            continue;
        }
        if t.is_empty() {
            if para.is_empty() {
                continue;
            }
            break;
        }
        // Skip non-prose lead lines (headings, tables, lists, blockquotes, raw
        // HTML) until the prose paragraph starts.
        if para.is_empty() && is_non_prose(t) {
            continue;
        }
        para.push(t);
    }

    if para.is_empty() {
        return None;
    }
    Some(first_sentence(&strip_md_links(&para.join(" "))))
}

/// Whether a trimmed line begins a non-prose block (used to skip past headings,
/// tables, lists, blockquotes, and raw HTML when hunting for a lead paragraph).
fn is_non_prose(t: &str) -> bool {
    t.starts_with('#')
        || t.starts_with('|')
        || t.starts_with('-')
        || t.starts_with('*')
        || t.starts_with('>')
        || t.starts_with('<')
}

/// Replace markdown link syntax with its visible text: `[text](url)` and
/// `[text][ref]` both become `text`, and a bare `[text]` keeps `text`.
fn strip_md_links(s: &str) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < s.len() {
        if s.as_bytes()[i] == b'[' {
            if let Some(rel_close) = s[i + 1..].find(']') {
                let close = i + 1 + rel_close;
                let text = &s[i + 1..close];
                let after = close + 1;
                let rest = &s[after..];
                if rest.starts_with('(') {
                    if let Some(p) = rest.find(')') {
                        out.push_str(text);
                        i = after + p + 1;
                        continue;
                    }
                } else if rest.starts_with('[') {
                    if let Some(p) = rest.find(']') {
                        out.push_str(text);
                        i = after + p + 1;
                        continue;
                    }
                }
                out.push_str(text);
                i = after;
                continue;
            }
        }
        let ch = s[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Trim a flattened paragraph to its first sentence. A sentence ends at a `.`
/// followed by a space or end-of-string (so decimals like `2.0` don't split);
/// with no sentence terminator, a trailing `:` is dropped.
fn first_sentence(s: &str) -> String {
    let s = s.trim();
    for (idx, ch) in s.char_indices() {
        if ch == '.' {
            let next = s[idx + 1..].chars().next();
            if next.is_none_or(|c| c == ' ') {
                return s[..=idx].trim().to_string();
            }
        }
    }
    s.trim_end_matches(':').trim().to_string()
}

/// Render the `llms.txt` body from the book metadata and page entries.
fn render_llms_txt(title: &str, description: Option<&str>, entries: &[LlmsEntry]) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "# {title}\n");
    if let Some(d) = description {
        let _ = writeln!(s, "> {d}\n");
    }
    let _ = writeln!(
        s,
        "This file follows the [llms.txt convention](https://llmstxt.org/) so \
         LLMs and agents can discover nessemble's documentation. Each link below \
         points to a page of the mdBook manual.\n"
    );
    let _ = writeln!(s, "## Documentation\n");
    for e in entries {
        match &e.description {
            Some(d) => {
                let _ = writeln!(s, "- [{}]({}): {}", e.title, e.href, d);
            }
            None => {
                let _ = writeln!(s, "- [{}]({})", e.title, e.href);
            }
        }
    }
    s
}

/// Append `?v=<workspace version>` to the component/wasm asset references in
/// `path`, so a new release invalidates any CDN- or browser-cached copy (the
/// site is served behind a CDN, and unversioned URLs otherwise serve stale CSS/JS
/// after a deploy). No-op for any reference not present in the file.
fn cache_bust(path: &Path) -> Result<(), String> {
    let version = env!("CARGO_PKG_VERSION"); // the workspace version
    let mut text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    for asset in [
        "nessemble-assembler.css",
        "nessemble-assembler.js",
        "nessemble.js",
        "nessemble_bg.wasm",
    ] {
        text = text.replace(&format!("{asset}\""), &format!("{asset}?v={version}\""));
    }
    std::fs::write(path, text).map_err(|e| format!("write {}: {e}", path.display()))
}

// ---------------------------------------------------------------------------
// Docs syntax highlighting (build-time)
// ---------------------------------------------------------------------------

/// Recursively rewrite ` ```nessemble ` fenced code blocks in the `.md` files
/// under `dir` into pre-highlighted `<pre class="na-code">…</pre>` HTML, so the
/// docs' static code blocks are colored by the same lexer as the editor. Other
/// fences (and everything else) are left untouched.
fn highlight_fences(dir: &Path) -> Result<(), String> {
    for entry in std::fs::read_dir(dir).map_err(|e| format!("read {}: {e}", dir.display()))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.is_dir() {
            highlight_fences(&path)?;
        } else if path.extension().is_some_and(|e| e == "md") {
            let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
            std::fs::write(&path, highlight_markdown(&text)).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Replace every ` ```nessemble ` fenced block in `text` with a highlighted
/// `<pre class="na-code">` HTML block (an mdBook-passthrough raw-HTML block).
fn highlight_markdown(text: &str) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out = String::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim() == "```nessemble" {
            let mut j = i + 1;
            while j < lines.len() && lines[j].trim() != "```" {
                j += 1;
            }
            let code = lines[i + 1..j].join("\n");
            // Blank lines around it so mdBook treats it as a raw-HTML block; no
            // inner `<code>` so mdBook's code-block JS/highlight.js ignore it.
            out.push_str("\n<pre class=\"na-code\">");
            out.push_str(&highlight_code(&code));
            out.push_str("</pre>\n\n");
            i = j + 1;
        } else {
            out.push_str(lines[i]);
            out.push('\n');
            i += 1;
        }
    }
    out
}

/// Highlight nessemble `code` into HTML: significant tokens become
/// `<span class="na-tok-…">`, whitespace/newlines are copied verbatim; all text
/// is HTML-escaped. Uses the shared `tooling::lex` + `tooling::classify`.
fn highlight_code(code: &str) -> String {
    let mut html = String::new();
    for lx in tooling::lex(code) {
        let piece = &code[lx.start..lx.end];
        let esc = escape_html(piece);
        match lx.kind {
            LexKind::Whitespace | LexKind::Newline => html.push_str(&esc),
            kind => {
                let _ = write!(
                    html,
                    "<span class=\"na-tok-{}\">{esc}</span>",
                    class_name(tooling::classify(kind, piece))
                );
            }
        }
    }
    html
}

/// The CSS class suffix for a token class (index-aligned with the wasm
/// `token_classes` legend), so static blocks and the editor share `na-tok-*`.
fn class_name(class: TokenClass) -> &'static str {
    match class {
        TokenClass::Directive => "directive",
        TokenClass::Instruction => "instruction",
        TokenClass::Identifier => "identifier",
        TokenClass::Number => "number",
        TokenClass::String => "string",
        TokenClass::Comment => "comment",
        TokenClass::Operator => "operator",
    }
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Copy the assembler component + the wasm bundle into `dest` (recreating it),
/// for a docs or website asset directory. Requires [`wasm`] to have run first.
fn stage_web_assets(dest: &Path) -> Result<(), String> {
    let root = repo_root();
    let _ = std::fs::remove_dir_all(dest);
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    let assets = [
        root.join("web/nessemble-assembler.js"),
        root.join("web/nessemble-assembler.css"),
        root.join("crates/nessemble-wasm/pkg/nessemble.js"),
        root.join("crates/nessemble-wasm/pkg/nessemble_bg.wasm"),
    ];
    for src in assets {
        let name = src.file_name().ok_or("bad asset path")?;
        std::fs::copy(&src, dest.join(name)).map_err(|e| format!("copy {}: {e}", src.display()))?;
    }
    // The vendored CodeMirror 6 bundle is the editing surface; keep its `vendor/`
    // subdir so the staged layout matches `web/` and the component's relative
    // `import("vendor/codemirror.js")` resolves.
    let vendor = dest.join("vendor");
    std::fs::create_dir_all(&vendor).map_err(|e| e.to_string())?;
    let cm = root.join("web/vendor/codemirror.js");
    std::fs::copy(&cm, vendor.join("codemirror.js"))
        .map_err(|e| format!("copy {}: {e}", cm.display()))?;
    Ok(())
}

/// Recursively copy `from` into `to` (creating `to`).
fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| e.to_string())?;
    for entry in std::fs::read_dir(from).map_err(|e| format!("read {}: {e}", from.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if src.is_dir() {
            copy_dir(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst).map_err(|e| format!("copy {}: {e}", src.display()))?;
        }
    }
    Ok(())
}

fn repo_root() -> PathBuf {
    // xtask lives at <root>/xtask; CARGO_MANIFEST_DIR points there.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().map(Path::to_path_buf).unwrap_or(manifest)
}

// ---------------------------------------------------------------------------
// Oracle fetching
// ---------------------------------------------------------------------------

fn oracle_dir() -> PathBuf {
    repo_root().join(".oracle")
}

fn oracle_binary(i386: bool) -> PathBuf {
    oracle_dir().join(if i386 { "nessemble-i386" } else { "nessemble" })
}

fn fetch_oracle(args: &[String]) -> Result<(), String> {
    let i386 = args.iter().any(|a| a == "--i386");
    let arch = if i386 { "i386" } else { "amd64" };
    let url = format!(
        "https://github.com/kevinselwyn/nessemble/releases/download/v{REFERENCE_VERSION}/nessemble_{REFERENCE_VERSION}_{arch}.deb"
    );

    let dir = oracle_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let deb = dir.join(format!("nessemble_{arch}.deb"));

    eprintln!("Downloading {url}");
    run_tool(
        "curl",
        &["-sSL", "--fail", "-o", &deb.to_string_lossy(), &url],
        None,
    )?;

    let extract = dir.join(format!("extract_{arch}"));
    let _ = std::fs::remove_dir_all(&extract);
    std::fs::create_dir_all(&extract).map_err(|e| format!("mkdir {}: {e}", extract.display()))?;

    // Prefer dpkg-deb; fall back to `ar` + tar.
    let dpkg = run_tool(
        "dpkg-deb",
        &["-x", &deb.to_string_lossy(), &extract.to_string_lossy()],
        None,
    );
    if dpkg.is_err() {
        run_tool("ar", &["x", &deb.to_string_lossy()], Some(&extract))?;
        // data.tar.{gz,xz,zst}
        let data = std::fs::read_dir(&extract)
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .find(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("data.tar"))
            })
            .ok_or("could not find data.tar in extracted .deb")?;
        run_tool(
            "tar",
            &[
                "-xf",
                &data.to_string_lossy(),
                "-C",
                &extract.to_string_lossy(),
            ],
            None,
        )?;
    }

    let src = extract.join("usr/local/bin/nessemble");
    let dst = oracle_binary(i386);
    std::fs::copy(&src, &dst)
        .map_err(|e| format!("copy {} -> {}: {e}", src.display(), dst.display()))?;
    make_executable(&dst)?;

    eprintln!("Oracle ready: {}", dst.display());
    Ok(())
}

fn make_executable(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)
            .map_err(|e| e.to_string())?
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).map_err(|e| e.to_string())?;
    }
    let _ = path;
    Ok(())
}

fn run_tool(program: &str, args: &[&str], cwd: Option<&Path>) -> Result<(), String> {
    let mut cmd = Command::new(program);
    cmd.args(args);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let status = cmd
        .status()
        .map_err(|e| format!("failed to run `{program}`: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`{program}` exited with {status}"))
    }
}

/// Like [`run_tool`], but with `$HOME` overridden (used to install the bundled
/// scripts into a hermetic scripts-home).
fn run_tool_env(program: &str, args: &[&str], home: Option<&Path>) -> Result<(), String> {
    let mut cmd = Command::new(program);
    cmd.args(args);
    if let Some(home) = home {
        cmd.env("HOME", home);
    }
    let status = cmd
        .status()
        .map_err(|e| format!("failed to run `{program}`: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`{program}` exited with {status}"))
    }
}

// ---------------------------------------------------------------------------
// Corpus discovery
// ---------------------------------------------------------------------------

struct TestCase {
    group: String,
    name: String,
    asm: PathBuf,
    rom: PathBuf,
    flags: Vec<String>,
    /// Requires the bundled scripts installed into a `~/.nessemble/scripts`
    /// (a scripts-home is set for these when running `nessemble-rs`).
    needs_scripts: bool,
    /// Set when the v1.1.1 oracle cannot reproduce this golden in-sandbox (its
    /// polyglot/Lua scripting isn't available); such cases are skipped by
    /// `verify-goldens` but still checked by `parity`.
    oracle_skip: Option<&'static str>,
}

fn discover_corpus() -> Result<Vec<TestCase>, String> {
    let corpus = repo_root().join("tests/corpus");
    let mut cases = Vec::new();

    for group in CORPUS_GROUPS {
        let group_dir = corpus.join(group);
        if !group_dir.is_dir() {
            continue;
        }
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&group_dir)
            .map_err(|e| format!("read {}: {e}", group_dir.display()))?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        entries.sort();

        for dir in entries {
            let name = dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            let asm = dir.join(format!("{name}.asm"));
            let rom = dir.join(format!("{name}.rom"));
            if !asm.is_file() || !rom.is_file() {
                continue;
            }

            let (mut flags, needs_scripts, oracle_skip) = classify(group, &name);
            // The custom-pseudo example maps its directives via a `-p` file.
            if group == "examples" && name == "custom" {
                flags.push("--pseudo".to_string());
                flags.push(dir.join("custom.txt").to_string_lossy().into_owned());
            }
            cases.push(TestCase {
                group: group.to_string(),
                name,
                asm,
                rom,
                flags,
                needs_scripts,
                oracle_skip,
            });
        }
    }

    cases.sort_by(|a, b| {
        (a.group.as_str(), a.name.as_str()).cmp(&(b.group.as_str(), b.name.as_str()))
    });
    Ok(cases)
}

/// Per-test flags, whether it needs the bundled scripts installed, and whether
/// the oracle can reproduce it in-sandbox.
///
/// The reference per-test drivers pass no extra flags except for undocumented
/// opcodes (`-u`). The scripting cases (`custom`/`ease`/`ease-type`) run through
/// the Rhai host; the oracle's Lua/polyglot scripting isn't available here, so
/// they are `parity`-checked but `verify-goldens`-skipped.
fn classify(group: &str, name: &str) -> (Vec<String>, bool, Option<&'static str>) {
    let flags = if name == "undocumented" {
        vec!["-u".to_string()]
    } else {
        Vec::new()
    };

    let (needs_scripts, oracle_skip) = match (group, name) {
        ("examples", "custom") => (false, Some("polyglot scripts not runnable in-sandbox")),
        ("examples", "ease") | ("errors", "ease-type") => (true, Some("bundled Lua ease script")),
        _ => (false, None),
    };

    (flags, needs_scripts, oracle_skip)
}

/// Run a binary on a test case and return the combined output bytes.
///
/// Mirrors the reference test harness: `<bin> <asm> --output -` (+ flags),
/// combined = stderr followed by stdout. `scripts_home` (when set) is used as
/// `$HOME` so directives resolve against installed bundled scripts.
fn run_case(bin: &Path, case: &TestCase, scripts_home: Option<&Path>) -> Result<Vec<u8>, String> {
    let mut cmd = Command::new(bin);
    cmd.arg(&case.asm)
        .arg("--output")
        .arg("-")
        .args(&case.flags);
    if case.needs_scripts {
        if let Some(home) = scripts_home {
            cmd.env("HOME", home);
        }
    }
    let output = cmd
        .output()
        .map_err(|e| format!("failed to run {}: {e}", bin.display()))?;

    let mut combined = output.stderr;
    combined.extend_from_slice(&output.stdout);
    Ok(combined)
}

// ---------------------------------------------------------------------------
// verify-goldens
// ---------------------------------------------------------------------------

fn verify_goldens() -> Result<(), String> {
    let oracle = oracle_binary(false);
    if !oracle.is_file() {
        return Err(format!(
            "oracle binary not found at {} — run `cargo run -p xtask -- fetch-oracle` first",
            oracle.display()
        ));
    }

    let cases = discover_corpus()?;
    let mut ok = 0usize;
    let mut mismatched = Vec::new();
    let mut skipped = 0usize;

    for case in &cases {
        if case.oracle_skip.is_some() {
            // The oracle's Lua/polyglot scripting isn't available in-sandbox.
            skipped += 1;
            continue;
        }
        let golden = std::fs::read(&case.rom).map_err(|e| e.to_string())?;
        let got = run_case(&oracle, case, None)?;
        if got == golden {
            ok += 1;
        } else {
            mismatched.push(format!(
                "{}/{} (golden {} bytes, oracle {} bytes)",
                case.group,
                case.name,
                golden.len(),
                got.len()
            ));
        }
    }

    println!(
        "verify-goldens: {ok} reproduced, {} mismatched, {skipped} skipped (scripting)",
        mismatched.len()
    );
    for m in &mismatched {
        println!("  MISMATCH {m}");
    }

    if mismatched.is_empty() {
        println!("All committed goldens are reproduced by the v{REFERENCE_VERSION} oracle.");
        Ok(())
    } else {
        Err(format!("{} golden(s) not reproduced", mismatched.len()))
    }
}

// ---------------------------------------------------------------------------
// parity
// ---------------------------------------------------------------------------

fn parity(args: &[String]) -> Result<(), String> {
    let release = args.iter().any(|a| a == "--release");

    // Build the CLI binary.
    let mut build = vec!["build", "-p", "nessemble-cli"];
    if release {
        build.push("--release");
    }
    run_tool("cargo", &build, Some(&repo_root()))?;

    let bin = repo_root().join(if release {
        "target/release/nessemble"
    } else {
        "target/debug/nessemble"
    });
    if !bin.is_file() {
        return Err(format!("built binary not found at {}", bin.display()));
    }

    // Install the bundled scripts into a hermetic scripts-home so directives
    // like `.ease` resolve without touching the real `$HOME`.
    let scripts_home = repo_root().join("target/parity-home");
    let _ = std::fs::remove_dir_all(&scripts_home);
    std::fs::create_dir_all(&scripts_home).map_err(|e| e.to_string())?;
    run_tool_env(&bin.to_string_lossy(), &["scripts"], Some(&scripts_home))?;

    let cases = discover_corpus()?;
    let mut pass = 0usize;
    let mut fail = Vec::new();

    for case in &cases {
        let golden = std::fs::read(&case.rom).map_err(|e| e.to_string())?;
        let got = run_case(&bin, case, Some(&scripts_home))?;
        if got == golden {
            pass += 1;
        } else {
            fail.push(format!(
                "{}/{}: got {} bytes, expected {} ({})",
                case.group,
                case.name,
                got.len(),
                golden.len(),
                first_diff(&got, &golden)
            ));
        }
    }

    let total = pass + fail.len();
    let mut report = String::new();
    let _ = writeln!(
        report,
        "nessemble-rs parity vs v{REFERENCE_VERSION} goldens\n\
         =================================================\n\
         pass:    {pass}/{total}\n\
         fail:    {}/{total}\n",
        fail.len()
    );
    for f in &fail {
        report.push_str("FAIL ");
        report.push_str(f);
        report.push('\n');
    }

    let report_path = repo_root().join("tests/parity-report.txt");
    std::fs::write(&report_path, &report).map_err(|e| e.to_string())?;

    print!("{report}");
    println!("(report written to {})", report_path.display());
    Ok(())
}

fn first_diff(got: &[u8], expected: &[u8]) -> String {
    let n = got.len().min(expected.len());
    for i in 0..n {
        if got[i] != expected[i] {
            return format!(
                "first diff at byte {i}: {:#04x} != {:#04x}",
                got[i], expected[i]
            );
        }
    }
    if got.len() == expected.len() {
        "identical".to_string()
    } else {
        "differing length".to_string()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn book_meta_reads_title_and_description() {
        let toml = "[book]\ntitle = \"nessemble\"\ndescription = \"A 6502 assembler\"\nsrc = \"src\"\n\n[output.html]\ntitle = \"ignored\"\n";
        let (title, desc) = book_meta(toml);
        assert_eq!(title, "nessemble");
        assert_eq!(desc.as_deref(), Some("A 6502 assembler"));
    }

    #[test]
    fn parse_summary_keeps_ordered_local_pages() {
        let summary = "# Summary\n\n[Introduction](index.md)\n\n- [Installation](installation.md)\n- [Repo](https://example.com/x.md)\n- [Usage](usage.md)\n";
        assert_eq!(
            parse_summary(summary),
            vec![
                ("Introduction".to_string(), "index.md".to_string()),
                ("Installation".to_string(), "installation.md".to_string()),
                ("Usage".to_string(), "usage.md".to_string()),
            ]
        );
    }

    #[test]
    fn md_to_url_builds_extensionless_dir_urls() {
        assert_eq!(
            md_to_url("installation.md"),
            format!("{DOCS_BASE_URL}installation/")
        );
        // The introduction is the docs root.
        assert_eq!(md_to_url("index.md"), DOCS_BASE_URL);
    }

    #[test]
    fn rewrite_link_target_directorifies_flat_links() {
        // From the root (introduction / SUMMARY): foo.md -> foo/index.md.
        assert_eq!(rewrite_link_target("usage.md", false), "usage/index.md");
        assert_eq!(rewrite_link_target("index.md", false), "index.md");
        // From inside a moved chapter dir: foo.md -> ../foo/index.md.
        assert_eq!(rewrite_link_target("usage.md", true), "../usage/index.md");
        assert_eq!(rewrite_link_target("index.md", true), "../index.md");
        // Fragments are preserved.
        assert_eq!(
            rewrite_link_target("extending.md#filesystem-access", true),
            "../extending/index.md#filesystem-access"
        );
        // External links and anchors are left alone.
        assert_eq!(
            rewrite_link_target("https://rhai.rs", true),
            "https://rhai.rs"
        );
        assert_eq!(rewrite_link_target("#section", true), "#section");
    }

    #[test]
    fn rewrite_chapter_links_only_touches_link_targets() {
        let text = "See [Usage](usage.md) and [Rhai](https://rhai.rs).";
        assert_eq!(
            rewrite_chapter_links(text, true),
            "See [Usage](../usage/index.md) and [Rhai](https://rhai.rs)."
        );
    }

    #[test]
    fn strip_index_html_cleans_urls() {
        assert_eq!(
            strip_index_html(r#"<a href="../syntax/index.html">x</a>"#),
            r#"<a href="../syntax/">x</a>"#
        );
        assert_eq!(
            strip_index_html(r#"<a href="usage/index.html#opts">x</a>"#),
            r#"<a href="usage/#opts">x</a>"#
        );
        // A book-root link back to the introduction.
        assert_eq!(
            strip_index_html(r#"<a href="index.html">Home</a>"#),
            r#"<a href="./">Home</a>"#
        );
        assert_eq!(
            strip_index_html(r#"<a href="../index.html">Home</a>"#),
            r#"<a href="../">Home</a>"#
        );
    }

    #[test]
    fn strip_md_links_reduces_to_text() {
        assert_eq!(
            strip_md_links("uses [Rhai](https://rhai.rs), a language"),
            "uses Rhai, a language"
        );
        assert_eq!(
            strip_md_links("a [Language Server][lsp] for 6502"),
            "a Language Server for 6502"
        );
    }

    #[test]
    fn lead_description_takes_first_sentence() {
        let page = "# Upgrading\n\n`nessemble` 2.0 is a ground-up rewrite in Rust. Assembly output\nis compatible.\n\nMore text.\n";
        assert_eq!(
            lead_description(page).as_deref(),
            Some("`nessemble` 2.0 is a ground-up rewrite in Rust.")
        );
    }

    #[test]
    fn lead_description_skips_leading_fence() {
        // A page whose lead is a code block, then prose after a heading.
        let page = "# Usage\n\n```text\nnessemble --help\n```\n\n## Options\n\nSets things up.\n";
        assert_eq!(lead_description(page).as_deref(), Some("Sets things up."));
    }

    #[test]
    fn lead_description_strips_trailing_colon_when_no_period() {
        let page = "# Installation\n\nDownload the latest release for your system:\n\n| a | b |\n";
        assert_eq!(
            lead_description(page).as_deref(),
            Some("Download the latest release for your system")
        );
    }

    #[test]
    fn render_llms_txt_formats_entries() {
        let entries = vec![
            LlmsEntry {
                title: "Installation".to_string(),
                href: "https://x/docs/installation.html".to_string(),
                description: Some("Get it.".to_string()),
            },
            LlmsEntry {
                title: "Usage".to_string(),
                href: "https://x/docs/usage.html".to_string(),
                description: None,
            },
        ];
        let out = render_llms_txt("nessemble", Some("An assembler"), &entries);
        assert!(out.starts_with("# nessemble\n"));
        assert!(out.contains("> An assembler\n"));
        assert!(out.contains("## Documentation\n"));
        assert!(out.contains("- [Installation](https://x/docs/installation.html): Get it.\n"));
        assert!(out.contains("- [Usage](https://x/docs/usage.html)\n"));
    }
}
