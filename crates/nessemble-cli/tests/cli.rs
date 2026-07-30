//! Integration tests for the `nessemble` CLI surface: exit codes, help/version
//! text, `init` scaffolding, and i18n locale loading.

use std::io::Write;
use std::process::{Command, Stdio};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_nessemble"))
}

#[test]
fn help_exits_129_and_omits_out_of_scope() {
    let out = bin().arg("-h").output().unwrap();
    // The reference returns RETURN_USAGE (129) for -h/-v/-L.
    assert_eq!(out.status.code(), Some(129));
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("Options:") && text.contains("Commands:"));
    for forbidden in [
        "disassemble",
        "reassemble",
        "simulate",
        "registry",
        "publish",
    ] {
        assert!(!text.contains(forbidden), "help leaked `{forbidden}`");
    }
}

#[test]
fn version_exits_129() {
    let out = bin().arg("--version").output().unwrap();
    assert_eq!(out.status.code(), Some(129));
    // The displayed version tracks the workspace version, so assert the framing
    // around the version number rather than the number itself.
    let banner = String::from_utf8(out.stdout).unwrap();
    assert!(banner.starts_with("nessemble v"));
    assert!(banner.ends_with("\n\nCopyright 2017-2026 Kevin Selwyn\n"));
}

#[test]
fn unknown_option_is_a_usage_error() {
    let out = bin().arg("-z").output().unwrap();
    // The reference's usage return code (129) is preserved. clap writes parse
    // errors to stderr (unlike the old hand-rolled parser, which printed the
    // full usage to stdout), so the "Usage:" line now appears there.
    assert_eq!(out.status.code(), Some(129));
    assert!(String::from_utf8(out.stderr).unwrap().contains("Usage:"));
}

