---
nessemble: minor
---
Comment directives are now validated and can exclude lines from coverage.

- **Coverage ignores.** `; @nessemble-coverage-ignore-next-line` drops the next significant line from a `nessemble coverage` report, and `; @nessemble-coverage-ignore start` / `end` drops a region — a region left unclosed runs to the end of the file, which is how a whole file opts out. Excluded lines leave both the numerator and the denominator; the run reports how many were dropped, JSON carries `ignored` / `ignoredFiles` counts, and `--no-ignore` reports every line regardless. Rhai scripts honor the same directives written as `//` comments.
- **Linting.** Three new rules — `unknown-comment-directive`, `deprecated-comment-directive`, and `ineffective-comment-directive` — catch a directive that is mistyped, written with the deprecated `@fmt` spelling, or placed where it cannot apply. They are configurable in `.nessemblerc` like any other rule, and `nessemble lint` rows now print the finding's message.
- **Editor.** The same findings appear inline; comments offer directive completions with documentation; hovering a directive explains it; a quick fix renames `@fmt` to `@nessemble-format`; and a comment carrying a directive gets the `documentation` semantic-token modifier so themes can set it apart.
