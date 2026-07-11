//! Integration tests for the `nessemble` CLI surface: exit codes, help/version
//! text, `init` scaffolding, `config` round-tripping, and i18n locale loading.

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
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "nessemble v1.1.1\n\nCopyright 2017 Kevin Selwyn\n"
    );
}

#[test]
fn unknown_option_is_a_usage_error() {
    let out = bin().arg("-z").output().unwrap();
    assert_eq!(out.status.code(), Some(129));
    assert!(String::from_utf8(out.stdout).unwrap().contains("Usage:"));
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
fn coverage_reports_per_bank_for_ines_file_output() {
    let dir = std::env::temp_dir().join(format!("nessemble-cov-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let asm = dir.join("cov.asm");
    let nes = dir.join("cov.nes");
    // One PRG + one CHR bank, with a couple of emitted bytes.
    std::fs::write(&asm, ".inesprg 1\n.ineschr 1\n    LDA #$01\n    BRK\n").unwrap();

    let out = bin()
        .args([
            "-C",
            "-f",
            "nes",
            "-o",
            nes.to_str().unwrap(),
            asm.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    // Three emitted bytes (A9 01 00) land in PRG bank 0.
    assert_eq!(text, "PRG 00:     3/16384\nCHR 00:     0/8192 \n");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn config_round_trips_in_isolated_home() {
    let home = std::env::temp_dir().join(format!("nessemble-home-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();

    // set
    assert!(bin()
        .env("HOME", &home)
        .args(["config", "author", "ada"])
        .status()
        .unwrap()
        .success());
    // get
    let got = bin()
        .env("HOME", &home)
        .args(["config", "author"])
        .output()
        .unwrap();
    assert!(got.status.success());
    assert_eq!(String::from_utf8(got.stdout).unwrap(), "ada\n");
    // missing key fails
    let missing = bin()
        .env("HOME", &home)
        .args(["config", "nope"])
        .output()
        .unwrap();
    assert_eq!(missing.status.code(), Some(1));

    let _ = std::fs::remove_dir_all(&home);
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
