//! `init` subcommand: scaffold a new project `.asm`, matching the reference
//! `init.c` output. Values are taken from positional arguments; any not given
//! are prompted for interactively (filename, PRG banks, CHR banks, mapper,
//! mirroring).

use std::io::{BufRead, Write};

use nessemble_i18n::t;

/// The PRG-bank-0 body template (reference `static/init.txt`).
const INIT_TEMPLATE: &str = include_str!("data/init.txt");

/// Run `init` with the given positional arguments, returning the process exit
/// code (0 on success, 1 on failure).
pub fn run(args: &[String]) -> u8 {
    let filename = match arg_or_prompt(args, 0, &t!("init-prompt-filename")) {
        Some(f) => f,
        None => return 1,
    };
    let prg = match int_arg_or_prompt(
        args,
        1,
        &t!("init-prompt-prg"),
        0,
        i32::MAX,
        &t!("init-choose-banks"),
    ) {
        Some(v) => v,
        None => return 1,
    };
    let chr = match int_arg_or_prompt(
        args,
        2,
        &t!("init-prompt-chr"),
        0,
        i32::MAX,
        &t!("init-choose-banks"),
    ) {
        Some(v) => v,
        None => return 1,
    };
    let mapper = match int_arg_or_prompt(
        args,
        3,
        &t!("init-prompt-mapper"),
        0,
        0xFF,
        &t!("init-choose-mapper"),
    ) {
        Some(v) => v,
        None => return 1,
    };
    let mirroring = match int_arg_or_prompt(
        args,
        4,
        &t!("init-prompt-mirroring"),
        0,
        0x0F,
        &t!("init-choose-mirroring"),
    ) {
        Some(v) => v,
        None => return 1,
    };

    if std::path::Path::new(&filename).exists() && !confirm_overwrite(&filename) {
        return 0;
    }

    match write_project(&filename, prg, chr, mapper, mirroring) {
        Ok(()) => {
            println!("{}", t!("init-created", file = filename));
            0
        }
        Err(e) => {
            eprintln!("nessemble: could not open `{filename}`: {e}");
            1
        }
    }
}

/// Assemble and write the scaffold file, matching the reference byte layout.
fn write_project(
    filename: &str,
    prg: i32,
    chr: i32,
    mapper: i32,
    mirroring: i32,
) -> std::io::Result<()> {
    let mut out = std::fs::File::create(filename)?;
    // The reference applies these (quirky) moduli to mapper/mirroring.
    write!(
        out,
        ".inesprg {prg}\n.ineschr {chr}\n.inesmap {}\n.inesmir {}\n",
        mapper % 0xFF,
        mirroring % 0x0F
    )?;

    let body = INIT_TEMPLATE.strip_suffix('\n').unwrap_or(INIT_TEMPLATE);
    for i in 0..prg.max(0) {
        write!(out, "\n;;;;;;;;;;;;;;;;\n\n.prg {i}\n")?;
        if i == 0 {
            write!(out, "\n{body}\n")?;
        }
    }
    for i in 0..chr.max(0) {
        write!(out, "\n;;;;;;;;;;;;;;;;\n\n.chr {i}\n")?;
    }
    Ok(())
}

/// Take positional arg `idx`, or prompt for a non-empty line.
fn arg_or_prompt(args: &[String], idx: usize, prompt: &str) -> Option<String> {
    if let Some(v) = args.get(idx) {
        return Some(v.clone());
    }
    loop {
        let line = read_line(prompt)?;
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
}

/// Take positional arg `idx` as an integer, or prompt (re-prompting until the
/// value parses and falls within `[lo, hi]`, printing `choose_msg` otherwise).
fn int_arg_or_prompt(
    args: &[String],
    idx: usize,
    prompt: &str,
    lo: i32,
    hi: i32,
    choose_msg: &str,
) -> Option<i32> {
    if let Some(v) = args.get(idx) {
        // The reference uses atoi(): non-numeric input yields 0.
        return Some(v.parse().unwrap_or(0));
    }
    loop {
        let line = read_line(prompt)?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match trimmed.parse::<i32>() {
            Ok(v) if v >= lo && v <= hi => return Some(v),
            Ok(_) => println!("{choose_msg}"),
            Err(_) => continue,
        }
    }
}

fn read_line(prompt: &str) -> Option<String> {
    print!("{prompt}");
    std::io::stdout().flush().ok()?;
    let mut line = String::new();
    match std::io::stdin().lock().read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line),
    }
}

fn confirm_overwrite(filename: &str) -> bool {
    loop {
        print!("{}", t!("init-overwrite", file = filename));
        let _ = std::io::stdout().flush();
        match read_line("") {
            Some(line) => match line.trim().chars().next() {
                Some('y') | Some('Y') => return true,
                Some('n') | Some('N') => return false,
                _ => continue,
            },
            None => return false,
        }
    }
}
