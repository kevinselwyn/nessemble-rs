//! The `changeset` command group — changeset-driven release versioning.
//!
//! Every PR that changes shipped behavior carries a changeset file under
//! `.changeset/` (the format the [`changesets`] crate parses); a release
//! computes the next version from every changeset accumulated since the last
//! one. This module owns the *policy* (bump precedence, the `none` opt-out,
//! changelog rendering); the actual manifest/lockfile edit is delegated to
//! `cargo set-version`. See `plans/004-release-orchestration.md`.
//!
//! Subcommands:
//!   add <major|minor|patch|none> [summary]   Scaffold a new changeset file.
//!   check                                    Validate the pending changesets.
//!   status                                   Show pending changesets + next version.
//!   version                                  Bump the workspace and consume the changesets.

use std::fmt::{Display, Write as _};
use std::path::{Path, PathBuf};

use changesets::{Change, ChangeType, UniqueId, Versioning};

use crate::{repo_root, run_tool};

/// The single umbrella package key for this single-version workspace: every
/// crate shares the one workspace version, so a changeset targets `nessemble`
/// rather than naming individual crates.
const PACKAGE: &str = "nessemble";

const CHANGELOG_HEADER: &str = "# Changelog";

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

pub fn run(args: &[String]) -> Result<(), String> {
    let sub = args.first().map(String::as_str);
    let rest = &args[args.len().min(1)..];
    match sub {
        Some("add") => add(rest),
        Some("check") => check(),
        Some("status") => status(),
        Some("version") => version(),
        Some(other) => Err(format!(
            "unknown changeset subcommand `{other}` (try add|check|status|version)"
        )),
        None => Err("changeset: missing subcommand (add|check|status|version)".to_string()),
    }
}

// ---------------------------------------------------------------------------
// Model
// ---------------------------------------------------------------------------

/// A version bump level. Declared low-to-high so the derived `Ord` makes
/// `Major` the greatest — `.max()` then yields the winning bump.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Bump {
    Patch,
    Minor,
    Major,
}

impl Bump {
    fn label(self) -> &'static str {
        match self {
            Bump::Patch => "patch",
            Bump::Minor => "minor",
            Bump::Major => "major",
        }
    }
}

/// A parsed, validated pending changeset.
struct Pending {
    file: PathBuf,
    id: String,
    /// `None` marks a `none` changeset — a documented no-release-impact change.
    bump: Option<Bump>,
    summary: String,
}

/// A `major.minor.patch` version. Any pre-release/build suffix is ignored; the
/// workspace version on `main` is always a plain release version.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Semver {
    major: u64,
    minor: u64,
    patch: u64,
}

impl Semver {
    fn parse(s: &str) -> Result<Self, String> {
        let core = s.split(['-', '+']).next().unwrap_or(s);
        let mut parts = core.split('.');
        let mut next = |field: &str| -> Result<u64, String> {
            parts
                .next()
                .and_then(|p| p.trim().parse().ok())
                .ok_or_else(|| format!("invalid version `{s}`: missing/invalid {field}"))
        };
        Ok(Semver {
            major: next("major")?,
            minor: next("minor")?,
            patch: next("patch")?,
        })
    }

    fn bumped(self, bump: Bump) -> Self {
        match bump {
            Bump::Major => Semver {
                major: self.major + 1,
                minor: 0,
                patch: 0,
            },
            Bump::Minor => Semver {
                major: self.major,
                minor: self.minor + 1,
                patch: 0,
            },
            Bump::Patch => Semver {
                major: self.major,
                minor: self.minor,
                patch: self.patch + 1,
            },
        }
    }
}

impl Display for Semver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

// ---------------------------------------------------------------------------
// Loading & validation
// ---------------------------------------------------------------------------

fn changeset_dir() -> PathBuf {
    repo_root().join(".changeset")
}

/// Read and validate every pending changeset, sorted by file name.
fn load_pending() -> Result<Vec<Pending>, String> {
    load_from_dir(&changeset_dir())
}

/// Load the pending changesets from `dir` — every `*.md` except `README.md`,
/// sorted for a stable changelog order. Each file is parsed and validated; a
/// malformed or off-convention file is an error.
fn load_from_dir(dir: &Path) -> Result<Vec<Pending>, String> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("read {}: {e}", dir.display()))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|e| e == "md")
                && p.file_name().and_then(|n| n.to_str()) != Some("README.md")
        })
        .collect();
    files.sort();

    files.iter().map(|f| parse_one(f)).collect()
}

fn parse_one(file: &Path) -> Result<Pending, String> {
    let name = file
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let change = Change::from_file(file).map_err(|e| format!("{name}: {e}"))?;
    let bump = bump_of(&change).map_err(|e| format!("{name}: {e}"))?;
    Ok(Pending {
        file: file.to_path_buf(),
        id: change.unique_id.to_string(),
        bump,
        summary: change.summary,
    })
}

