---
nessemble: minor
---
`; @nessemble-format stride=N` now binds to its data run through one lookahead
shared by `format` and `lint`, so the two can no longer disagree about which run
a directive governs. Blank lines, comment lines, and label/constant definitions
between the directive and the run are **transparent** to both: a directive may
sit above the label that names the run, and an explanation may follow it — the
same courtesy `@nessemble-coverage-ignore-next-line` already documented. The
deprecated `; @fmt` alias binds identically.

Previously the formatter skipped blank and label lines while
`ineffective-comment-directive` skipped blank and comment lines, so a label
between the two produced a spurious warning on a directive that worked, and a
comment between them made the formatter silently ignore the directive with
nothing reported.

> **Expect a formatting diff on the first run after upgrading.** Data runs whose
> stride hint was separated from them by a comment (or by more than one blank
> line) were previously left alone; they are now re-flowed as the hint asks. Run
> `nessemble format --write` once and review the result — no directive changed
> meaning, only which run it attaches to.
