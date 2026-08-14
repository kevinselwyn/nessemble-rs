//! The **host API catalog**: every function, method, property, and type a
//! custom pseudo-op script can reach, as data.
//!
//! A script is written against an API that, until this crate, existed only as a
//! run of `register_fn` calls in `nessemble_script::engine` and a page of prose
//! in [`docs/src/extending.md`]. Nothing enumerated it, so nothing could index
//! it, complete it, or check it. [`SCRIPT_API`] is that enumeration, in the
//! manner of [`nessemble_isa::DIRECTIVES`] — one table, several readers:
//!
//! - the domain-grouped table of contents in the Extending docs,
//! - `nessemble reference script`,
//! - the language server's completion, hover, and signature help,
//! - and a drift test in `nessemble-script` that fails when a function is
//!   registered without an entry here (or an entry survives its function).
//!
//! This crate is **data only and dependency-free**, including of the scripting
//! host: the language server documents this API in builds with no Rhai in them
//! at all. See `plans/014-scripting-docs-and-tooling.md` §3.
//!
//! [`docs/src/extending.md`]: https://kevinselwyn.github.io/nessemble-rs/docs/extending.html
//! [`nessemble_isa::DIRECTIVES`]: ../nessemble_isa/constant.DIRECTIVES.html

/// Which group of the Extending page an entry belongs to.
///
/// The order of the variants is the order the docs and `nessemble reference
/// script` present the groups in, which is the order a script author meets them:
/// what a script *is*, then what it can read, then what it can do with what it
/// read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Domain {
    /// The `custom` entry point and how a script returns what it emits.
    EntryPoint,
    /// Reading files, and how a script's paths resolve.
    Files,
    /// Decoding PNGs and walking their pixels, tiles, and cells.
    Images,
    /// Snapping shade values to palette indices.
    Palette,
    /// XML and JSON documents the host parses on the script's behalf.
    StructuredData,
    /// Bulk numeric decoding and the string/blob helpers around it.
    Text,
    /// Random values, for procedural and randomized data.
    Random,
}

impl Domain {
    /// Every domain, in presentation order.
    pub const ALL: &'static [Domain] = &[
        Domain::EntryPoint,
        Domain::Files,
        Domain::Images,
        Domain::Palette,
        Domain::StructuredData,
        Domain::Text,
        Domain::Random,
    ];

    /// The group's heading, as the docs and `reference` print it.
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Domain::EntryPoint => "Entry point and output",
            Domain::Files => "Files and paths",
            Domain::Images => "Images (PNG)",
            Domain::Palette => "Palette",
            Domain::StructuredData => "Structured data",
            Domain::Text => "Numbers, strings, and blobs",
            Domain::Random => "Randomness",
        }
    }

    /// One line of orientation for the group, for a reader who has not read the
    /// page around it.
    #[must_use]
    pub fn blurb(self) -> &'static str {
        match self {
            Domain::EntryPoint => {
                "What every script defines, and the three ways it can answer with output."
            }
            Domain::Files => {
                "Reading assets from disk. Relative paths resolve against the source file's \
                 directory; `@/` resolves from the project root."
            }
            Domain::Images => {
                "Decoding a PNG once, then reading it by pixel, by tile, or by matching whole \
                 cells against a bank."
            }
            Domain::Palette => "Turning shade values into fixed-palette indices.",
            Domain::StructuredData => {
                "The host parses the document; the script walks it. Rhai is fast enough to \
                 orchestrate a parse and far too slow to be one."
            }
            Domain::Text => {
                "Decoding delimited numbers in one native call, and the small string and blob \
                 gaps Rhai's standard library leaves."
            }
            Domain::Random => {
                "Procedural noise and randomized tables. A script that draws random values is \
                 never cached, and never reproducible."
            }
        }
    }
}

/// What an entry *is*, which decides how it is written and how an editor offers
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ApiKind {
    /// A free function: `decode_png_file(path)`.
    Function,
    /// A method on a value: `img.tile(col, row, w, h)`.
    Method,
    /// A property read off a value: `img.width`.
    Property,
    /// An opaque handle type: `image`, `xml_node`.
    Type,
}

