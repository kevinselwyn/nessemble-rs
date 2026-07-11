# Translating

`nessemble` routes every user-facing string through a
[Project Fluent](https://projectfluent.org) catalog, so it can be fully
translated. `en-US` ships built in and is always the fallback: any message a
locale does not translate falls back to its English value.

## Add a locale at runtime

Drop a Fluent file at `~/.nessemble/locales/<lang>.ftl` and select it with the
`NESSEMBLE_LANG` environment variable (or the standard `LANG` / `LC_ALL`):

```text
# ~/.nessemble/locales/de.ftl
no-errors = Alles gut
```

```text
NESSEMBLE_LANG=de nessemble -c game.asm
# -> Alles gut
```

`<lang>` should be a valid locale identifier such as `de`, `de-DE`, or `fr`.

## Notes for translators

- Copy the built-in `en-US.ftl` catalog and translate the **values**; message
  ids are stable and must not be renamed.
- Interpolate variables as `{ $name }` — the variable names are part of each
  message's contract.
- To keep a trailing space (Fluent trims trailing whitespace), write it as an
  explicit literal, e.g. `init-prompt-filename = Filename:{ " " }`.
- Numbers are interpolated verbatim (no locale grouping).

Only the messages you translate are overridden; everything else falls back to
`en-US`.
