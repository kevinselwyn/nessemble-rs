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
//!   help                    Show this help.
//!
//! It is intentionally dependency-free (std only), shelling out to `curl`,
//! `dpkg-deb`/`ar`/`tar`, and `cargo`.

use std::path::{Path, PathBuf};
use std::process::Command;

const REFERENCE_VERSION: &str = "1.1.1";
const CORPUS_GROUPS: [&str; 4] = ["opcodes", "examples", "nerdy-nights", "errors"];

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("help");
    let rest = &args[args.len().min(1)..];

    let result = match cmd {
        "fetch-oracle" => fetch_oracle(rest),
        "verify-goldens" => verify_goldens(),
        "parity" => parity(rest),
        "dist" => dist(),
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
         \x20 dist                    Build the GitHub Pages site (website + mdBook docs)\n\
         \x20 help                    Show this help"
    );
}

// ---------------------------------------------------------------------------
// dist — assemble the GitHub Pages site
// ---------------------------------------------------------------------------

/// Build the static site into `site/`: the marketing website at the root, with
/// the mdBook documentation under `site/docs/`. Requires `mdbook` on `PATH`.
fn dist() -> Result<(), String> {
    let root = repo_root();
    let site = root.join("site");
    let _ = std::fs::remove_dir_all(&site);
    std::fs::create_dir_all(&site).map_err(|e| e.to_string())?;

    // Marketing website (index.html + static/) at the site root.
    copy_dir(&root.join("website"), &site)?;

    // Documentation under /docs.
    run_tool(
        "mdbook",
        &["build", &root.join("docs").to_string_lossy()],
        None,
    )?;
    copy_dir(&root.join("docs/book"), &site.join("docs"))?;

    println!("Built site at {}", site.display());
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
                    .map(|n| n.starts_with("data.tar"))
                    .unwrap_or(false)
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
    report.push_str(&format!(
        "nessemble-rs parity vs v{REFERENCE_VERSION} goldens\n\
         =================================================\n\
         pass:    {pass}/{total}\n\
         fail:    {}/{total}\n\n",
        fail.len()
    ));
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
    if got.len() != expected.len() {
        "differing length".to_string()
    } else {
        "identical".to_string()
    }
}
