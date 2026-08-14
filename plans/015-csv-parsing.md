# nessemble-rs: A Plan for CSV Parsing in Custom Pseudo-Ops

> Status: **Shipped** ([§7](#7-phased-plan)).

## 1. Goal

CSV/TSV is the natural authoring format for flat, row-per-record data — a
per-entity-type stat table, a drop table, a per-frame animation table — the way
[plan 013](013-structured-data-parsing.md) made XML the natural format for
nested, tree-shaped data. A script wanting it today hand-rolls a parser on top
of `open_file(path, "r").read_string()`: split lines, split fields, trim, build
a header→index map, validate row/column counts — generic infrastructure a
concrete case (a 25-row, nine-column entity stat table, `entity_stats.rhai`)
spent roughly 60 of its ~110 lines on before reaching any table-specific logic.

This plan gives `parse_csv`/`parse_csv_file` the same one-call convenience
`parse_xml`/`parse_xml_file` already give tree-shaped documents:

```rhai
fn custom(ints, texts) {
    let table = parse_csv_file(texts[0]);
    let out = [];
    for row in table {
        out.push(parse_int(row["max_speed_x"], 16));
    }
    out
}
```

## 2. API

```rhai
let table = parse_csv_file(path);                     // host-parsed table, comma-delimited
let table = parse_csv_file(path, #{ delimiter: "\t" }); // TSV, or any single-character delimiter
let table = parse_csv(text);                           // same, from a string already in hand
let table = parse_csv(text, #{ delimiter: "\t" });
```

A `csv_table` handle (opaque, `Arc`-backed — see [§4](#4-representation-arcd-handles-like-xml-not-eager-conversion-like-json)):

| Member | Type | Notes |
| --- | --- | --- |
| `.headers()` | array of string | column names, in file order |
| `.rows()` | array of `csv_row` | every data row (the header row is not one) |
| `.len()` | int | row count, same as `table.rows().len()` without building the array |
| iteration | — | `for row in table { ... }` walks `.rows()` |

A `csv_row` handle, indexable **both** ways the request asks for:

| Expression | Notes |
| --- | --- |
| `row["max_speed_x"]` | by column name — the first header with that name, if the header row has a duplicate |
| `row[1]` | by zero-based position, matching every other array-like index in this host (`img.pixel(x, y)`, `node.children[i]`) |

Both throw on a bad index — an unknown column name, or an out-of-range
position — rather than returning `()`, unlike `xml_node.attr(name)`
([§5.1](#51-a-bad-row-index-throws-rather-than-returning-)).

Every field is a plain string, exactly as an XML attribute value is always a
string ([§9, non-goals](#9-non-goals)): no numeric coercion, no automatic
whitespace trimming ([§5.2](#52-fields-are-not-auto-trimmed)).

## 3. Parser rules

One dependency-free hand-rolled tokenizer (`crates/nessemble-script/src/csv.rs`),
the same choice XML made and for the same reason
(plan 013 §11.4 — no CSV crate in this workspace, and the grammar this feature
needs is bounded):

- **RFC 4180-style quoting.** A field beginning with `"` is a quoted field:
  everything up to the matching closing quote is literal (including embedded
  delimiters and newlines), and `""` inside it decodes to one literal `"`. An
  unquoted field ends at the next delimiter, line ending, or end of input.
- **A configurable delimiter**, one character, comma by default
  (`#{ delimiter: "\t" }` for TSV). The quote character is always `"` — not
  configurable, matching every real-world CSV dialect this feature targets.
- **Blank lines are skipped, not turned into a phantom row.** A line is
  "blank" when it is zero characters between line endings — not merely a line
  whose fields are all empty (a single stray comma is one row of two empty
  fields, not blank). This covers stray leading/trailing newlines the same way
  `parse_int_list` skips a delimiter run's phantom empty fields (plan 013 §5).
- **`\r\n` and bare `\n` line endings**, so a Windows-authored CSV and a
  Unix-authored one parse identically.
- **The first non-blank line is the header row.** Every later row's field
  count must match the header's column count exactly — see
  [§5.3](#53-row-column-count-errors-name-the-file-line-and-column).
- **Errors carry line and column** (the parser's own cursor, precisely as
  `xml.rs`'s `Parser` tracks them) and surface through the directive's
  ordinary thrown-string error path, prefixed with the file path for
  `parse_csv_file` — plan 013 §2.6's reasoning applies verbatim: one
  diagnostic, on the directive's own line, worded so a script author knows
  which file and where without opening a generated intermediate.

## 4. Representation: `Arc`'d handles, like XML, not eager conversion like JSON

Plan 013 §2.4/§11.3 drew this exact line already and the same test applies
here: does the value have per-node behavior worth deferring, or does it
convert cleanly into values a script would have built by hand anyway?

CSV lands on the XML side, not the JSON side — `row[name]`/`row[idx]` is a
per-row *behavior* (dual-mode indexing no native Rhai map or array offers
together), not a value shape `serde_json::Value`'s recursive conversion
already produces for free. `csv_table`/`csv_row` are therefore `Arc`-backed
handles: cloning one (assigning it, passing it to a helper function, storing
several in an array) is a refcount bump, the same guarantee `Image` and
`xml_node` already give. `.rows()` still builds the whole array eagerly (it is
flat data, not a tree a caller might only walk part of), but each element of
that array is a cheap handle clone, not a copy of every field's string data.

## 5. Decisions

### 5.1 A bad row index throws, rather than returning `()`

`xml_node.attr(name)` returns `()` for a missing attribute because a document
legitimately may or may not set one — a script routinely checks. A CSV row's
columns are fixed by the table's own header row: every row has exactly the
same columns, so `row["typo_column"]` is essentially always a programming
mistake, and returning `()` would turn a typo into a silent `()`-shaped bug
three lines later instead of a diagnostic at the indexing expression. Same
reasoning for `row[99]` against a nine-column table.

### 5.2 Fields are not auto-trimmed

The request's own motivating example describes a hand-rolled parser that
trims each field — but the non-goals are explicit that this feature takes no
opinion on a field's content beyond "it is a string," the same restraint
`xml_node.text` already exercises (whitespace-only indentation text is
reported verbatim, not stripped, precisely so the host "stays mechanical").
Auto-trimming would also be a real behavior change inside a quoted field
(`"  padded  "` legitimately wants to keep its spaces), and distinguishing
"trim only unquoted fields" from "trim everything" is exactly the kind of
per-field policy call the request says to leave to the script. Plan 013 §4
already added the tool for the common case — `s.trimmed()` — so
`row["x"].trimmed()` is one call, not a parser feature.

### 5.3 Row/column-count errors name the file, line, and column

The request calls for "a useful parse error that includes file path + line
number + column name (or index)" — the header row fixes what every later
row's shape must be, so a length mismatch is a parse-time error, not a
runtime one a script discovers by indexing past the end. A short row names the
first header its length falls short of (`headers[row.len()]`, since that is
the column a script would have reached for and not found); a long row names
the first extra position it has no header for, by index (there is no header
name to blame). Both are worded like `parse_csv_file`'s other errors —
`` parse_csv_file: stats.csv:6: row has 8 fields, expected 9 (missing \
"max_speed_y") `` — prefixed with the file path and line the same way
`parse_xml_file`'s malformed-XML errors are (plan 013 §2.6).

### 5.4 An unrecognized option key is an error

`#{ delimiter: "\t" }` is the one option this feature defines. A typo
(`delimeter`) that silently fell back to the comma default would produce a
plausible-looking but wrong parse with no diagnostic anywhere near the
mistake — worse than refusing it outright. `parse_csv`/`parse_csv_file`
therefore reject any options map key that is not `delimiter`, naming the
unrecognized key.

### 5.5 No new workspace dependency

Same call as XML (plan 013 §11.4): no `csv` crate is in `Cargo.lock` today,
and RFC 4180's grammar is small enough that a hand-rolled tokenizer — sharing
`xml.rs`'s cursor shape (`rest`/`line`/`col`, `advance`/`peek`/`error`) rather
than introducing a second style — costs less than auditing a general-purpose
CSV crate's dialect-handling surface for behavior this feature does not want.

## 6. Cache correctness

`parse_csv_file` is a fifth route from a path string to a file's bytes,
alongside `read_blob`, `decode_png_file`, `parse_xml_file`, and
`parse_json_file` — it calls the same `resolve`/`record` pair every one of
those already calls before touching the filesystem
(plan 013 §3), so it is a tracked cache dependency for free, and `@/`
resolves for it exactly as it does for the other four (plan 013 §11.1/Phase 1,
already shipped by the time this plan was written — `parse_csv_file` needs no
extra work here, only to be added to that existing, generalized mechanism).

## 7. Phased plan

Single phase — the mechanism this rides on (caching, `@/` resolution, the
host-API catalog, the runaway-script guard) already exists and needed no
changes, unlike plan 013's own Phase 0, which built that mechanism at the same
time as XML/JSON.

- `crates/nessemble-script/src/csv.rs`: the tokenizer/parser, `CsvTable`/
  `CsvRow` types, `parse`/`parse_with_delimiter`.
- `parse_csv`/`parse_csv_file` registered in `engine_recording`
  (`crates/nessemble-script/src/lib.rs`), the file form calling the existing
  `resolve`/`record` choke point (§6) — no change to `nessemble-cli`'s cache.
- `csv_table`/`csv_row` registered as Rhai types; `.headers()`, `.rows()`,
  `.len()` as methods; `register_iterator` for `for row in table`;
  `register_indexer_get` twice on `csv_row` (once for `&str`, once for `int`)
  for `row[name]`/`row[idx]`.
- `crates/nessemble-script-api`: catalog entries for `parse_csv`,
  `parse_csv_file`, `csv_table`, `headers`, `rows`, `len`, under
  `Domain::StructuredData`. Row indexing is not a named registration (Rhai
  indexers have no script-visible name), so — like `path`
  (`nessemble-script/tests/api_catalog.rs`'s own `NOT_SCRIPT_FACING`) — the
  drift test's source scan never sees `register_indexer_get`'s call and
  requires no entry for it; the docs prose still spells out `row[name]`/
  `row[idx]` in full.
- `docs/src/extending.md`: a `#### CSV` section alongside `#### XML`/`#### JSON`
  under "Parsing structured data," and the generated table of contents
  (`cargo run -p xtask -- script-api`) picks up the new catalog entries.
- Tests: parser unit tests in `csv.rs` (quoting, escaped quotes, delimiter
  embedded in quotes, blank-line skipping, `\r\n`/`\n`, header mismatch
  naming the column, line/column on error) mirroring `xml.rs`'s own; the
  existing `lib.rs` test module gains cases mirroring
  `records_every_route_from_a_path_to_a_file` and
  `every_path_taking_function_honors_at_slash` for the new function, an error
  test mirroring `parse_xml_file_error_names_the_file_and_position`, and a
  byte-for-byte-reproduction test mirroring
  `parse_xml_file_reproduces_a_compiled_conversion_byte_for_byte`.
- Changeset: `minor` (new script-facing functionality, no breaking change).

## 8. Acceptance

- A script using `parse_csv_file` reproduces, byte for byte, a data table an
  equivalent compiled implementation produces from the same CSV.
- `row["name"]` and `row[i]` both read the same field; iterating `table`
  visits every data row in file order.
- A quoted field with an embedded delimiter, newline, or escaped quote reads
  back exactly as written.
- A row whose field count disagrees with the header is a parse-time error
  naming the file, line, and the column it disagrees about — not a value a
  script has to notice is short by indexing past the end.
- Editing a source `.csv` invalidates exactly the cache entries that read it,
  the same as every other path-taking script function (§6).
- Existing scripts and directives are unchanged (no existing registration,
  key shape, or public function signature changes).

## 9. Non-goals

Unchanged from the request:

- **Writing CSV.** Reading is the asymmetric win, as with XML/JSON (plan 013
  §9) — round-tripping a specific tool's formatting is the downstream
  project's problem.
- **Numeric coercion.** Every field is a string; a script that wants a hex
  byte, a decimal, or a label name decides how to read it, the same as an XML
  attribute value.
- **A "schema" concept**, or anything else CSV tooling offers that XML's
  tree-query convenience does not already parallel for this host — parity
  with `parse_xml_file` for the flat/tabular case, not a general-purpose CSV
  feature set.
