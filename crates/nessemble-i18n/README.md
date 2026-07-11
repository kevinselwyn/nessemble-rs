# nessemble-i18n

Internationalization for `nessemble-rs`, built on [Project Fluent](https://projectfluent.org).

Every user-facing string — assembler diagnostics, CLI usage/version, `init`
prompts, `config`/`scripts` output — is looked up by a stable message id from a
per-locale Fluent catalog. This is the equivalent of the reference C tool's
gettext `_()` call sites.

`en-US` ships embedded (`locales/en-US.ftl`) and is always the fallback: any
message a selected locale does not translate falls back to its `en-US` value, so
a partial translation is always usable.

## Adding a locale

There are two ways to add a locale; both key off the message ids in
`locales/en-US.ftl`.

### 1. Ship it with the binary

Add `locales/<lang>.ftl` (e.g. `locales/de.ftl`) and register it at startup.
Copy `en-US.ftl`, translate the values (keep the ids and `{ $variables }`), and
embed it the same way `en-US` is embedded in `src/lib.rs`.

### 2. Drop it in at runtime (no rebuild)

Place a file at `~/.nessemble/locales/<lang>.ftl` and select it with the
`NESSEMBLE_LANG` environment variable (or the standard `LANG` / `LC_ALL`):

```sh
# ~/.nessemble/locales/de.ftl
#   no-errors = Alles gut
NESSEMBLE_LANG=de nessemble -c game.asm
# -> Alles gut
```

The CLI scans `~/.nessemble/locales/*.ftl` on startup (the file stem is the
locale id) and registers each. Only the messages you translate are overridden;
everything else falls back to `en-US`.

## Notes for translators

- Message ids are stable API — never rename them; only change the values.
- Interpolate variables as `{ $name }`; the variable names are part of the id's
  contract (see `en-US.ftl`).
- To keep a **trailing space** (Fluent trims trailing whitespace), write it as an
  explicit literal, e.g. `init-prompt-filename = Filename:{ " " }`.
- Numbers are interpolated verbatim (no locale grouping), so `{ $line }` renders
  `1234`, never `1,234`.