/// Validate a change against the single-package convention and map its change
/// type onto our bump policy: `none` (a custom type) becomes `None` (no bump);
/// any other custom type is rejected so typos like `minr` don't slip through.
fn bump_of(change: &Change) -> Result<Option<Bump>, String> {
    let mut change_type = None;
    for (name, ct) in change.versioning.iter() {
        if name.as_str() != PACKAGE {
            return Err(format!(
                "unknown package `{name}`; use the single key `{PACKAGE}`"
            ));
        }
        change_type = Some(ct);
    }
    let ct = change_type.ok_or_else(|| "changeset targets no package".to_string())?;
    match ct {
        ChangeType::Major => Ok(Some(Bump::Major)),
        ChangeType::Minor => Ok(Some(Bump::Minor)),
        ChangeType::Patch => Ok(Some(Bump::Patch)),
        ChangeType::Custom(s) if s.eq_ignore_ascii_case("none") => Ok(None),
        ChangeType::Custom(s) => Err(format!(
            "unknown change type `{s}`; use major, minor, patch, or none"
        )),
    }
}

/// The winning bump across all pending changesets — the highest level, ignoring
/// `none`. `None` means nothing to release (no changesets, or all `none`).
fn overall_bump(pending: &[Pending]) -> Option<Bump> {
    pending.iter().filter_map(|p| p.bump).max()
}

fn workspace_version() -> Result<Semver, String> {
    let path = repo_root().join("Cargo.toml");
    let text =
        std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    parse_workspace_version(&text)
}

/// Read the `version` under `[workspace.package]` from a root `Cargo.toml`.
fn parse_workspace_version(toml: &str) -> Result<Semver, String> {
    let mut in_workspace_package = false;
    for line in toml.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_workspace_package = trimmed == "[workspace.package]";
            continue;
        }
        if in_workspace_package {
            if let Some(rest) = trimmed.strip_prefix("version") {
                if rest.trim_start().starts_with('=') {
                    if let Some(value) = rest.split('"').nth(1) {
                        return Semver::parse(value);
                    }
                }
            }
        }
    }
    Err("could not find [workspace.package] version in Cargo.toml".to_string())
}

// ---------------------------------------------------------------------------
// Changelog rendering
// ---------------------------------------------------------------------------

/// Render the `CHANGELOG.md` section for `version`, grouping the changeset
/// summaries under their bump level. `none` changesets are consumed but carry
/// no changelog entry.
fn render_changelog_section(version: &Semver, date: &str, pending: &[Pending]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "## {version} - {date}");
    out.push('\n');
    for (level, heading) in [
        (Bump::Major, "Major changes"),
        (Bump::Minor, "Minor changes"),
        (Bump::Patch, "Patch changes"),
    ] {
        let mut any = false;
        for p in pending.iter().filter(|p| p.bump == Some(level)) {
            if !any {
                let _ = writeln!(out, "### {heading}");
                out.push('\n');
                any = true;
            }
            let _ = writeln!(out, "- {}", bullet_body(&p.summary));
        }
        if any {
            out.push('\n');
        }
    }
    format!("{}\n", out.trim_end())
}

/// A changeset summary as a Markdown bullet body: trimmed, with continuation
/// lines indented so they stay under the bullet.
fn bullet_body(summary: &str) -> String {
    summary.trim().replace('\n', "\n  ")
}