/// Who registers an entry — which decides what the drift test can check
/// ([`SCRIPT_API`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Origin {
    /// Registered by `nessemble_script::engine` itself. These are the entries
    /// the drift test holds against the engine's source.
    Host,
    /// Provided by a third-party Rhai package (`rhai-fs`, `rhai-rand`) that the
    /// engine installs wholesale. The engine's source names the package, not
    /// the functions, so these entries are curated by hand — the documented
    /// subset of what the package offers, not all of it.
    Package(&'static str),
    /// Defined by the *script*, not the host: the `custom` entry point the
    /// assembler calls.
    Script,
}

/// When an entry exists at all.
///
/// Not hypothetical: the WebAssembly build turns both `fs` and `rand` off (no
/// filesystem, no entropy source), and there a call to one of these functions
/// is a "function not found" error. That is documentation, so it is a field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Availability {
    /// Present in every build.
    Always,
    /// Present only when `nessemble-script` is built with this feature (on by
    /// default for native builds; off for `wasm32`).
    Feature(&'static str),
}

impl Availability {
    /// The feature this entry needs, or `None` when it is always present.
    #[must_use]
    pub fn feature(self) -> Option<&'static str> {
        match self {
            Availability::Always => None,
            Availability::Feature(f) => Some(f),
        }
    }

    /// A short note for a docs column or a completion item's detail, or `None`
    /// when there is nothing to say.
    #[must_use]
    pub fn note(self) -> Option<&'static str> {
        match self {
            Availability::Always => None,
            Availability::Feature("fs") => Some("needs `fs` (absent in the WebAssembly build)"),
            Availability::Feature("rand") => Some("needs `rand` (absent in the WebAssembly build)"),
            Availability::Feature(_) => Some("feature-gated"),
        }
    }
}

/// One callable thing a script can reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScriptApi {
    /// The name as the engine knows it — `decode_png_file`, `find_all`, `width`.
    /// This is the registration name, so a method's entry is `tile`, not
    /// `img.tile`; [`signature`](Self::signature) carries the receiver.
    pub name: &'static str,
    /// How it is written at a call site, receiver and all:
    /// `img.tile(col, row, w, h)`. Optional trailing arguments are bracketed:
    /// `parse_int_list(text, delim[, radix])`.
    pub signature: &'static str,
    /// One line, in the voice of `nessemble_isa::DIRECTIVES`: lowercase, no
    /// trailing period, says what it does rather than what it is.
    pub summary: &'static str,
    /// Which group of the Extending page documents it.
    pub domain: Domain,
    /// Function, method, property, or type.
    pub kind: ApiKind,
    /// Who registers it.
    pub origin: Origin,
    /// Which builds have it.
    pub availability: Availability,
    /// The heading anchor in `docs/src/extending.md` that explains it, without
    /// the leading `#`. Several entries share one anchor — the docs are
    /// organized by task, and the catalog indexes into them rather than
    /// replacing them.
    pub anchor: &'static str,
}

impl ScriptApi {
    /// The entry's documentation URL on the published book.
    ///
    /// ```
    /// # use nessemble_script_api::lookup;
    /// let e = lookup("nes_shade").next().unwrap();
    /// assert!(e.docs_url().ends_with("extending.html#palette-quantization"));
    /// ```
    #[must_use]
    pub fn docs_url(&self) -> String {
        format!("{DOCS_BASE_URL}extending.html#{}", self.anchor)
    }
}

/// Base URL of the published mdBook documentation, for [`ScriptApi::docs_url`].
/// Kept in step with `xtask`'s `DOCS_BASE_URL`.
pub const DOCS_BASE_URL: &str = "https://kevinselwyn.github.io/nessemble-rs/docs/";

