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

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map_or("help", String::as_str);
    let rest = &args[args.len().min(1)..];

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
         \x20 dist                    Build the GitHub Pages site (website + mdBook docs)\n\
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
/// the mdBook documentation under `site/docs/`. Requires `mdbook` on `PATH`.
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
    // Cache-bust before mdBook copies these into the book, so every docs page
    // requests the versioned asset URLs.
    cache_bust(&build_docs.join("theme/head.hbs"))?;
    cache_bust(&build_docs.join("src/nessemble/nessemble-assembler.js"))?;
    run_tool("mdbook", &["build", &build_docs.to_string_lossy()], None)?;
    copy_dir(&build_docs.join("book"), &site.join("docs"))?;

    println!("Built site at {}", site.display());
    Ok(())
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
