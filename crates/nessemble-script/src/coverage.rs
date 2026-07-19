//! Optional **line coverage** for Rhai pseudo-op scripts (feature `coverage`).
//!
//! Runtime CDL coverage cannot see scripts — they run *inside the assembler*, not
//! on the NES — so this is a separate instrumentation path that feeds the same
//! report. [`run_with_coverage`] executes a script's `custom()` on a
//! debugger-instrumented engine that records which source lines run, while the
//! set of *coverable* lines is taken from the compiled AST (so a line that never
//! runs still shows up as uncovered rather than vanishing). Hits accumulate
//! across every invocation of a script during one assembly.
//!
//! Enabling this feature turns on Rhai's `debugging` and `internals` APIs; the
//! debugger is registered **only** here, so ordinary [`crate::run`] execution is
//! unaffected.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use rhai::debugger::DebuggerCommand;
use rhai::{ASTNode, Array, Dynamic, Scope};

use super::{dynamic_to_bytes, engine, error_message};

/// Accumulated coverage for every instrumented script in one assembly, keyed by
/// the script's path.
#[derive(Debug, Default)]
pub struct ScriptCoverage {
    files: BTreeMap<PathBuf, FileHits>,
}

/// Per-script line sets: which lines *could* run (from the AST) and which *did*.
#[derive(Debug, Default)]
struct FileHits {
    coverable: BTreeSet<u32>,
    hit: BTreeSet<u32>,
}

impl ScriptCoverage {
    /// A fresh, empty collector.
    #[must_use]
    pub fn new() -> ScriptCoverage {
        ScriptCoverage::default()
    }

    /// Whether any script was recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Iterate the recorded scripts as `(path, rows)`, where `rows` lists every
    /// coverable line (the AST's lines plus any line that actually ran) in
    /// ascending order, each paired with whether it executed.
    pub fn files(&self) -> impl Iterator<Item = (&Path, Vec<(u32, bool)>)> {
        self.files.iter().map(|(path, f)| {
            let mut lines = f.coverable.clone();
            lines.extend(f.hit.iter().copied());
            let rows = lines
                .into_iter()
                .map(|line| (line, f.hit.contains(&line)))
                .collect();
            (path.as_path(), rows)
        })
    }
}

/// A shared, mutable [`ScriptCoverage`] the resolver writes to across every
/// script invocation during one assembly.
pub type SharedCoverage = Rc<RefCell<ScriptCoverage>>;

/// Run a script's `custom(ints, texts)` like [`crate::run`], but on a
/// debugger-instrumented engine that records which source lines execute,
/// accumulating into `cov` under `script_path`.
///
/// The coverable-line denominator comes from the compiled AST (every statement
/// and expression position), so lines that never run are still counted. Coverage
/// is recorded even when the script errors — the lines it reached before failing
/// are covered.
///
/// # Errors
/// Returns the compile or run error message, exactly as [`crate::run`] would.
pub fn run_with_coverage(
    source: &str,
    ints: &[i64],
    texts: &[String],
    base_dir: &Path,
    script_path: &Path,
    cov: &SharedCoverage,
) -> Result<Vec<u8>, String> {
    let hits: Rc<RefCell<BTreeSet<u32>>> = Rc::new(RefCell::new(BTreeSet::new()));

    let mut engine = engine(base_dir);
    {
        let hits = hits.clone();
        // `register_debugger` is a stable-but-volatile Rhai API (marked
        // deprecated only to signal that); it is the supported way to observe
        // execution. Step into every node so each executed line is seen.
        #[allow(deprecated)]
        engine.register_debugger(
            |_engine, debugger| debugger,
            move |_ctx, _event, _node, _source, pos| {
                if let Some(line) = pos.line() {
                    hits.borrow_mut().insert(line as u32);
                }
                Ok(DebuggerCommand::StepInto)
            },
        );
    }

    let ast = engine.compile(source).map_err(|e| e.to_string())?;

    // Coverable lines: every AST node position, including the `custom` function
    // body (`AST::walk` descends into function bodies).
    let mut coverable: BTreeSet<u32> = BTreeSet::new();
    ast.walk(&mut |path: &[ASTNode]| {
        if let Some(line) = path.last().and_then(|node| node.position().line()) {
            coverable.insert(line as u32);
        }
        true
    });

    let int_arr: Array = ints.iter().map(|&i| Dynamic::from(i)).collect();
    let text_arr: Array = texts.iter().map(|t| Dynamic::from(t.clone())).collect();
    let mut scope = Scope::new();
    let result = engine.call_fn::<Dynamic>(&mut scope, &ast, "custom", (int_arr, text_arr));

    {
        let mut cov = cov.borrow_mut();
        let entry = cov.files.entry(script_path.to_path_buf()).or_default();
        entry.coverable.extend(coverable);
        entry.hit.extend(hits.borrow().iter().copied());
    }

    match result {
        Ok(value) => dynamic_to_bytes(value),
        Err(err) => Err(error_message(&err)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A script whose `if` takes one branch or the other by the sign of `ints[0]`.
    const SRC: &str = concat!(
        "fn custom(ints, texts) {\n", // 1
        "    let x = 0;\n",           // 2
        "    if ints[0] > 0 {\n",     // 3
        "        x = 10;\n",          // 4  (then branch)
        "    } else {\n",             // 5
        "        x = 20;\n",          // 6  (else branch)
        "    }\n",                    // 7
        "    [x]\n",                  // 8
        "}\n",                        // 9
    );

    fn uncovered(cov: &SharedCoverage) -> Vec<u32> {
        let cov = cov.borrow();
        let (_, rows) = cov.files().next().expect("one script recorded");
        rows.into_iter()
            .filter(|(_, hit)| !hit)
            .map(|(line, _)| line)
            .collect()
    }

    #[test]
    fn records_executed_and_coverable_lines() {
        let cov: SharedCoverage = Rc::new(RefCell::new(ScriptCoverage::new()));
        let path = Path::new("test.rhai");

        // Take the `then` branch: `x = 10` runs, `x = 20` does not.
        let out = run_with_coverage(SRC, &[1], &[], Path::new("."), path, &cov).unwrap();
        assert_eq!(out, vec![10]);
        let after_then = uncovered(&cov);
        // The else branch (and any structural line) is uncovered so far.
        assert!(
            !after_then.is_empty(),
            "else branch should be uncovered after only the then branch ran"
        );

        // Now take the `else` branch too; hits accumulate across invocations.
        let out = run_with_coverage(SRC, &[-1], &[], Path::new("."), path, &cov).unwrap();
        assert_eq!(out, vec![20]);
        let after_both = uncovered(&cov);
        assert!(
            after_both.len() < after_then.len(),
            "covering the else branch must reduce the uncovered set: {after_then:?} -> {after_both:?}"
        );
    }

    #[test]
    fn records_coverage_even_when_the_script_throws() {
        let cov: SharedCoverage = Rc::new(RefCell::new(ScriptCoverage::new()));
        let src = concat!(
            "fn custom(ints, texts) {\n",
            "    let y = 1;\n",
            "    throw \"boom\";\n",
            "}\n",
        );
        let err = run_with_coverage(src, &[], &[], Path::new("."), Path::new("t.rhai"), &cov)
            .unwrap_err();
        assert_eq!(err, "boom");
        // The lines reached before the throw were still recorded.
        assert!(!cov.borrow().is_empty());
    }
}