/// Every entry, ordered by [`Domain`] and, within a domain, in the order the
/// docs introduce them.
///
/// A name can appear more than once when the API genuinely offers it more than
/// once: `read_blob` is both a method on an open file handle and a one-call
/// free function.
pub const SCRIPT_API: &[ScriptApi] = &[
    // ── Entry point and output ───────────────────────────────────────────────
    ScriptApi {
        name: "custom",
        signature: "fn custom(ints, texts)",
        summary: "the entry point every script defines: the directive's integer and string \
                  arguments, returning the bytes to emit",
        domain: Domain::EntryPoint,
        kind: ApiKind::Function,
        origin: Origin::Script,
        availability: Availability::Always,
        anchor: "writing-a-script",
    },
    ScriptApi {
        name: "emit_source",
        signature: "emit_source(text)",
        summary: "return `text` as assembly source for the assembler to expand at the call site, \
                  rather than as bytes",
        domain: Domain::EntryPoint,
        kind: ApiKind::Function,
        origin: Origin::Host,
        availability: Availability::Always,
        anchor: "emitting-assembly-source",
    },
    // ── Files and paths ──────────────────────────────────────────────────────
    ScriptApi {
        name: "open_file",
        signature: "open_file(path[, mode])",
        summary: "open a file — `\"r\"` to read, no mode to read and write, creating or \
                  truncating it",
        domain: Domain::Files,
        kind: ApiKind::Function,
        origin: Origin::Package("rhai-fs"),
        availability: Availability::Feature("fs"),
        anchor: "filesystem-access",
    },
    ScriptApi {
        name: "read_blob",
        signature: "file.read_blob([n])",
        summary: "read the whole file, or `n` bytes, as a blob",
        domain: Domain::Files,
        kind: ApiKind::Method,
        origin: Origin::Package("rhai-fs"),
        availability: Availability::Feature("fs"),
        anchor: "filesystem-access",
    },
    ScriptApi {
        name: "read_string",
        signature: "file.read_string([n])",
        summary: "read the whole file, or `n` bytes, as a string",
        domain: Domain::Files,
        kind: ApiKind::Method,
        origin: Origin::Package("rhai-fs"),
        availability: Availability::Feature("fs"),
        anchor: "filesystem-access",
    },
    ScriptApi {
        name: "write",
        signature: "file.write(data)",
        summary: "write a blob or string to the file, returning the byte count",
        domain: Domain::Files,
        kind: ApiKind::Method,
        origin: Origin::Package("rhai-fs"),
        availability: Availability::Feature("fs"),
        anchor: "filesystem-access",
    },
    ScriptApi {
        name: "seek",
        signature: "file.seek(pos)",
        summary: "move the file's read/write cursor",
        domain: Domain::Files,
        kind: ApiKind::Method,
        origin: Origin::Package("rhai-fs"),
        availability: Availability::Feature("fs"),
        anchor: "filesystem-access",
    },
    ScriptApi {
        name: "read_blob",
        signature: "read_blob(path)",
        summary: "read a whole file as a blob in one call",
        domain: Domain::Files,
        kind: ApiKind::Function,
        origin: Origin::Host,
        availability: Availability::Feature("fs"),
        anchor: "filesystem-access",
    },
    // ── Images (PNG) ─────────────────────────────────────────────────────────
    ScriptApi {
        name: "decode_png",
        signature: "decode_png(blob)",
        summary: "decode PNG bytes into an image handle",
        domain: Domain::Images,
        kind: ApiKind::Function,
        origin: Origin::Host,
        availability: Availability::Always,
        anchor: "decoding-pngs",
    },
    ScriptApi {
        name: "decode_png_file",
        signature: "decode_png_file(path)",
        summary: "read and decode a PNG in one call",
        domain: Domain::Images,
        kind: ApiKind::Function,
        origin: Origin::Host,
        availability: Availability::Feature("fs"),
        anchor: "decoding-pngs",
    },
    ScriptApi {
        name: "image",
        signature: "image",
        summary: "a decoded image; a shared handle, so passing it around copies nothing",
        domain: Domain::Images,
        kind: ApiKind::Type,
        origin: Origin::Host,
        availability: Availability::Always,
        anchor: "decoding-pngs",
    },
    ScriptApi {
        name: "width",
        signature: "img.width",
        summary: "the image width in pixels",
        domain: Domain::Images,
        kind: ApiKind::Property,
        origin: Origin::Host,
        availability: Availability::Always,
        anchor: "decoding-pngs",
    },
    ScriptApi {
        name: "height",
        signature: "img.height",
        summary: "the image height in pixels",
        domain: Domain::Images,
        kind: ApiKind::Property,
        origin: Origin::Host,
        availability: Availability::Always,
        anchor: "decoding-pngs",
    },
    ScriptApi {
        name: "pixels",
        signature: "img.pixels",
        summary: "every channel as a flat `R, G, B, A` array, row-major — built fresh on each \
                  read, so prefer the accessors",
        domain: Domain::Images,
        kind: ApiKind::Property,
        origin: Origin::Host,
        availability: Availability::Always,
        anchor: "decoding-pngs",
    },
    ScriptApi {
        name: "r",
        signature: "img.r(x, y)",
        summary: "the red channel of a pixel — its shade, for the grayscale images scripts use",
        domain: Domain::Images,
        kind: ApiKind::Method,
        origin: Origin::Host,
        availability: Availability::Always,
        anchor: "pixel-accessors",
    },
    ScriptApi {
        name: "pixel",
        signature: "img.pixel(x, y)",
        summary: "a whole pixel as `[r, g, b, a]`",
        domain: Domain::Images,
        kind: ApiKind::Method,
        origin: Origin::Host,
        availability: Availability::Always,
        anchor: "pixel-accessors",
    },
    ScriptApi {
        name: "tile",
        signature: "img.tile(col, row, w, h)",
        summary: "a `w`×`h` block's red channels, row-major, at grid position `(col, row)`",
        domain: Domain::Images,
        kind: ApiKind::Method,
        origin: Origin::Host,
        availability: Availability::Always,
        anchor: "pixel-accessors",
    },
    ScriptApi {
        name: "find_cell",
        signature: "bank.find_cell(src, col, row, w, h)",
        summary: "the index of the bank cell drawing the same thing as that cell of `src`, or \
                  `-1`",
        domain: Domain::Images,
        kind: ApiKind::Method,
        origin: Origin::Host,
        availability: Availability::Always,
        anchor: "cell-matching",
    },
    ScriptApi {
        name: "cell_equals",
        signature: "bank.cell_equals(index, src, col, row, w, h)",
        summary: "whether bank cell `index` draws that cell of `src`",
        domain: Domain::Images,
        kind: ApiKind::Method,
        origin: Origin::Host,
        availability: Availability::Always,
        anchor: "cell-matching",
    },
    ScriptApi {
        name: "nearest_cell",
        signature: "bank.nearest_cell(src, col, row, w, h)",
        summary: "the closest bank cell by summed shade difference — never `-1`",
        domain: Domain::Images,
        kind: ApiKind::Method,
        origin: Origin::Host,
        availability: Availability::Always,
        anchor: "cell-matching",
    },
    // ── Palette ──────────────────────────────────────────────────────────────
    ScriptApi {
        name: "quantize",
        signature: "quantize(value, thresholds)",
        summary: "snap a value — or a whole array of them — to a palette index by counting the \
                  ascending `thresholds` it reaches",
        domain: Domain::Palette,
        kind: ApiKind::Function,
        origin: Origin::Host,
        availability: Availability::Always,
        anchor: "palette-quantization",
    },
    ScriptApi {
        name: "nes_shade",
        signature: "nes_shade(value)",
        summary: "the NES four-shade case of `quantize` (thresholds `[43, 128, 213]`), returning \
                  `0`–`3`; also takes an array",
        domain: Domain::Palette,
        kind: ApiKind::Function,
        origin: Origin::Host,
        availability: Availability::Always,
        anchor: "palette-quantization",
    },
    // ── Structured data ──────────────────────────────────────────────────────
    ScriptApi {
        name: "parse_xml",
        signature: "parse_xml(source)",
        summary: "parse an XML document held in a string, returning its root element",
        domain: Domain::StructuredData,
        kind: ApiKind::Function,
        origin: Origin::Host,
        availability: Availability::Always,
        anchor: "xml",
    },
    ScriptApi {
        name: "parse_xml_file",
        signature: "parse_xml_file(path)",
        summary: "read and parse an XML document in one call",
        domain: Domain::StructuredData,
        kind: ApiKind::Function,
        origin: Origin::Host,
        availability: Availability::Feature("fs"),
        anchor: "xml",
    },
    ScriptApi {
        name: "xml_node",
        signature: "xml_node",
        summary: "a parsed XML element; a shared handle, like an image",
        domain: Domain::StructuredData,
        kind: ApiKind::Type,
        origin: Origin::Host,
        availability: Availability::Always,
        anchor: "xml",
    },
    ScriptApi {
        name: "name",
        signature: "node.name",
        summary: "the element's name, verbatim",
        domain: Domain::StructuredData,
        kind: ApiKind::Property,
        origin: Origin::Host,
        availability: Availability::Always,
        anchor: "xml",
    },
    ScriptApi {
        name: "attrs",
        signature: "node.attrs",
        summary: "every attribute as a name → value map, sorted by name rather than document \
                  order",
        domain: Domain::StructuredData,
        kind: ApiKind::Property,
        origin: Origin::Host,
        availability: Availability::Always,
        anchor: "xml",
    },
    ScriptApi {
        name: "attr",
        signature: "node.attr(name)",
        summary: "one attribute's value, or `()` when it is not set",
        domain: Domain::StructuredData,
        kind: ApiKind::Method,
        origin: Origin::Host,
        availability: Availability::Always,
        anchor: "xml",
    },
    ScriptApi {
        name: "children",
        signature: "node.children",
        summary: "the child elements, as an array — text is not a child",
        domain: Domain::StructuredData,
        kind: ApiKind::Property,
        origin: Origin::Host,
        availability: Availability::Always,
        anchor: "xml",
    },
    ScriptApi {
        name: "text",
        signature: "node.text",
        summary: "the element's own text with entities decoded, or `()` when it has none",
        domain: Domain::StructuredData,
        kind: ApiKind::Property,
        origin: Origin::Host,
        availability: Availability::Always,
        anchor: "xml",
    },
    ScriptApi {
        name: "find",
        signature: "node.find(name)",
        summary: "the first child element with that name, or `()`",
        domain: Domain::StructuredData,
        kind: ApiKind::Method,
        origin: Origin::Host,
        availability: Availability::Always,
        anchor: "xml",
    },
    ScriptApi {
        name: "find_all",
        signature: "node.find_all(name)",
        summary: "every child element with that name, as an array",
        domain: Domain::StructuredData,
        kind: ApiKind::Method,
        origin: Origin::Host,
        availability: Availability::Always,
        anchor: "xml",
    },
    ScriptApi {
        name: "parse_json",
        signature: "parse_json(source)",
        summary: "parse a JSON document held in a string into native maps, arrays, and scalars",
        domain: Domain::StructuredData,
        kind: ApiKind::Function,
        origin: Origin::Host,
        availability: Availability::Always,
        anchor: "json",
    },
    ScriptApi {
        name: "parse_json_file",
        signature: "parse_json_file(path)",
        summary: "read and parse a JSON document in one call",
        domain: Domain::StructuredData,
        kind: ApiKind::Function,
        origin: Origin::Host,
        availability: Availability::Feature("fs"),
        anchor: "json",
    },
    ScriptApi {
        name: "parse_csv",
        signature: "parse_csv(text[, options])",
        summary: "parse a CSV/TSV document held in a string, returning it as a table",
        domain: Domain::StructuredData,
        kind: ApiKind::Function,
        origin: Origin::Host,
        availability: Availability::Always,
        anchor: "csv",
    },
    ScriptApi {
        name: "parse_csv_file",
        signature: "parse_csv_file(path[, options])",
        summary: "read and parse a CSV/TSV document in one call",
        domain: Domain::StructuredData,
        kind: ApiKind::Function,
        origin: Origin::Host,
        availability: Availability::Feature("fs"),
        anchor: "csv",
    },
    ScriptApi {
        name: "csv_table",
        signature: "csv_table",
        summary: "a parsed CSV/TSV document; a shared handle, like an image",
        domain: Domain::StructuredData,
        kind: ApiKind::Type,
        origin: Origin::Host,
        availability: Availability::Always,
        anchor: "csv",
    },
    ScriptApi {
        name: "csv_row",
        signature: "csv_row",
        summary: "one data row of a csv_table, indexable by column name (row[\"x\"]) or \
                  position (row[0])",
        domain: Domain::StructuredData,
        kind: ApiKind::Type,
        origin: Origin::Host,
        availability: Availability::Always,
        anchor: "csv",
    },
    ScriptApi {
        name: "headers",
        signature: "table.headers()",
        summary: "the column names, in file order",
        domain: Domain::StructuredData,
        kind: ApiKind::Method,
        origin: Origin::Host,
        availability: Availability::Always,
        anchor: "csv",
    },
    ScriptApi {
        name: "rows",
        signature: "table.rows()",
        summary: "every data row, each indexable by column name or position",
        domain: Domain::StructuredData,
        kind: ApiKind::Method,
        origin: Origin::Host,
        availability: Availability::Always,
        anchor: "csv",
    },
    ScriptApi {
        name: "len",
        signature: "table.len()",
        summary: "the row count",
        domain: Domain::StructuredData,
        kind: ApiKind::Method,
        origin: Origin::Host,
        availability: Availability::Always,
        anchor: "csv",
    },
    // ── Numbers, strings, and blobs ──────────────────────────────────────────
    ScriptApi {
        name: "parse_int_list",
        signature: "parse_int_list(text, delim[, radix])",
        summary: "decode a whole delimited column of integers in one native call, skipping empty \
                  fields",
        domain: Domain::Text,
        kind: ApiKind::Function,
        origin: Origin::Host,
        availability: Availability::Always,
        anchor: "bulk-numeric-decoding",
    },
    ScriptApi {
        name: "to_char",
        signature: "to_char(value)",
        summary: "a one-character string for a Unicode scalar, for building strings out of bytes",
        domain: Domain::Text,
        kind: ApiKind::Function,
        origin: Origin::Host,
        availability: Availability::Always,
        anchor: "string-and-hex-helpers",
    },
    ScriptApi {
        name: "trimmed",
        signature: "s.trimmed()",
        summary: "a trimmed copy of a string — the non-mutating form of `trim()`, which returns \
                  `()`",
        domain: Domain::Text,
        kind: ApiKind::Method,
        origin: Origin::Host,
        availability: Availability::Always,
        anchor: "string-and-hex-helpers",
    },
    ScriptApi {
        name: "format_hex",
        signature: "format_hex(value, width)",
        summary: "assembly's own hex spelling: `$`-prefixed and zero-padded to `width`",
        domain: Domain::Text,
        kind: ApiKind::Function,
        origin: Origin::Host,
        availability: Availability::Always,
        anchor: "string-and-hex-helpers",
    },
    // ── Randomness ───────────────────────────────────────────────────────────
    ScriptApi {
        name: "rand",
        signature: "rand([min, max])",
        summary: "a random integer, or one in the inclusive range `min..=max`",
        domain: Domain::Random,
        kind: ApiKind::Function,
        origin: Origin::Package("rhai-rand"),
        availability: Availability::Feature("rand"),
        anchor: "random-numbers",
    },
    ScriptApi {
        name: "rand_float",
        signature: "rand_float()",
        summary: "a random float in `0.0..1.0`",
        domain: Domain::Random,
        kind: ApiKind::Function,
        origin: Origin::Package("rhai-rand"),
        availability: Availability::Feature("rand"),
        anchor: "random-numbers",
    },
    ScriptApi {
        name: "rand_bool",
        signature: "rand_bool([p])",
        summary: "a random `true`/`false`, or `true` with probability `p`",
        domain: Domain::Random,
        kind: ApiKind::Function,
        origin: Origin::Package("rhai-rand"),
        availability: Availability::Feature("rand"),
        anchor: "random-numbers",
    },
    ScriptApi {
        name: "shuffle",
        signature: "array.shuffle()",
        summary: "shuffle an array in place",
        domain: Domain::Random,
        kind: ApiKind::Method,
        origin: Origin::Package("rhai-rand"),
        availability: Availability::Feature("rand"),
        anchor: "random-numbers",
    },
    ScriptApi {
        name: "sample",
        signature: "array.sample([n])",
        summary: "one random element, or `n` of them",
        domain: Domain::Random,
        kind: ApiKind::Method,
        origin: Origin::Package("rhai-rand"),
        availability: Availability::Feature("rand"),
        anchor: "random-numbers",
    },
];

