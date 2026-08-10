# nessemble-rs: A Plan for Structured Data Parsing in Custom Pseudo-Ops

> Status: **Phase 0 shipped ([§8](#8-phased-plan)); Phases 1–4 designed, not yet
> built.** This document gives custom pseudo-op scripts native, host-side parsers
> for structured text — XML first, JSON alongside it ([§2](#2-native-document-parsing))
> — on the same "host does the byte-level work, the script does the logic"
> contract [`decode_png_file`](../crates/nessemble-script/src/lib.rs) already
> implements for images. It reuses the caching architecture built in
> [plan 011](011-pseudo-op-caching.md) rather than extending it: any new host
> function that opens a file through the existing `path`/`read_blob` registration
> choke point ([§3](#3-cache-correctness-is-inherited-not-rebuilt)) is a tracked
> cache dependency for free. It also closes the small set of string/blob gaps
> found while prototyping ([§4](#4-string-and-blob-helpers)) and scopes the
> larger, riskier parts of the originating request — assembly-source-returning
> directives, parallel script execution, a settable operation limit, and a
> per-directive timing report — as designed-but-deferred phases
> ([§5](#5-deferred-source-returning-directives), [§6](#6-deferred-parallel-script-execution),
> [§7](#7-deferred-operation-limits-and-timing)).
>
> The through-line, inherited from the request this plan answers: **the host does
> byte- and character-level work; the script does the logic.** Rhai is a
> tree-walking interpreter — roughly 10× too slow to tokenize a document, but
> comfortably fast enough to orchestrate one the host has already parsed.

---

## 1. Goal

Give a custom pseudo-op script the same one-call convenience for structured text
that `decode_png_file` already gives it for images:

```rhai
fn custom(ints, texts) {
    let doc = parse_xml_file(texts[0]);        // host-parsed root element
    let out = [];
    for row in doc.find_all("row") {
        out += parse_int_list(row.attr("data"), ",");
    }
    out
}
```

Today the only way to get here is a project-specific pre-build step in another
language that converts assets to `.asm` fragments — a second toolchain, gitignored
intermediates, and (because per-invocation process startup dwarfs the actual
conversion) a batching layer to amortise it. That is a lot of machinery to avoid
work the assembler is already running a scripting engine for.

## 2. Native document parsing

### 2.1 Why Rhai cannot be the tokenizer

Measured against nessemble v2.23.0 on ~900 KB of XML:

| Workload | Time |
| --- | --- |
| Rhai tokenizing tags/attributes and decoding CSV bodies, no conversion logic, written the fast way (`index_of`/`sub_string`, no per-character loops) | ~0.9s |
| A compiled implementation doing the complete parse → transform → emit | ~0.09s |
| Rhai doing orchestration only — ~11k map/array reads against an already-parsed document | ~0.017s |
| nessemble process floor | ~0.004s |

A Rhai XML tokenizer is slower than an entire out-of-process conversion in a
compiled language — porting a parser into a script makes builds worse. The
interpreter is, however, comfortably fast enough to walk a document the host
already parsed. So: parsing is Rust; walking is Rhai.

### 2.2 XML

```rhai
let doc = parse_xml_file(path);     // root element, resolved/read/recorded like read_blob
let doc = parse_xml(source_string); // same, from a string already in hand
```

An `xml_node` handle (opaque, `Arc`-backed like [`Image`](../crates/nessemble-script/src/lib.rs)
— see [§2.4](#24-representation-an-arcd-handle-not-an-eager-tree-of-maps)):

| Member | Type | Notes |
| --- | --- | --- |
| `.name` | string | element name, verbatim (no namespace splitting) |
| `.attrs` | map | attribute name → string value — see [§2.5](#25-attrs-is-sorted-not-insertion-ordered) for the one deviation from the request as written |
| `.attr(name)` | string or `()` | convenience for the above; the common case |
| `.children` | array of `xml_node` | child **elements** only — no text-node children |
| `.text` | string or `()` | concatenated direct text content, entities decoded; `()` if the element has none |
| `.find(name)` | `xml_node` or `()` | first child element with that name |
| `.find_all(name)` | array of `xml_node` | every child element with that name |

Parser rules, in one dependency-free hand-rolled tokenizer
(`crates/nessemble-script/src/xml.rs`):

- **Entities**: the five predefined (`&amp; &lt; &gt; &quot; &apos;`) plus numeric
  character references, decimal (`&#10;`) and hex (`&#x41;`).
- **No DTD, no external entities.** A `<!DOCTYPE` is a **hard parse error**, not a
  silent skip — an assembler that expands external entities on a script's behalf
  has an XXE problem, and no asset pipeline needs one. Comments (`<!-- … -->`),
  processing instructions (`<?xml … ?>`), and `CDATA` sections are recognized and
  skipped/passed through literally (no entity decoding inside `CDATA`).
- **No namespace handling.** `xmlns:foo="…"` is an ordinary attribute; a
  `<ns:tag>` name is the literal string `"ns:tag"`. The request is explicit that
  this is out of scope, and splitting later is a non-breaking addition to `.name`.
- **Errors carry line and column** (from the parser's own cursor) and surface as
  the directive's ordinary thrown-string error, prefixed with the file path for
  `parse_xml_file` — the same path every other host-function error in this crate
  already takes (`decode_png_file: cannot read …`, `decode_png: input is not a
  valid PNG`). See [§2.6](#26-where-file-line-and-column-actually-surface) for what
  "surfaces through the normal directive error path" means precisely.

### 2.3 JSON

The same feature, a different tokenizer, using `serde_json` — already a workspace
dependency (`nessemble-cli`, `nessemble-core`'s dev-dependencies), so this adds no
dependency the workspace does not already build:

```rhai
let doc = parse_json_file(path);
let doc = parse_json(source_string);
```

`serde_json::Value` converts directly to the Rhai type it already resembles —
no opaque handle needed, unlike XML, because JSON has no separate concept of "one
document, walked repeatedly without re-copying it": a script's own local `let`
binding already avoids re-parsing.

| JSON | Rhai |
| --- | --- |
| `null` | `()` |
| `true` / `false` | `bool` |
| a number that fits `i64` | `int` |
| any other number | `float` |
| string | string |
| array | array |
| object | map |

### 2.4 Representation: an `Arc`'d handle, not an eager tree of maps

XML gets the `Image` treatment (`Arc<XmlDoc>` behind an opaque `xml_node` type)
rather than JSON's "convert straight to Rhai values" treatment, for the same
reason `decode_png` doesn't eagerly build `#{ width, height, pixels: [...] }`
(`lib.rs` doc comment on `img_pixels`): a `.find_all(...)` walk over a
thousand-row document must not pay a Rhai-value allocation for every attribute of
every element it doesn't visit. Cloning an `xml_node` (passing it to a helper
function, storing it in an array) is therefore a refcount bump, matching `Image`'s
existing, tested guarantee (`passing_an_image_to_a_function_does_not_copy_it`).

JSON does not get this treatment because there is no equivalent over-fetch risk:
a JSON document converts into exactly the Rhai values a script would have built by
hand walking it, once, and `serde_json::Value` has no per-node behavior (`.find`,
`.attr`) worth deferring — it is the request's own framing, "elements, attributes,
text, entities. No namespaces, no XPath" — none of which JSON has to begin with.

### 2.5 `.attrs` is sorted, not insertion-ordered

The request's table says `.attrs` should be insertion-ordered. `rhai::Map` is a
`BTreeMap<Identifier, Dynamic>` (`rhai-1.25.1/src/lib.rs:304`) — Rhai does not
offer an insertion-ordered map type, so returning `.attrs` as a `rhai::Map` means
returning it key-sorted, not document-ordered, whichever order the attributes were
declared in the source.

This is called out rather than quietly shipped, and the accepted cost is narrow:
`.attr(name)` — the documented common case, and the one every prototype script in
the request's own example uses — is unaffected, since it is a direct lookup, not
an iteration. Only a script that iterates `.attrs` as a whole to *reproduce*
document order (an XML pretty-printer, essentially) would notice, and writing
structured text back out is an explicit non-goal ([§9](#9-non-goals)) — nothing
downstream of this feature is round-tripping formatting. Closing the gap for real
(an ordered-map Rhai type, or a `.attr_names()` array alongside `.attrs`) is easy
to add later without breaking the current shape; it is left out of Phase 0 because
no example in the request's own `find_all`/`attr` walking style needs it.

### 2.6 Where file, line, and column actually surface

"Errors carry file, line and column, and surface through the normal directive
error path" does not mean a new diagnostic channel. The **normal directive error
path** for a host function today is: the Rust function returns
`Err(Box<EvalAltResult>)`, `error_message` unwraps it to a plain string
(`lib.rs:703`), and the CLI's resolver returns that string as the directive's
`Err`, which `Assembler::exec_custom` reports via `self.hard_error(msg)` — one
diagnostic, on the **directive's own line** in the `.asm` file, whose *message
text* names whatever the host function wants to say.

A `parse_xml_file` error is therefore not a second squiggle at the failing line
*inside* the XML file (there is no LSP surface for a file the editor never opens
for this purpose, and none is added here) — it is the directive's diagnostic,
worded like `` parse_xml_file: map.png:14:9: unknown entity '&nope;' `` — naming
exactly what a script author needs to keep debugging productive: which file,
and where in it, without opening some generated intermediate that no longer
exists. This is exactly the shape `decode_png_file`'s own errors already take
(`decode_png_file: cannot read {path}: {io error}`), so it costs no new
convention to learn.

## 3. Cache correctness is inherited, not rebuilt

The request's §2 states the invariant as new work: "any host function that opens a
file registers it." It already **is** the invariant — plan 011 built exactly this
mechanism, and it needs no extension, only correct use.

`engine_recording` (`crates/nessemble-script/src/lib.rs:172`) shadows rhai-fs's own
`path` conversion function and additionally registers `read_blob` and
`decode_png_file` — the three routes, today, from a path string to a file. Every
one of them calls `record(rec.as_ref(), resolve(&base, p))` before touching the
filesystem, which — when a recorder is present (`run_with_inputs`, not the plain
`run`) — inserts the resolved, absolute path into an `Rc<RefCell<BTreeSet<PathBuf>>>`.
`RunOutcome::inputs` is that set, and `nessemble-cli`'s `Resolver::resolve`
(`custom.rs:81-84`) stamps every one of them into the on-disk cache entry
(`cache::Cache::put`) with no awareness of what kind of file it was.

So: `parse_xml_file` and `parse_json_file` become tracked cache dependencies by
**calling the same `record`/`resolve` pair `read_blob` already calls**, registered
alongside it in `engine_recording`. No change to `RunOutcome`, `Cache`, `Key`, or
`Stamp` (`nessemble-cli/src/cache.rs`) is needed, and none is made. A document that
`parse_xml_file`s a shared definitions file, which itself gets `parse_xml_file`d
from *inside the script* for its own nested reference, is two recorded paths
because it is two calls through the choke point — the recorder does not care that
one call happened lexically inside a loop over the other's `.find_all` results.

The one thing this section adds to the *codebase*, rather than to the mechanism,
is a line in the module doc comment at `lib.rs:167` ("The three registrations
below are the *only* routes...") once there are five, not three — so the next
person adding a path-taking host function finds the invariant stated where they
are about to violate it, per plan 011 §15's own risk table ("A new host function
that takes a path must record it; the recorder lives beside the registrations so
the omission is visible in review").

## 4. String and blob helpers

Checked against `rhai` 1.25.1's own stdlib (`packages/string_basic.rs`,
`string_more.rs`, `math_basic.rs`, `blob_basic.rs`) before adding anything, since
duplicating an existing function under the same name is a silent shadowing bug
waiting to happen:

| Request | Status | Action |
| --- | --- | --- |
| `to_char(int)` | **Missing.** `char.to_int()` exists (`char_functions`); nothing goes the other way. | Add `to_char(value) -> String`, a one-character string (not a bare Rhai `char`, so `s += to_char(b)` composes the way the request's own motivating example wants). Errors on a value outside the Unicode scalar range. |
| `trim()` returns `()` | **Confirmed.** `string_more.rs`'s `trim` mutates in place and returns nothing — exactly the `let t = s.trim();` trap the request describes. | Add `trimmed()`, non-mutating, returning the trimmed copy. `trim()` itself is untouched — scripts that already use it for its mutating behavior keep working. |
| `format_hex(value, width)` | **Missing** in this shape. `to_hex(value)` exists but has no width/padding and no `$` prefix — assembly's own hex literal spelling. | Add `format_hex(value, width) -> String`, e.g. `format_hex(255, 2)` → `"$FF"`. |
| `parse_int(str, radix)` | **Already exists.** `math_basic.rs`'s `parse_int_radix(string, radix)` is registered under the same overloaded name `"parse_int"` Rhai's one-argument form uses. | **No code change.** Documented in the crate doc comment and `docs/src/extending.md` so the request's own gap report (evidently checked against an older Rhai release) doesn't get rediscovered. |
| `blob.to_string()` / `blob.sub_string(...)` | **Already covered.** `blob_basic.rs` has `Blob::as_string() -> String` (lossless UTF-8 decode); a script that wants a substring of that calls the stock `sub_string` **on the resulting string** — no separate blob method needed. | **No code change**, same treatment as `parse_int`. |

Net new host functions: `to_char`, `trimmed`, `format_hex` — three, not five, once
what upstream Rhai already ships is subtracted. All three are free functions
registered in `engine_recording` alongside the existing `quantize`/`nes_shade`
helpers (no file access, so no `resolve`/`record` involvement).

## 5. Bulk numeric decoding

```rhai
let values = parse_int_list(text, ",");        // -> array of ints, radix 10
let values = parse_int_list(text, ",", 16);    // optional radix, 2..=36 (matches parse_int's own bound)
```

One native call: split `text` on the literal delimiter string, trim whitespace
from each field, skip empty fields (a trailing delimiter or repeated delimiters
produce no phantom zero), parse each remaining field with
`i64::from_str_radix`. A field that fails to parse is a thrown error naming the
field's index and text, not a silent zero — matching `nes_shade`/`quantize`'s
existing convention of erroring on a bad element rather than coercing it
(`quantize_arr`, `lib.rs:633`).

No prefix stripping (`$…`, `0x…`) is done automatically: `radix` says how to read
every field, and guessing per-field from a prefix would silently misinterpret a
column that mixes decimal and hex by convention rather than by marker. A caller
that needs prefix-aware parsing already has `parse_int(field, radix)` per field.

## 6. Deferred: source-returning directives

The request's §3 — letting `custom()` return a string of **assembly source** that
the assembler expands macro-expansion-style, rather than only bytes — is real and
valuable for a documented data table, but it is a different kind of change from
§§2–5: it touches the directive dispatch path in `nessemble-core`
(`Assembler::exec_custom`, `assemble.rs:1346`), the listing file, the linter, and
coverage — all consumers of *emitted bytes* today that would need to understand
*emitted source* instead. `dynamic_to_bytes` (`lib.rs:670`) already special-cases a
returned string ("a returned string emits its bytes, like the reference Lua
host") — precisely the return shape this feature needs to distinguish itself
from, meaning the distinguishing marker has to be something other than "the
return value is a string" (an explicit wrapper value, e.g. `emit_source(text)`
returning a tagged handle, as the request itself sketches).

**Not attempted in Phase 0.** It is real work, independent of whether XML/JSON
parsing exists, and bundling it into this plan would make an already-large change
larger without buying the acceptance criteria in §10, which are stated entirely in
terms of bytes. Left for a phase of its own, sketched only far enough to confirm it
does not conflict with anything Phase 0 ships: `emit_source` would be one more
function registered in `engine_recording`, and `dynamic_to_bytes` would grow one
more `Dynamic` shape to recognize before the plain-string case — additive, not a
rework of the parsing added here.

## 7. Deferred: parallel script execution

The request's §6 is the largest available win on script-heavy builds (a build
reported as 100% of one CPU core, wall time equal to user+sys time), and it needs
no script changes at all — every invocation is independent by construction. It is
deferred here for a structural reason, not a difficulty one: `Assembler::run_pass`
(`assemble.rs:771`) is a single sequential loop over `&mut self`, and a custom
directive's *arguments* — `ints`, evaluated via `self.eval(e)` — can depend on a
forward-referenced symbol that is only fully known once a pass completes
(plan 011 §3, "`ints` can differ between passes"). True across-directive
parallelism therefore cannot simply wrap the existing loop; it needs either a
scan-ahead prepass restricted to arguments that are safe to know early, or a
"parallel prewarm" step that runs every distinct invocation concurrently to
populate `custom_memo`/the disk cache *before* the sequential emission passes read
from it. The workspace has no data-parallelism dependency today (no `rayon`,
confirmed absent from `Cargo.lock`); adding one is exactly the kind of choice that
wants its own design pass rather than a subsection of this one, particularly since
cache reads/writes (`nessemble-cli/src/cache.rs`) are not currently safe under
concurrent access and would need it before anything else runs in parallel.

**Not attempted in Phase 0.** Structured-data parsing is valuable stand-alone and
does not block on it, and vice versa — a future parallel-prewarm phase benefits
identically whether the invocations it parallelizes call `decode_png_file` or
`parse_xml_file`.

## 8. Deferred: operation limits and timing

Two smaller, independent asks from the request, both left out of Phase 0 for the
same reason: they are CLI/engine-configuration surface, not parsing, and bundling
unrelated surface area into the first PR of a large feature makes it harder to
review, not easier.

- **§7, operation limits.** `engine_recording` hardcodes
  `set_max_operations(10_000_000)` (`lib.rs:174`) today; it is already documented
  in the surrounding comment block, just not in `docs/src/extending.md` and not
  settable. Making it settable means a new `Options`-shaped parameter threaded
  through `run`/`run_with_inputs`/the `CustomResolver` chain (mirroring how
  `Options::project_root` was added in plan 012) and a CLI flag. Small, but its own
  change.
- **§8, `--time-scripts`.** Per-directive wall time, invocation count, and
  cache hit/miss, aggregated and reported. The natural home is
  `nessemble-cli/src/custom.rs`'s `Resolver::resolve`, which already sees a script
  identity, its cache-hit/miss outcome, and now runs Rust-side host parsing that
  is worth being able to measure separately from Rhai's own execution time — a
  motivating example for the feature is this plan's own §2.1 table, which the
  request's author had to reconstruct by hand-timing a shell wrapper.

Both are natural **Phase 4** work (§8 below) once §§2–5 give scripts something
substantial to time and something worth bounding the operation count of.

## 9. Non-goals

Unchanged from the request, restated because they bound what "done" means for
Phase 0:

- **Writing structured text.** Reading is the asymmetric win (§2.1); round-tripping
  means matching some external tool's exact formatting, which is the downstream
  project's problem.
- **A general-purpose XML/JSON feature set.** Elements, attributes, text,
  entities. No namespaces, no XPath, no schema validation, no DTDs (rejected
  outright, §2.2).
- **Anything that lets a script reach a file it did not resolve through the host.**
  Cache correctness depends on that being exhaustive (§3); this plan adds callers
  of the existing choke point, not a new one.
- **Project-root (`@/`) resolution inside the script-file API.** See
  [§11.1](#111-why-parse_xml_file-does-not-resolve-) — a deliberate,
  documented departure from the request's own text.

## 10. Acceptance

Unchanged from the request:

- A script using `parse_xml_file` reproduces, byte for byte, a data table an
  equivalent compiled implementation produces from the same document.
- The script's own share of a document-driven conversion sits near the
  orchestration figure in §2.1's table (~0.017s-class), not the tokenizing one
  (~0.9s-class) — if a full conversion lands near the in-script-parser cost, work
  that belongs in §2/§5 is still happening in Rhai.
- Editing a source document invalidates exactly the cache entries that read it,
  including entries that reached it indirectly (nested `parse_xml_file` calls,
  §3) — and no others.
- Existing scripts and directives are unchanged and still cache. (No existing
  registration, key shape, or public function signature changes — see §11.)

## 11. Decisions

Author's calls, recorded so they are decisions rather than defaults — no
maintainer round-trip happened before Phase 0 shipped, so these are exactly the
forks a review should scrutinize first.

### 11.1 Why `parse_xml_file` does not resolve `@/`

The request asks new parsers to "honour the same path resolution the built-in
media importers use, including the project-root form." Plan 012 §5 settled the
opposite for exactly this layer, under "Explicitly out of scope": *"The
script-side file API (nessemble-script's resolve). A path a script constructs
itself and passes to read_file still resolves against base. Teaching that layer
about the root means threading the root through the CustomResolver signature — a
public API break for a case no one has asked for."*

Someone has now asked for it — but honouring it only for `parse_xml_file`/
`parse_json_file` while `read_blob`/`decode_png_file` stay base-dir-only would be
a worse inconsistency than not having it at all: two path-taking script functions
resolving `@/lib/x.xml` two different ways depending on which one is called is the
exact "positional surprise" plan 012 was written to eliminate. Doing it correctly
means widening `CustomResolver` (`nessemble-core/src/assemble.rs:292`) to carry
the project root — touching `Assembler::exec_custom`, the LSP's
`lenient_custom_resolver`, and every embedder that constructs a `CustomResolver`
by hand — for all five path-taking script functions at once, not a two-function
special case.

**Decision: out of scope for Phase 0, same boundary plan 012 drew.** All four
path-taking script functions (`read_blob`, `decode_png_file`, and the two new
ones) resolve identically — base-dir-relative or absolute, no `@/` — so nothing
about this phase makes any *existing* function behave inconsistently with any
*other* one. Widening `CustomResolver` to thread the root through is real,
additive, non-breaking work (new `_with_root`-suffixed entry points alongside
`run`/`run_with_inputs`, exactly as `RunOutcome`/`run_with_inputs` were added
alongside `run` in plan 011 without touching it) — it is left for a phase with its
own review, rather than folded into this one under a deadline this plan does not
have.

### 11.2 `.attrs` ships sorted rather than insertion-ordered

Covered in full in §2.5. Restated here because it is the one place this plan's
Phase 0 knowingly ships something narrower than the request's own table says.
*Alternative considered:* a bespoke ordered-map Rhai type. Rejected for Phase 0 —
it is a new type in the engine for a property (`.attrs` iteration order) no
example in the request's own script snippets actually reads; `.attr(name)` is
unaffected and is what every snippet uses.

### 11.3 JSON gets no opaque handle; XML does

Covered in §2.4. The asymmetry is deliberate, not an oversight: XML's `.find`/
`.attr` API is worth deferring pixel-array-style; JSON has no equivalent
per-node API to defer, so eager conversion to native Rhai values is both simpler
and exactly what the request's own table (§2.3 here) describes.

### 11.4 No new workspace dependency for XML; `serde_json` reused for JSON

XML: hand-rolled, dependency-free, in `crates/nessemble-script/src/xml.rs` — no
existing crate in the workspace parses XML, and plan 011 §16.4's own reasoning
("no new dependency is needed... the workspace has no hashing crate") is the same
shape of call here: a bounded, spec-narrow parser (§2.2's exclusions make the
grammar small) is worth writing rather than pulling in a general-purpose XML crate
whose namespace/DTD/schema machinery this feature explicitly does not want.
JSON: `serde_json` is **already** a workspace dependency (`nessemble-cli`,
`nessemble-core`'s dev-dependencies) — adding it to `nessemble-script` is a
`Cargo.toml` line, not a new dependency the workspace has to start building.

## 12. Phased plan

### Phase 0 — XML and JSON parsing, bulk numeric decoding, string/blob gaps. **Shipped.**

- `crates/nessemble-script/src/xml.rs`: the tokenizer/parser, `XmlNode`/`XmlDoc`
  types, `parse_xml`/`parse_xml_file` registration (§2.2).
- `crates/nessemble-script/src/json.rs`: `serde_json::Value` → `Dynamic`
  conversion, `parse_json`/`parse_json_file` registration (§2.3).
- Both new file-opening functions call the existing `resolve`/`record` pair
  (§3) — no change to `nessemble-cli`'s cache at all.
- `parse_int_list` (§5), `to_char`/`trimmed`/`format_hex` (§4), registered
  alongside the existing helpers in `engine_recording`.
- `nessemble-script/Cargo.toml` gains `serde_json.workspace = true`.
- Tests: parser unit tests in `xml.rs` (entities, `CDATA`, comments, `DOCTYPE`
  rejection, line/column on error), `json.rs` (value-shape conversion, error
  position), and the existing `lib.rs` test module gains cases mirroring
  `records_every_route_from_a_path_to_a_file` for the two new functions, proving
  they are recorded exactly like `read_blob`.
- `docs/src/extending.md`: a "Parsing structured data" section, and the
  "Caching" section's file list grows the two new functions.
- Changeset: `minor` (new script-facing functionality, no breaking change).

### Phase 1 — `@/` for the script-file API (deferred, §11.1)

Widen `CustomResolver` and `nessemble-script`'s public `run`/`run_with_inputs` to
carry an optional project root, in the additive shape plan 011 used when it added
`run_with_inputs` alongside `run`. Every one of `read_blob`, `decode_png_file`,
`parse_xml_file`, `parse_json_file` gains `@/` support in the same change, so no
function is left inconsistent with another.

### Phase 2 — source-returning directives (deferred, §6)

`emit_source(text)`, the distinguishing return shape, `nessemble-core` expansion
of the returned source at the directive's call site, and listing/linter/coverage
visibility into the expanded lines.

### Phase 3 — parallel script execution (deferred, §7)

A concurrency-safe cache (`nessemble-cli/src/cache.rs`'s `get`/`put`), and a
prewarm step that runs independent invocations concurrently ahead of the
sequential emission passes.

### Phase 4 — operation limits and `--time-scripts` (deferred, §8)

A settable `max_operations`, documented; per-directive timing aggregated and
reported via a new CLI flag.

## 13. As built

### 13.1 Phase 0

- **`parse_int(str, radix)` and `blob.as_string()` were confirmed present** in
  `rhai` 1.25.1 (`math_basic.rs`'s `parse_int_radix`, `blob_basic.rs`'s
  `as_string`) before writing any host code for them — reading the vendored
  source directly rather than trusting the request's own gap report, which was
  evidently checked against an older release. Net new host functions came to
  three (`to_char`, `trimmed`, `format_hex`), not five, exactly as §4 predicts.
- **`XmlNode`/`XmlNodeData` needed `#[derive(Debug)]`** beyond what the design
  sketch implied — `Result::unwrap_err()` in the parser's own unit tests
  requires the `Ok` type to implement `Debug`, harmless since nothing in the
  Rhai-facing API depends on it.
- **`starts_with_ci` (the `<!DOCTYPE`/`<!doctype` check) uses `str::get`, not
  slicing**, to avoid a panic when the byte offset does not land on a UTF-8
  character boundary — caught before it shipped, not found by a fuzzer.
- **The mismatched-closing-tag error position points at the offending `</tag>`,
  not at the parent's own opening tag** — confirmed by test
  (`parse_xml_file_error_names_the_file_and_position`) against the parser's
  actual column tracking rather than hand-computed arithmetic, which was wrong
  by one column on the first attempt (an off-by-one in reasoning about when the
  cursor's column advances relative to the character it currently points at,
  not a bug in the parser itself).
- **`format_hex` masks `value` to `width` hex digits** rather than only
  zero-padding a shorter value — not specified in the request, but the natural
  reading of "assembly's own `$XX`/`$XXXX` spelling": a negative or oversized
  input wraps to a fixed-width dump the way `format_hex(-1, 2)` → `"$FF"`
  suggests it should, rather than printing 16 hex digits for a value that does
  not fit.
- **`.text` keeps whitespace-only indentation between child elements** rather
  than trying to distinguish "significant" from "structural" whitespace the way
  a DOM parser's whitespace-stripping mode might — stated as a design choice in
  §2.2, confirmed here as shipped exactly that way
  (`whitespace_only_indentation_text_is_still_reported`), with `.trimmed()`
  (§4) as the escape hatch for a script that wants only meaningful text.
- Every acceptance criterion in §10 that is testable without Phase 3/4 has a
  regression test: byte-for-byte parity with a compiled conversion
  (`parse_xml_file_reproduces_a_compiled_conversion_byte_for_byte`), and cache
  dependency tracking through both a direct call and a nested one
  (`records_every_route_from_a_path_to_a_file`,
  `a_nested_parse_xml_file_call_is_recorded_too`).