/// Prepend `section` to the changelog, keeping a single top-level header and any
/// prior content below the new section.
fn update_changelog_text(existing: Option<&str>, section: &str) -> String {
    let mut out = format!("{CHANGELOG_HEADER}\n\n{}\n", section.trim_end());
    let prior = existing.map(strip_header).unwrap_or_default();
    if !prior.trim().is_empty() {
        out.push('\n');
        out.push_str(prior.trim_start_matches('\n'));
        if !out.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

/// Drop a leading `# Changelog` header line (and the blank lines after it) so it
/// isn't duplicated when a new section is prepended.
fn strip_header(text: &str) -> String {
    let trimmed = text.trim_start();
    match trimmed.strip_prefix(CHANGELOG_HEADER) {
        Some(rest) if rest.starts_with(['\n', '\r']) => rest.trim_start_matches('\n').to_string(),
        _ => trimmed.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Subcommands
// ---------------------------------------------------------------------------

fn add(args: &[String]) -> Result<(), String> {
    let usage = "usage: changeset add <major|minor|patch|none> [summary]";
    let level = args.first().ok_or(usage)?;
    let change_type = match level.as_str() {
        "major" => ChangeType::Major,
        "minor" => ChangeType::Minor,
        "patch" => ChangeType::Patch,
        "none" => ChangeType::Custom("none".to_string()),
        other => {
            return Err(format!(
                "unknown change type `{other}`; use major, minor, patch, or none"
            ))
        }
    };

    let given = args[args.len().min(1)..].join(" ");
    let given = given.trim();
    let has_summary = !given.is_empty();
    let summary = if has_summary {
        given.to_string()
    } else {
        "Describe your change here.".to_string()
    };
    let slug = if has_summary {
        truncate_slug(&UniqueId::normalize(given).to_string())
    } else {
        "changeset".to_string()
    };
    let id = format!("{slug}_{}", short_unique());

    let dir = changeset_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let change = Change {
        unique_id: UniqueId::exact(&id),
        versioning: Versioning::from((PACKAGE, change_type)),
        summary,
    };
    let path = change
        .write_to_directory(&dir)
        .map_err(|e| format!("write changeset: {e}"))?;
    println!("Created {}", path.display());
    if !has_summary {
        println!("Edit it to describe your change.");
    }
    Ok(())
}

fn check() -> Result<(), String> {
    let pending = load_pending()?;
    println!("{} changeset(s) valid", pending.len());
    Ok(())
}

fn status() -> Result<(), String> {
    let pending = load_pending()?;
    if pending.is_empty() {
        println!("No pending changesets.");
        return Ok(());
    }
    for p in &pending {
        let level = p.bump.map_or("none", Bump::label);
        println!("  {} [{level}] {}", p.id, first_line(&p.summary));
    }
    let current = workspace_version()?;
    match overall_bump(&pending) {
        Some(bump) => println!(
            "\n{} pending changeset(s): {current} -> {} ({} bump)",
            pending.len(),
            current.bumped(bump),
            bump.label()
        ),
        None => println!(
            "\n{} pending changeset(s), all `none`: no release ({current} unchanged)",
            pending.len()
        ),
    }
    Ok(())
}

fn version() -> Result<(), String> {
    let pending = load_pending()?;
    let Some(bump) = overall_bump(&pending) else {
        return Err(
            "no release-impacting changesets in .changeset/ — nothing to release".to_string(),
        );
    };
    let current = workspace_version()?;
    let next = current.bumped(bump);
    let next_str = next.to_string();

    // 1. Bump every manifest and Cargo.lock: the root workspace version (which
    //    all crates inherit) and the internal [workspace.dependencies] pins.
    run_tool(
        "cargo",
        &["set-version", "--workspace", &next_str],
        Some(&repo_root()),
    )?;

    // 2. Prepend the changelog section (before deleting the changesets it's
    //    rendered from, so a failure here leaves them in place).
    let section = render_changelog_section(&next, &today(), &pending);
    let path = repo_root().join("CHANGELOG.md");
    let existing = std::fs::read_to_string(&path).ok();
    std::fs::write(&path, update_changelog_text(existing.as_deref(), &section))
        .map_err(|e| format!("write {}: {e}", path.display()))?;

    // 3. Consume the changesets.
    for p in &pending {
        std::fs::remove_file(&p.file).map_err(|e| format!("remove {}: {e}", p.file.display()))?;
    }

    println!(
        "Released {current} -> {next} ({} bump) from {} changeset(s).",
        bump.label(),
        pending.len()
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("").trim()
}

fn truncate_slug(s: &str) -> String {
    let base: String = s.chars().take(40).collect();
    let base = base.trim_matches('_');
    if base.is_empty() {
        "changeset".to_string()
    } else {
        base.to_string()
    }
}

/// A short, effectively-unique suffix for a generated changeset file name, so
/// changesets authored on concurrent branches don't collide.
fn short_unique() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("{:x}", (nanos as u64) & 0xffff_ffff)
}

/// Today's date as `YYYY-MM-DD` (UTC), via the `date` tool. Falls back to a
/// placeholder if it isn't available.
fn today() -> String {
    std::process::Command::new("date")
        .args(["-u", "+%Y-%m-%d"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map_or_else(
            || "unreleased".to_string(),
            |o| String::from_utf8_lossy(&o.stdout).trim().to_string(),
        )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn change(content: &str) -> Change {
        Change::from_file_name_and_content("test.md", content).expect("parse change")
    }

    fn pending(bump: Option<Bump>, summary: &str) -> Pending {
        Pending {
            file: PathBuf::from(format!("{summary}.md")),
            id: summary.to_string(),
            bump,
            summary: summary.to_string(),
        }
    }

    #[test]
    fn bump_of_maps_the_standard_change_types() {
        assert_eq!(
            bump_of(&change("---\nnessemble: major\n---\ns")),
            Ok(Some(Bump::Major))
        );
        assert_eq!(
            bump_of(&change("---\nnessemble: minor\n---\ns")),
            Ok(Some(Bump::Minor))
        );
        assert_eq!(
            bump_of(&change("---\nnessemble: patch\n---\ns")),
            Ok(Some(Bump::Patch))
        );
        assert_eq!(bump_of(&change("---\nnessemble: none\n---\ns")), Ok(None));
    }

    #[test]
    fn bump_of_rejects_unknown_package_and_type() {
        assert!(bump_of(&change("---\nwrongpkg: minor\n---\ns")).is_err());
        assert!(bump_of(&change("---\nnessemble: minr\n---\ns")).is_err());
    }

    #[test]
    fn overall_bump_takes_the_highest_and_ignores_none() {
        let set = [
            pending(Some(Bump::Patch), "a"),
            pending(None, "b"),
            pending(Some(Bump::Minor), "c"),
        ];
        assert_eq!(overall_bump(&set), Some(Bump::Minor));

        let majored = [
            pending(Some(Bump::Minor), "a"),
            pending(Some(Bump::Major), "b"),
        ];
        assert_eq!(overall_bump(&majored), Some(Bump::Major));
    }

    #[test]
    fn overall_bump_is_none_when_empty_or_all_none() {
        assert_eq!(overall_bump(&[]), None);
        assert_eq!(
            overall_bump(&[pending(None, "a"), pending(None, "b")]),
            None
        );
    }

    #[test]
    fn semver_parse_and_bump() {
        let v = Semver::parse("2.8.1").unwrap();
        assert_eq!(
            v,
            Semver {
                major: 2,
                minor: 8,
                patch: 1
            }
        );
        assert_eq!(v.bumped(Bump::Patch).to_string(), "2.8.2");
        assert_eq!(v.bumped(Bump::Minor).to_string(), "2.9.0");
        assert_eq!(v.bumped(Bump::Major).to_string(), "3.0.0");
        // A pre-release/build suffix is ignored.
        assert_eq!(Semver::parse("2.8.1-dev").unwrap().to_string(), "2.8.1");
        assert!(Semver::parse("2.8").is_err());
    }

    #[test]
    fn parses_the_workspace_package_version() {
        let toml = "[workspace]\nmembers = []\n\n[workspace.package]\nversion = \"2.8.1\"\nedition = \"2021\"\n";
        assert_eq!(parse_workspace_version(toml).unwrap().to_string(), "2.8.1");
    }

    #[test]
    fn changelog_section_groups_by_level_and_skips_none() {
        let set = [
            pending(Some(Bump::Minor), "Add a feature"),
            pending(Some(Bump::Patch), "Fix a bug"),
            pending(None, "Docs only"),
        ];
        let section = render_changelog_section(
            &Semver {
                major: 2,
                minor: 9,
                patch: 0,
            },
            "2026-07-16",
            &set,
        );
        assert!(section.contains("## 2.9.0 - 2026-07-16"));
        assert!(section.contains("### Minor changes"));
        assert!(section.contains("- Add a feature"));
        assert!(section.contains("### Patch changes"));
        assert!(section.contains("- Fix a bug"));
        assert!(!section.contains("Major changes"));
        assert!(!section.contains("Docs only"));
    }

    #[test]
    fn update_changelog_prepends_and_keeps_one_header() {
        let existing = "# Changelog\n\n## 2.8.1 - 2026-01-01\n\n### Patch changes\n\n- Old\n";
        let section = "## 2.9.0 - 2026-07-16\n\n### Minor changes\n\n- New\n";
        let out = update_changelog_text(Some(existing), section);
        assert_eq!(out.matches("# Changelog").count(), 1);
        let new_at = out.find("2.9.0").unwrap();
        let old_at = out.find("2.8.1").unwrap();
        assert!(new_at < old_at, "new section must come first");
    }

    #[test]
    fn update_changelog_creates_from_nothing() {
        let section = "## 1.0.0 - 2026-07-16\n\n### Minor changes\n\n- First\n";
        let out = update_changelog_text(None, section);
        assert!(out.starts_with("# Changelog\n"));
        assert!(out.contains("- First"));
    }

    #[test]
    fn load_from_dir_skips_readme_and_sorts() {
        let dir = std::env::temp_dir().join(format!("nessemble-cs-{}", short_unique()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("README.md"), "not a changeset\n").unwrap();
        std::fs::write(
            dir.join("b_second.md"),
            "---\nnessemble: patch\n---\nSecond\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("a_first.md"),
            "---\nnessemble: minor\n---\nFirst\n",
        )
        .unwrap();

        let pending = load_from_dir(&dir).unwrap();
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(pending.len(), 2, "README.md must be ignored");
        assert_eq!(pending[0].id, "a_first");
        assert_eq!(pending[1].id, "b_second");
        assert_eq!(overall_bump(&pending), Some(Bump::Minor));
    }
}