#[test]
fn init_scaffolds_expected_project() {
    let dir = std::env::temp_dir().join(format!("nessemble-init-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("game.asm");
    let status = bin()
        .args(["init", file.to_str().unwrap(), "1", "1", "0", "0"])
        .status()
        .unwrap();
    assert!(status.success());

    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.starts_with(
        ".inesprg 1\n.ineschr 1\n.inesmap 0\n.inesmir 0\n\n;;;;;;;;;;;;;;;;\n\n.prg 0\n\n"
    ));
    assert!(text.contains("vblankwait:"));
    assert!(text.contains(".org $FFFA"));
    assert!(text.ends_with("\n.chr 0\n"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn coverage_subcommand_writes_lcov_from_a_cdl() {
    let dir = std::env::temp_dir().join(format!("nessemble-cov-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let asm = dir.join("cov.asm");
    let cdl = dir.join("cov.cdl");
    let lcov = dir.join("out.lcov");
    // One PRG + one CHR bank: LDA #$01 (line 3, bytes 0-1), BRK (line 4, byte 2).
    std::fs::write(&asm, ".inesprg 1\n.ineschr 1\n    LDA #$01\n    BRK\n").unwrap();
    // CDL is PRG(16384)+CHR(8192): mark the LDA bytes as code, leave BRK untouched.
    let mut bytes = vec![0u8; 16384 + 8192];
    bytes[0] = 0x01;
    bytes[1] = 0x01;
    std::fs::write(&cdl, &bytes).unwrap();

    let out = bin()
        .args([
            "coverage",
            asm.to_str().unwrap(),
            "--cdl",
            cdl.to_str().unwrap(),
            "--format",
            "lcov",
            "--out",
            lcov.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // The LDA line is covered, BRK is not → 1 of 2 lines.
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "coverage: 1/2 lines (50.0%)\n"
    );

    let report = std::fs::read_to_string(&lcov).unwrap();
    assert!(report.contains("DA:3,1"), "{report}");
    assert!(report.contains("DA:4,0"), "{report}");
    assert!(report.contains("LF:2"), "{report}");
    assert!(report.contains("LH:1"), "{report}");
    assert!(report.ends_with("end_of_record\n"), "{report}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn coverage_ignore_directives_exclude_lines_and_no_ignore_restores_them() {
    let dir = std::env::temp_dir().join(format!("nessemble-covign-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let asm = dir.join("cov.asm");
    let cdl = dir.join("cov.cdl");
    let json = dir.join("out.json");
    // LDA (line 3, covered) then an uncovered BRK (line 6) excluded by the
    // directive on line 4 — with an explanatory comment in between.
    std::fs::write(
        &asm,
        ".inesprg 1\n.ineschr 1\n    LDA #$01\n; @nessemble-coverage-ignore-next-line\n\
         ; unreachable without the mapper IRQ\n    BRK\n",
    )
    .unwrap();
    let mut bytes = vec![0u8; 16384 + 8192];
    bytes[0] = 0x01;
    bytes[1] = 0x01;
    std::fs::write(&cdl, &bytes).unwrap();

    let run = |extra: &[&str]| {
        let mut argv = vec![
            "coverage",
            asm.to_str().unwrap(),
            "--cdl",
            cdl.to_str().unwrap(),
            "--format",
            "json",
            "--out",
            json.to_str().unwrap(),
        ];
        argv.extend_from_slice(extra);
        let out = bin().args(&argv).output().unwrap();
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    };

    // The excluded line leaves both sides of the ratio: 1/1, not 1/2.
    let stdout = run(&[]);
    assert_eq!(stdout, "coverage: 1/1 lines (100.0%) — 1 line ignored\n");
    let report = std::fs::read_to_string(&json).unwrap();
    assert!(report.contains("\"ignored\": 1"), "{report}");
    assert!(!report.contains("\"line\": 6"), "{report}");

    // `--no-ignore` reports the unfiltered truth.
    let stdout = run(&["--no-ignore"]);
    assert_eq!(stdout, "coverage: 1/2 lines (50.0%)\n");
    let report = std::fs::read_to_string(&json).unwrap();
    assert!(report.contains("\"line\": 6"), "{report}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn coverage_ignore_region_can_exclude_a_whole_file() {
    let dir = std::env::temp_dir().join(format!("nessemble-covfile-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let asm = dir.join("cov.asm");
    let inc = dir.join("dead.asm");
    let cdl = dir.join("cov.cdl");
    let lcov = dir.join("out.lcov");
    std::fs::write(
        &asm,
        ".inesprg 1\n.ineschr 1\n    LDA #$01\n    .include \"dead.asm\"\n",
    )
    .unwrap();
    // An unclosed region at the top is the whole-file opt-out.
    std::fs::write(
        &inc,
        "; @nessemble-coverage-ignore start\n    BRK\n    BRK\n",
    )
    .unwrap();
    let mut bytes = vec![0u8; 16384 + 8192];
    bytes[0] = 0x01;
    bytes[1] = 0x01;
    std::fs::write(&cdl, &bytes).unwrap();

    let out = bin()
        .args([
            "coverage",
            asm.to_str().unwrap(),
            "--cdl",
            cdl.to_str().unwrap(),
            "--format",
            "lcov",
            "--out",
            lcov.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "coverage: 1/1 lines (100.0%) — 1 file ignored\n"
    );
    // The excluded file gets no `SF:` block at all.
    let report = std::fs::read_to_string(&lcov).unwrap();
    assert!(!report.contains("dead.asm"), "{report}");
    assert!(report.contains("cov.asm"), "{report}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn coverage_rejects_a_cdl_of_the_wrong_size() {
    let dir = std::env::temp_dir().join(format!("nessemble-covbad-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let asm = dir.join("cov.asm");
    let cdl = dir.join("bad.cdl");
    std::fs::write(&asm, ".inesprg 1\n.ineschr 1\n    BRK\n").unwrap();
    // Too small: not PRG+CHR sized.
    std::fs::write(&cdl, vec![0u8; 100]).unwrap();

    let out = bin()
        .args([
            "coverage",
            asm.to_str().unwrap(),
            "--cdl",
            cdl.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("PRG+CHR"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "coverage")]
#[test]
fn coverage_scripts_reports_an_unexecuted_rhai_branch() {
    let dir = std::env::temp_dir().join(format!("nessemble-covscr-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // A script with an else branch that never runs for a positive argument.
    std::fs::write(
        dir.join("pick.rhai"),
        "fn custom(ints, texts) {\n    let out = [];\n    if ints[0] > 0 {\n        out += ints[0] & 0xFF;\n    } else {\n        out += 0xFF;\n    }\n    out\n}\n",
    )
    .unwrap();
    std::fs::write(dir.join("pseudo.txt"), ".pick = pick.rhai\n").unwrap();
    std::fs::write(
        dir.join("main.asm"),
        ".inesprg 1\n.ineschr 1\n    .pick 5\n    RTS\n",
    )
    .unwrap();
    let mut cdl = vec![0u8; 16384 + 8192];
    for b in &mut cdl[0..4] {
        *b = 0x01;
    }
    std::fs::write(dir.join("main.cdl"), &cdl).unwrap();
    let lcov = dir.join("cov.lcov");

    let out = bin()
        .args([
            "coverage",
            dir.join("main.asm").to_str().unwrap(),
            "--cdl",
            dir.join("main.cdl").to_str().unwrap(),
            "-p",
            dir.join("pseudo.txt").to_str().unwrap(),
            "--scripts",
            "--format",
            "lcov",
            "--out",
            lcov.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report = std::fs::read_to_string(&lcov).unwrap();
    // The script file appears, with its else-branch line (line 6) unexecuted.
    assert!(report.contains("pick.rhai"), "{report}");
    assert!(report.contains("DA:6,0"), "{report}");
    // And an executed line is marked hit.
    assert!(report.contains("DA:4,1"), "{report}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_dropped_in_locale_localizes_output_end_to_end() {
    // A translator drops `~/.nessemble/locales/<lang>.ftl`; selecting it with
    // NESSEMBLE_LANG localizes output, and messages the locale omits fall back
    // to en-US.
    let home = std::env::temp_dir().join(format!("nessemble-i18n-{}", std::process::id()));
    let locales = home.join(".nessemble").join("locales");
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&locales).unwrap();
    std::fs::write(
        locales.join("de.ftl"),
        "no-errors = Alles gut\ninvalid-mode = Ungueltiger Modus\n",
    )
    .unwrap();

    // A CLI message: `-c` on empty input prints the (overridden) "No errors".
    let child = bin()
        .env("HOME", &home)
        .env("NESSEMBLE_LANG", "de")
        .arg("-c")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(String::from_utf8(out.stdout).unwrap(), "Alles gut\n");

    // A core diagnostic: the localized message is embedded in the (en-US) frame.
    let mut child = bin()
        .env("HOME", &home)
        .env("NESSEMBLE_LANG", "de")
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"    LDA [$0000]\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("Ungueltiger Modus"), "stderr = {stderr:?}");

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn format_prints_to_stdout_and_leaves_file_untouched() {
    let dir = std::env::temp_dir().join(format!("nessemble-fmt-out-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("messy.asm");
    let original = "label:\n      LDA #$00\n.db 1,2,  3\n";
    std::fs::write(&file, original).unwrap();

    let out = bin()
        .args(["format", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "label:\n    LDA #$00\n.db 1, 2, 3\n"
    );
    // The file itself is not modified in stdout mode.
    assert_eq!(std::fs::read_to_string(&file).unwrap(), original);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn format_check_exits_nonzero_and_lists_unformatted() {
    let dir = std::env::temp_dir().join(format!("nessemble-fmt-check-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("messy.asm");
    std::fs::write(&file, "label:\n      LDA #$00\n").unwrap();

    let out = bin()
        .args(["format", "--check", file.to_str().unwrap()])
        .output()
        .unwrap();
    // Non-zero exit, the differing path on stdout, and no write.
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        format!("{}\n", file.display())
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "label:\n      LDA #$00\n"
    );

    // An already-formatted file passes the check with exit 0 and no output.
    std::fs::write(&file, "label:\n    LDA #$00\n").unwrap();
    let out = bin()
        .args(["format", "--check", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8(out.stdout).unwrap().is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn format_write_edits_in_place_and_reports_changed_files() {
    let dir = std::env::temp_dir().join(format!("nessemble-fmt-write-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    let a = dir.join("a.asm");
    let b = dir.join("sub/b.asm");
    let txt = dir.join("skip.txt");
    std::fs::write(&a, "a:\n  NOP\n").unwrap();
    std::fs::write(&b, "b:\n   RTS\n").unwrap();
    std::fs::write(&txt, "not asm\n").unwrap();

    // A directory is walked recursively for `.asm` files only.
    let out = bin()
        .args(["format", "--write", dir.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let reported = String::from_utf8(out.stdout).unwrap();
    assert!(reported.contains(&format!("formatted {}", a.display())));
    assert!(reported.contains(&format!("formatted {}", b.display())));

    assert_eq!(std::fs::read_to_string(&a).unwrap(), "a:\n    NOP\n");
    assert_eq!(std::fs::read_to_string(&b).unwrap(), "b:\n    RTS\n");
    // A non-`.asm` file is left alone.
    assert_eq!(std::fs::read_to_string(&txt).unwrap(), "not asm\n");

    // Re-running is a no-op: nothing changes, nothing is reported.
    let out = bin()
        .args(["format", "--write", dir.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8(out.stdout).unwrap().is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn format_directory_without_write_or_check_is_a_usage_error() {
    let dir = std::env::temp_dir().join(format!("nessemble-fmt-diruse-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.asm"), "a:\n  NOP\n").unwrap();

    let out = bin()
        .args(["format", dir.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(129));
    assert!(String::from_utf8(out.stderr)
        .unwrap()
        .contains("requires --write or --check"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn format_missing_path_reports_error() {
    let out = bin()
        .args(["format", "/no/such/nessemble/file.asm"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8(out.stderr)
        .unwrap()
        .contains("no such file or directory"));
}

#[test]
fn format_help_lists_options() {
    let out = bin().args(["format", "-h"]).output().unwrap();
    assert_eq!(out.status.code(), Some(129));
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("--write"));
    assert!(text.contains("--check"));
    assert!(text.contains("--config"));
    assert!(text.contains("--no-config"));
}

#[test]
fn format_applies_nessemblerc_data_per_line() {
    let dir = std::env::temp_dir().join(format!("nessemble-rc-dpl-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(".nessemblerc"), r#"{"dataPerLine": 2}"#).unwrap();
    let file = dir.join("d.asm");
    std::fs::write(&file, ".db $01\n.db $02\n.db $03\n").unwrap();

    // dataPerLine=2 → two values per consolidated line.
    let out = bin()
        .args(["format", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        ".db $01, $02\n.db $03\n"
    );

    // --no-config ignores it and uses the default (eight per line → one line).
    let out = bin()
        .args(["format", "--no-config", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        ".db $01, $02, $03\n"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn format_applies_case_normalization_from_config() {
    let dir = std::env::temp_dir().join(format!("nessemble-rc-case-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(".nessemblerc"),
        r#"{"mnemonicCase": "upper", "hexDigitCase": "lower"}"#,
    )
    .unwrap();
    let file = dir.join("d.asm");
    std::fs::write(&file, "start:\n    lda #$AB\n").unwrap();

    let out = bin()
        .args(["format", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "start:\n    LDA #$ab\n"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn format_explicit_config_file() {
    let dir = std::env::temp_dir().join(format!("nessemble-rc-explicit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let cfg = dir.join("my.json");
    std::fs::write(&cfg, r#"{"dataPerLine": 1}"#).unwrap();
    let file = dir.join("d.asm");
    std::fs::write(&file, ".db $01\n.db $02\n").unwrap();

    let out = bin()
        .args([
            "format",
            "--config",
            cfg.to_str().unwrap(),
            file.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8(out.stdout).unwrap(), ".db $01\n.db $02\n");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn format_rejects_unknown_config_key() {
    let dir = std::env::temp_dir().join(format!("nessemble-rc-bad-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(".nessemblerc"), r#"{"dataPerline": 2}"#).unwrap();
    let file = dir.join("d.asm");
    std::fs::write(&file, ".db $01\n").unwrap();

    let out = bin()
        .args(["format", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8(out.stderr)
        .unwrap()
        .contains("unknown field `dataPerline`"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn format_override_and_ignore_and_extensions() {
    let dir = std::env::temp_dir().join(format!("nessemble-rc-ovr-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("data")).unwrap();
    std::fs::create_dir_all(dir.join("vendor")).unwrap();
    // Base dataPerLine=2; files under data/ get 4; vendor/ is ignored.
    std::fs::write(
        dir.join(".nessemblerc"),
        r#"{"dataPerLine": 2, "overrides": [{"files": "data/**/*.asm", "options": {"dataPerLine": 4}}]}"#,
    )
    .unwrap();
    std::fs::write(dir.join(".nessembleignore"), "vendor/\n").unwrap();
    std::fs::write(dir.join("root.asm"), ".db $01\n.db $02\n.db $03\n").unwrap();
    std::fs::write(
        dir.join("data/t.asm"),
        ".db $01\n.db $02\n.db $03\n.db $04\n.db $05\n",
    )
    .unwrap();
    std::fs::write(dir.join("vendor/v.asm"), ".db $09\n.db $08\n").unwrap();

    let out = bin()
        .args(["format", "--write", dir.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let reported = String::from_utf8(out.stdout).unwrap();
    // vendor/ is ignored — never reported.
    assert!(!reported.contains("vendor"));

    // Base config: two per line.
    assert_eq!(
        std::fs::read_to_string(dir.join("root.asm")).unwrap(),
        ".db $01, $02\n.db $03\n"
    );
    // Override under data/: four per line.
    assert_eq!(
        std::fs::read_to_string(dir.join("data/t.asm")).unwrap(),
        ".db $01, $02, $03, $04\n.db $05\n"
    );
    // Ignored file is untouched.
    assert_eq!(
        std::fs::read_to_string(dir.join("vendor/v.asm")).unwrap(),
        ".db $09\n.db $08\n"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Path to a corpus directory for a scripting example/error case.
fn corpus(group: &str, name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/corpus")
        .join(group)
        .join(name)
}

#[test]
fn custom_pseudo_ops_resolve_via_pseudo_file() {
    let dir = corpus("examples", "custom");
    let out = bin()
        .arg(dir.join("custom.asm"))
        .args(["--pseudo"])
        .arg(dir.join("custom.txt"))
        .args(["--output", "-"])
        .output()
        .unwrap();
    assert!(out.status.success());
    // .sum/.product/.difference/.quotient/.factorial each yield 6.
    assert_eq!(out.stdout, vec![6, 6, 6, 6, 6]);
}

#[test]
fn custom_pseudo_script_resolves_relative_to_the_pseudo_file() {
    // A script path in the `--pseudo` mapping resolves relative to the mapping
    // file's own directory — not the source `.asm` — even when the directive is
    // used from an included file in another directory.
    let root = std::env::temp_dir().join(format!(
        "nessemble-pseudo-rel-{}-{}",
        std::process::id(),
        line!()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("cfg")).unwrap();
    std::fs::create_dir_all(root.join("sub")).unwrap();

    std::fs::write(root.join("main.asm"), b".include \"sub/mod.asm\"\n").unwrap();
    std::fs::write(root.join("sub/mod.asm"), b".double 5\n").unwrap();
    // The mapping and its script live together in `cfg/`; the bare script path
    // resolves against `cfg/`.
    std::fs::write(root.join("cfg/pseudo.txt"), b".double = double.rhai\n").unwrap();
    std::fs::write(
        root.join("cfg/double.rhai"),
        b"fn custom(ints, texts) { [ints[0] * 2] }\n",
    )
    .unwrap();
    // A decoy next to the source file that uses `.double` — the old
    // source-relative behavior would have run this (5 * 100) instead.
    std::fs::write(
        root.join("sub/double.rhai"),
        b"fn custom(ints, texts) { [ints[0] * 100] }\n",
    )
    .unwrap();

    let out = bin()
        .arg(root.join("main.asm"))
        .args(["--pseudo"])
        .arg(root.join("cfg/pseudo.txt"))
        .args(["--output", "-"])
        .output()
        .unwrap();
    assert!(out.status.success());
    // 5 * 2 = 10 (from cfg/double.rhai), not 5 * 100 from the sub/ decoy.
    assert_eq!(out.stdout, vec![10]);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn custom_pseudo_reads_a_file_via_rhai_fs() {
    // End-to-end: a custom directive whose script uses rhai-fs to read a file
    // and emit its bytes. The script's relative path resolves against the `.asm`
    // file's directory (the same base as `.include`/`.incbin`).
    let root = std::env::temp_dir().join(format!(
        "nessemble-fs-embed-{}-{}",
        std::process::id(),
        line!()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();

    std::fs::write(root.join("main.asm"), b".embed \"payload.bin\"\n").unwrap();
    // The asset the script reads, alongside the source file.
    std::fs::write(root.join("payload.bin"), [0xDE, 0xAD, 0xBE, 0xEF]).unwrap();
    // A script that opens the named file and returns its bytes verbatim.
    std::fs::write(
        root.join("embed.rhai"),
        b"fn custom(ints, texts) { open_file(texts[0], \"r\").read_blob() }\n",
    )
    .unwrap();
    std::fs::write(root.join("pseudo.txt"), b".embed = embed.rhai\n").unwrap();

    let out = bin()
        .arg(root.join("main.asm"))
        .args(["--pseudo"])
        .arg(root.join("pseudo.txt"))
        .args(["--output", "-"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(out.stdout, vec![0xDE, 0xAD, 0xBE, 0xEF]);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn bundled_ease_script_resolves_after_install() {
    let home = std::env::temp_dir().join(format!("nessemble-ease-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();

    // Install the bundled scripts, then assemble a file that uses `.ease`.
    assert!(bin()
        .env("HOME", &home)
        .arg("scripts")
        .status()
        .unwrap()
        .success());

    let dir = corpus("examples", "ease");
    let out = bin()
        .env("HOME", &home)
        .arg(dir.join("ease.asm"))
        .args(["--output", "-"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let golden = std::fs::read(dir.join("ease.rom")).unwrap();
    assert_eq!(out.stdout, golden);

    // A bad easing type surfaces the script's thrown message.
    let dir = corpus("errors", "ease-type");
    let out = bin()
        .env("HOME", &home)
        .arg(dir.join("ease-type.asm"))
        .args(["--output", "-"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(out.stderr).unwrap(),
        "Error in `ease-type.asm` on line 1: Invalid easing type `niceAndSlow`\n"
    );

    let _ = std::fs::remove_dir_all(&home);
}

// ─── lint ─────────────────────────────────────────────────────────────────────

/// A fresh temp dir keyed by a unique test suffix (process id is shared across a
/// binary's tests, so the suffix keeps parallel tests from colliding).
fn lint_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("nessemble-lint-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn lint_reports_grouped_warnings_and_exits_zero() {
    let dir = lint_dir("warn");
    let file = dir.join("a.asm");
    // Two undocumented block labels, well clear of any comment.
    std::fs::write(
        &file,
        "\nsound_engine:\n    lda #$00\n    sta $2000\n    rts\n\nnote_table:\n    .db $01\n",
    )
    .unwrap();

    let out = bin()
        .args(["lint", file.to_str().unwrap()])
        .output()
        .unwrap();
    // Warnings alone do not fail the run.
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(
        text.contains("`sound_engine` has no nearby comment  require-block-comment"),
        "{text}"
    );
    assert!(
        text.contains("`note_table` has no nearby comment  require-block-comment"),
        "{text}"
    );
    assert!(text.contains("2 problems (0 errors, 2 warnings)"), "{text}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn lint_clean_file_reports_no_problems() {
    let dir = lint_dir("clean");
    let file = dir.join("a.asm");
    std::fs::write(&file, "\n; documents the routine\nmain:\n    rts\n").unwrap();

    let out = bin()
        .args(["lint", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8(out.stdout)
        .unwrap()
        .contains("No problems"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn lint_error_severity_fails_the_run() {
    let dir = lint_dir("error");
    std::fs::write(
        dir.join(".nessemblerc"),
        r#"{"lint":{"rules":{"require-block-comment":"error"}}}"#,
    )
    .unwrap();
    let file = dir.join("a.asm");
    std::fs::write(&file, "\nwidget:\n    rts\n").unwrap();

    let out = bin()
        .args(["lint", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(
        text.contains("error    code block `widget` has no nearby comment  require-block-comment"),
        "{text}"
    );
    assert!(text.contains("(1 error, 0 warnings)"), "{text}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn lint_ignore_regex_exempts_matching_labels() {
    let dir = lint_dir("ignore");
    std::fs::write(dir.join(".nessemblerc"), r#"{"lint":{"ignore":["^loc_"]}}"#).unwrap();
    let file = dir.join("a.asm");
    std::fs::write(&file, "\nloc_c000:\n    nop\n    rts\n\nreal:\n    rts\n").unwrap();

    let out = bin()
        .args(["lint", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(!text.contains("loc_c000"), "ignored label leaked: {text}");
    assert!(
        text.contains("`real` has no nearby comment  require-block-comment"),
        "{text}"
    );
    assert!(text.contains("(0 errors, 1 warning)"), "{text}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn lint_quiet_suppresses_warnings() {
    let dir = lint_dir("quiet");
    let file = dir.join("a.asm");
    std::fs::write(&file, "\nwidget:\n    rts\n").unwrap();

    let out = bin()
        .args(["lint", "--quiet", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8(out.stdout)
        .unwrap()
        .contains("No problems"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn lint_max_warnings_gate_fails() {
    let dir = lint_dir("maxwarn");
    let file = dir.join("a.asm");
    std::fs::write(&file, "\nalpha:\n    rts\n\nbeta:\n    rts\n").unwrap();

    let out = bin()
        .args(["lint", "--max-warnings", "1", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn lint_walks_a_directory() {
    let dir = lint_dir("walk");
    std::fs::write(dir.join("a.asm"), "\nalpha:\n    rts\n").unwrap();
    std::fs::write(dir.join("b.asm"), "\nbeta:\n    rts\n").unwrap();
    // A non-.asm file is skipped by the directory walk.
    std::fs::write(dir.join("notes.txt"), "gamma:\n").unwrap();

    let out = bin()
        .args(["lint", dir.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(
        text.contains("`alpha` has no nearby comment  require-block-comment"),
        "{text}"
    );
    assert!(
        text.contains("`beta` has no nearby comment  require-block-comment"),
        "{text}"
    );
    assert!(!text.contains("gamma"), "non-asm file linted: {text}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn format_and_lint_agree_on_where_a_stride_hint_binds() {
    // `format` and `lint` resolve the directive's target through one shared
    // lookahead: blank, comment, and label lines are transparent to both. Before
    // that, a label was a lint false positive and a comment a formatter no-op.
    let dir = lint_dir("binding");
    let file = dir.join("a.asm");
    std::fs::write(
        &file,
        "; @nessemble-format stride=1\nfoo:\n; explanation\n    .db $01, $02, $03\n",
    )
    .unwrap();

    let out = bin()
        .args(["format", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "; @nessemble-format stride=1\nfoo:\n; explanation\n.db $01\n.db $02\n.db $03\n"
    );

    let out = bin()
        .args(["lint", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("✓ No problems."), "{text}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn lint_reports_comment_directive_problems() {
    let dir = lint_dir("directives");
    let file = dir.join("a.asm");
    std::fs::write(
        &file,
        "; @nessemble-formt stride=2\n; @fmt stride=2\n.db $01, $02, $03, $04\n\
         \n; @nessemble-coverage-ignore end\n    nop\n",
    )
    .unwrap();

    let out = bin()
        .args(["lint", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "warnings alone must not fail");
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(
        text.contains("unknown comment directive `@nessemble-formt`  unknown-comment-directive"),
        "{text}"
    );
    assert!(
        text.contains(
            "`@fmt` is deprecated; use `@nessemble-format`  deprecated-comment-directive"
        ),
        "{text}"
    );
    assert!(
        text.contains("no matching `start`  ineffective-comment-directive"),
        "{text}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn lint_directive_rules_can_be_configured_off() {
    let dir = lint_dir("directives-off");
    std::fs::write(
        dir.join(".nessemblerc"),
        r#"{"lint":{"rules":{"deprecated-comment-directive":"off"}}}"#,
    )
    .unwrap();
    let file = dir.join("a.asm");
    std::fs::write(&file, "; @fmt stride=2\n.db $01, $02\n").unwrap();

    let out = bin()
        .args(["lint", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("✓ No problems."), "{text}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn lint_unknown_config_rule_is_an_error() {
    let dir = lint_dir("badrule");
    std::fs::write(
        dir.join(".nessemblerc"),
        r#"{"lint":{"rules":{"require-block-commnt":"warn"}}}"#,
    )
    .unwrap();
    let file = dir.join("a.asm");
    std::fs::write(&file, "\nwidget:\n    rts\n").unwrap();

    let out = bin()
        .args(["lint", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8(out.stderr)
        .unwrap()
        .contains("unknown lint rule"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn lint_help_lists_options() {
    let out = bin().args(["lint", "-h"]).output().unwrap();
    assert_eq!(out.status.code(), Some(129));
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("--max-warnings"));
    assert!(text.contains("--quiet"));
    assert!(text.contains("--no-config"));
}

// ---- the custom pseudo-op cache (plan 011, Phases 4–5) --------------------

/// A project tree with its own `HOME` (so the cache lands inside it), holding a
/// source file, a `--pseudo` mapping, and the script the mapping names.
struct CacheProject {
    root: std::path::PathBuf,
}

impl CacheProject {
    /// Build a project whose `.gen` directive runs `script`.
    fn new(tag: &str, source: &str, script: &str) -> CacheProject {
        let root = std::env::temp_dir().join(format!(
            "nessemble-cache-cli-{tag}-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("home")).unwrap();
        std::fs::write(root.join("main.asm"), source).unwrap();
        std::fs::write(root.join("pseudo.txt"), b".gen = gen.rhai\n").unwrap();
        std::fs::write(root.join("gen.rhai"), script).unwrap();
        CacheProject { root }
    }

    /// Assemble to stdout, returning the emitted bytes.
    fn assemble(&self, extra: &[&str]) -> Vec<u8> {
        let out = bin()
            .arg(self.root.join("main.asm"))
            .arg("--pseudo")
            .arg(self.root.join("pseudo.txt"))
            .args(["--output", "-"])
            .args(extra)
            .env("HOME", self.root.join("home"))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "assemble failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    }

    /// The number of cached entries `nessemble cache info` reports.
    fn entry_count(&self) -> usize {
        let out = bin()
            .args(["cache", "info"])
            .env("HOME", self.root.join("home"))
            .output()
            .unwrap();
        assert!(out.status.success());
        let text = String::from_utf8(out.stdout).unwrap();
        let entries = text
            .lines()
            .find_map(|l| l.split_once(" entries"))
            .expect("an entries line");
        entries.0.trim().parse().expect("a count")
    }

    fn clear_cache(&self) -> String {
        let out = bin()
            .args(["cache", "clear"])
            .env("HOME", self.root.join("home"))
            .output()
            .unwrap();
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap()
    }

    /// Overwrite the script, preserving its length and modification time — the
    /// one edit the size+mtime freshness rule cannot see.
    fn rewrite_script_invisibly(&self, script: &str) {
        let path = self.root.join("gen.rhai");
        let before = std::fs::metadata(&path).unwrap().modified().unwrap();
        let old_len = std::fs::metadata(&path).unwrap().len() as usize;
        assert_eq!(script.len(), old_len, "test needs an equal-length rewrite");
        std::fs::write(&path, script).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(before)
            .unwrap();
    }
}

impl Drop for CacheProject {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn assembling_populates_the_cache_and_clear_empties_it() {
    let p = CacheProject::new(
        "populate",
        ".org $C000\n.gen 3\n",
        "fn custom(ints, texts) { [ints[0]] }",
    );

    assert_eq!(p.assemble(&[]), vec![3]);
    assert_eq!(p.entry_count(), 1, "the invocation was cached");
    // A second build hits, and emits the same bytes.
    assert_eq!(p.assemble(&[]), vec![3]);
    assert_eq!(p.entry_count(), 1);

    let cleared = p.clear_cache();
    assert!(cleared.contains("Cleared 1 entries"), "output: {cleared}");
    assert_eq!(p.entry_count(), 0);
}

#[test]
fn no_cache_neither_reads_nor_writes() {
    let p = CacheProject::new(
        "nocache",
        ".org $C000\n.gen 4\n",
        "fn custom(ints, texts) { [ints[0]] }",
    );

    assert_eq!(p.assemble(&["--no-cache"]), vec![4]);
    assert_eq!(p.entry_count(), 0, "nothing was written");
}

#[test]
fn a_random_script_is_never_cached() {
    // Its output is meant to differ per build; freezing it would be wrong.
    let p = CacheProject::new(
        "random",
        ".org $C000\n.gen 1\n",
        "fn custom(ints, texts) { [rand(0, 255)] }",
    );

    p.assemble(&[]);
    assert_eq!(p.entry_count(), 0);
}

#[test]
fn a_hit_answers_without_running_the_script() {
    // The only way to observe "the script did not run" from outside is to change
    // what it would return while leaving its stamp alone — which is exactly the
    // blind spot the size+mtime freshness rule documents. A hit therefore serves
    // the stored bytes, and `--no-cache` (or `cache clear`) is the way out.
    let p = CacheProject::new(
        "hit",
        ".org $C000\n.gen 1\n",
        "fn custom(ints, texts) { [0x11] }",
    );
    assert_eq!(p.assemble(&[]), vec![0x11]);

    p.rewrite_script_invisibly("fn custom(ints, texts) { [0x22] }");
    assert_eq!(p.assemble(&[]), vec![0x11], "served from the cache");
    // The escape hatch reaches the edited script.
    assert_eq!(p.assemble(&["--no-cache"]), vec![0x22]);
}

#[test]
fn an_edited_script_is_not_served_from_the_cache() {
    let p = CacheProject::new(
        "edited",
        ".org $C000\n.gen 1\n",
        "fn custom(ints, texts) { [0x11] }",
    );
    assert_eq!(p.assemble(&[]), vec![0x11]);

    // A normal edit moves the mtime (and here the length too).
    std::fs::write(
        p.root.join("gen.rhai"),
        "fn custom(ints, texts) { [0x22] }   // edited",
    )
    .unwrap();
    assert_eq!(p.assemble(&[]), vec![0x22]);
}

#[test]
fn a_changed_asset_is_not_served_from_the_cache() {
    // The script never declares its input: the host records what it opened.
    let p = CacheProject::new(
        "asset",
        ".org $C000\n.gen 1\n",
        r#"fn custom(ints, texts) { read_blob("asset.bin") }"#,
    );
    std::fs::write(p.root.join("asset.bin"), b"\x01\x02").unwrap();
    assert_eq!(p.assemble(&[]), vec![1, 2]);
    assert_eq!(p.entry_count(), 1);

    std::fs::write(p.root.join("asset.bin"), b"\x09\x08\x07").unwrap();
    assert_eq!(p.assemble(&[]), vec![9, 8, 7], "the asset was re-read");
}
