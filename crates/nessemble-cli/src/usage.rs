//! Usage, version, and license output, matching the reference `usage.c` for the
//! in-scope CLI surface. Out-of-scope flags (`-d`/`-R`/`-s`/`-r`) and commands
//! (registry/package/user) are omitted entirely — they appear nowhere here.

use nessemble_i18n::t;

const PROGRAM_NAME: &str = "nessemble";
/// The displayed version is the workspace (crate) version — the single source
/// of truth that also drives the release pipeline.
const PROGRAM_VERSION: &str = env!("CARGO_PKG_VERSION");
const PROGRAM_COPYRIGHT: &str = "2017";
const PROGRAM_AUTHOR: &str = "Kevin Selwyn";

/// The GPL notice printed by `--license` (the body after the version header).
const LICENSE_TEXT: &str = include_str!("data/license.txt");

/// In-scope option rows (`invocation`, `description`).
const OPTIONS: &[(&str, &str)] = &[
    ("-o, --output <outfile.rom>", "output file"),
    ("-f, --format {NES,RAW}", "output format"),
    ("-e, --empty <hex>", "empty byte value"),
    ("-u, --undocumented", "use undocumented opcodes"),
    (
        "-l, --list <listfile.txt>",
        "generate list of labels and constants",
    ),
    (
        "-p, --pseudo <pseudo.txt>",
        "use custom pseudo-instruction functions",
    ),
    ("-c, --check", "check syntax only"),
    ("-C, --coverage", "log data coverage"),
    ("-v, --version", "display program version"),
    ("-L, --license", "display program license"),
    ("-h, --help", "print this message"),
];

/// In-scope command rows.
const COMMANDS: &[(&str, &str)] = &[
    ("init [<arg> ...]", "initialize new project"),
    ("scripts", "install scripts"),
    (
        "reference [<category>] [<term>]",
        "get reference info about assembly terms",
    ),
    ("config [<key>] [<val>]", "list/get/set config info"),
];

/// Render a two-column block, aligning descriptions two spaces past the longest
/// invocation (matching the reference `print_usage`).
fn print_block(rows: &[(&str, &str)], out: &mut String) {
    let max = rows.iter().map(|(i, _)| i.len()).max().unwrap_or(0);
    for (invocation, description) in rows {
        let pad = max - invocation.len() + 2;
        out.push_str("  ");
        out.push_str(invocation);
        for _ in 0..pad {
            out.push(' ');
        }
        out.push_str(description);
        out.push('\n');
    }
}

/// The full usage text (as printed by `-h` and on argument errors). The framing
/// labels are translated (mirroring the reference `_()` call sites); the flag /
/// command descriptions are literal, as in the reference.
pub fn usage(exec: &str) -> String {
    let usage_label = t!("label-usage");
    let mut out = String::new();
    out.push_str(&format!(
        "{usage_label}: {exec} [{}] <infile.asm>\n",
        t!("label-options-arg")
    ));
    // Align the second line under `<infile.asm>`: label + ": " + exec + " ".
    let indent = usage_label.len() + 2 + exec.len() + 1;
    for _ in 0..indent {
        out.push(' ');
    }
    out.push_str(&format!(
        "<{}> [{}]\n\n",
        t!("label-command"),
        t!("label-args")
    ));
    out.push_str(&format!("{}:\n", t!("label-options")));
    print_block(OPTIONS, &mut out);
    out.push_str(&format!("\n{}:\n", t!("label-commands")));
    print_block(COMMANDS, &mut out);
    out
}

/// The version banner (`nessemble v<version>` + copyright line).
pub fn version() -> String {
    format!(
        "{PROGRAM_NAME} v{PROGRAM_VERSION}\n\n{} {PROGRAM_COPYRIGHT} {PROGRAM_AUTHOR}\n",
        t!("label-copyright")
    )
}

/// The full license output (version banner + GPL notice).
pub fn license() -> String {
    format!("{}\n{LICENSE_TEXT}", version())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_uses_workspace_version() {
        assert_eq!(
            version(),
            format!(
                "nessemble v{}\n\nCopyright 2017 Kevin Selwyn\n",
                env!("CARGO_PKG_VERSION")
            )
        );
    }

    #[test]
    fn license_starts_with_version_and_has_gpl() {
        let text = license();
        assert!(text.starts_with(&version()));
        assert!(text.contains("GNU General Public License"));
    }

    #[test]
    fn usage_omits_out_of_scope_surface() {
        let text = usage("nessemble");
        // In-scope headers and a couple of representative rows are present.
        assert!(text.contains("Options:"));
        assert!(text.contains("Commands:"));
        assert!(text.contains("-o, --output <outfile.rom>  output file"));
        assert!(text.contains("init [<arg> ...]"));
        // Out-of-scope flags and commands must appear nowhere. ("install" is
        // intentionally excluded — it legitimately appears in "install
        // scripts", the in-scope `scripts` command's description.)
        for forbidden in [
            "disassemble",
            "reassemble",
            "simulate",
            "recipe",
            "registry",
            "publish",
            "adduser",
            "logout",
        ] {
            assert!(!text.contains(forbidden), "usage leaked `{forbidden}`");
        }
    }
}
