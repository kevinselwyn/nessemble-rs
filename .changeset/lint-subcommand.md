---
nessemble: minor
---
Add a `nessemble lint` subcommand that reports style problems without rewriting source — the ESLint to `nessemble format`'s Prettier. Its first rule, `require-block-comment`, flags a block-opening label that has no comment nearby. Configure it in `.nessemblerc` under a `lint` block: per-rule `off`/`warn`/`error` severities, a comment `window`, and an `ignore` list of regexes that exempt matching label names (e.g. machine-generated `loc_`/`data_` labels). Errors fail the run; warnings do not unless `--max-warnings` is exceeded.