/// Every entry in `domain`, in catalog order.
pub fn in_domain(domain: Domain) -> impl Iterator<Item = &'static ScriptApi> {
    SCRIPT_API.iter().filter(move |e| e.domain == domain)
}

/// Every entry named `name`, in catalog order.
///
/// An iterator rather than an `Option` because a name can be both a free
/// function and a method — `read_blob` is.
pub fn lookup(name: &str) -> impl Iterator<Item = &'static ScriptApi> + '_ {
    SCRIPT_API.iter().filter(move |e| e.name == name)
}

/// Every entry whose name matches `name` case-insensitively, for a
/// user-typed lookup.
pub fn lookup_ignore_case(name: &str) -> impl Iterator<Item = &'static ScriptApi> + '_ {
    SCRIPT_API
        .iter()
        .filter(move |e| e.name.eq_ignore_ascii_case(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_domain_has_entries_and_they_are_grouped() {
        // Grouped, not merely present: the docs and `reference` walk the table
        // once per domain and would interleave groups if it were not sorted.
        let mut seen: Vec<Domain> = Vec::new();
        let mut last: Option<Domain> = None;
        for entry in SCRIPT_API {
            if last != Some(entry.domain) {
                assert!(
                    !seen.contains(&entry.domain),
                    "{:?} appears in two runs — the table must be grouped by domain",
                    entry.domain
                );
                seen.push(entry.domain);
                last = Some(entry.domain);
            }
        }
        for domain in Domain::ALL {
            assert!(
                in_domain(*domain).next().is_some(),
                "{domain:?} has no entries"
            );
        }
        assert_eq!(seen.len(), Domain::ALL.len(), "a domain is out of order");
    }

    #[test]
    fn summaries_are_written_to_one_house_style() {
        for entry in SCRIPT_API {
            assert!(
                !entry.summary.ends_with('.'),
                "`{}`: summaries take no trailing period",
                entry.name
            );
            let first = entry.summary.chars().next().expect("non-empty summary");
            assert!(
                !first.is_uppercase(),
                "`{}`: summaries start lowercase, like `DIRECTIVES`",
                entry.name
            );
            assert!(!entry.anchor.is_empty(), "`{}`: no docs anchor", entry.name);
            assert!(
                !entry.anchor.starts_with('#'),
                "`{}`: anchors are stored without the leading `#`",
                entry.name
            );
        }
    }

    #[test]
    fn a_methods_signature_carries_a_receiver_and_its_name_does_not() {
        for entry in SCRIPT_API {
            match entry.kind {
                ApiKind::Method | ApiKind::Property => assert!(
                    entry.signature.contains('.'),
                    "`{}`: a method/property signature shows its receiver",
                    entry.name
                ),
                ApiKind::Type => assert_eq!(
                    entry.signature, entry.name,
                    "a type's signature is just its name"
                ),
                ApiKind::Function => {}
            }
            assert!(
                !entry.name.contains('.'),
                "`{}`: `name` is the registration name, without a receiver",
                entry.name
            );
        }
    }

    #[test]
    fn a_name_repeats_only_across_different_kinds() {
        for entry in SCRIPT_API {
            let same: Vec<&ScriptApi> = lookup(entry.name).collect();
            if same.len() > 1 {
                let kinds: Vec<ApiKind> = same.iter().map(|e| e.kind).collect();
                let mut unique = kinds.clone();
                unique.dedup();
                assert_eq!(
                    kinds.len(),
                    unique.len(),
                    "`{}` is listed twice with the same kind",
                    entry.name
                );
            }
        }
    }

    #[test]
    fn feature_gated_entries_name_a_feature_that_exists() {
        for entry in SCRIPT_API {
            if let Some(feature) = entry.availability.feature() {
                assert!(
                    matches!(feature, "fs" | "rand"),
                    "`{}`: unknown nessemble-script feature `{feature}`",
                    entry.name
                );
                assert!(entry.availability.note().is_some());
            }
        }
    }

    #[test]
    fn the_script_defined_entry_is_the_entry_point_and_nothing_else() {
        let script_defined: Vec<&str> = SCRIPT_API
            .iter()
            .filter(|e| e.origin == Origin::Script)
            .map(|e| e.name)
            .collect();
        assert_eq!(script_defined, ["custom"]);
    }

    #[test]
    fn docs_url_points_at_the_extending_page() {
        let entry = lookup("find_all").next().expect("find_all is catalogued");
        assert_eq!(
            entry.docs_url(),
            "https://kevinselwyn.github.io/nessemble-rs/docs/extending.html#xml"
        );
    }
}
