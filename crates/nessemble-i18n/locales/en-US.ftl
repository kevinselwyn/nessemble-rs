# English (en-US) message catalog for nessemble-rs.
#
# Every user-facing string flows through these messages (the equivalent of the
# reference tool's gettext `_()` call sites). Message ids are stable API; the
# values here reproduce the reference's English wording byte-for-byte. Variables
# use `{ $name }`; a trailing space is written as `{ " " }` so Fluent does not
# trim it.

## Assembler diagnostics

symbol-not-defined = Symbol `{ $name }` was not defined
unknown-opcode = Unknown opcode `{ $mnemonic }`
invalid-mode = Invalid addressing mode
unknown-register = Unknown register `{ $reg }`
branch-out-of-range = Branch address out of range
address-too-high = Address too high
prg-start-8000 = Start address for PRG bank { $bank } is 0x8000
prg-start-c000 = Start address for PRG bank { $bank } is 0xC000
overflow-prg = Overflowing PRG Bank { $bank }
overflow-chr = Overflowing CHR Bank { $bank }
checksum-preceding = Checksums may only be performed on preceding data
fill-args = Not enough .fill arguments
font-args = Not enough .font arguments
defchr-args = Too few arguments. { $count } provided, need 8
value-too-high = Value too high
nes2-required = { $what } requires NES 2.0 mode (`.ines2 1`)
nes2-range = { $field } value { $value } is out of range ({ $min }-{ $max })
nes2-ram-size = Invalid { $field } size { $value } (must be 0 or a power-of-two byte count from 128 to 2097152)
nes2-console-conflict = Conflicting console type: VS, PlayChoice-10, and .inesconsole are mutually exclusive
nes2-extended-console = Extended console type (.inesconsole 3) is not yet supported
nes2-vs-ignored = VS PPU/hardware type set but console type is not VS; value ignored


## Diagnostics for source that cannot mean what it says (always errors)

label-content = a label must be on a line of its own; move `{ $rest }` to the next line
unexpected-after = unexpected `{ $token }` after `{ $statement }`
unexpected-after-list = unexpected `{ $token }` after `{ $statement }`; separate values with commas
unexpected-after-instruction = unexpected `{ $token }` after `{ $statement }`; indent an instruction, or it is read as a constant definition
macro-arity-none = macro `{ $name }` takes no arguments, but this call passes { $passed }
macro-arity-one = macro `{ $name }` takes 1 argument (it uses \1), but this call passes { $passed }
macro-arity = macro `{ $name }` takes { $arity } arguments (it uses \1-\{ $arity }), but this call passes { $passed }
note-macro-defined = `{ $name }` is defined at { $file } line { $line }
note-in-expansion = in expansion of macro `{ $name }` at { $file } line { $line }

## Configurable assembler diagnostics (rules)

duplicate-symbol-label = `{ $name }` is already defined at { $file } line { $line } (as a label at { $value }); this definition replaces it
duplicate-symbol-rs = `{ $name }` is already defined at { $file } line { $line } (as a `.rs` variable at { $value }); this definition replaces it
duplicate-symbol-constant = `{ $name }` is already defined at { $file } line { $line } (as a constant, { $value }); this definition replaces it
macro-redefined = macro `{ $name }` is already defined at { $file } line { $line }; this definition replaces it
value-out-of-range = value { $value } ({ $hex }) does not fit in { $field }; emitting { $emitted }
value-out-of-range-negative = value { $value } does not fit in { $field }; emitting { $emitted }
field-byte = a byte
field-word = a word
field-zero-page = a zero-page operand
field-absolute = an absolute operand

## Media importers

could-not-load-png = Could not load PNG
could-not-read = Could not read `{ $file }`
could-not-open = Could not open `{ $file }`
not-a-wav = `{ $file }` is not a WAV
wav-not-mono = WAV is not mono

## Macros and includes

macro-not-defined = Macro `{ $name }` was not defined
too-many-includes = Too many nested includes
could-not-include = Could not include `{ $file }`
project-root-unresolved = `{ $file }` uses `@/`, but no project root could be determined
project-root-escape = `{ $file }` resolves outside the project root
full-path-of = Could not get full path of { $target }
full-path = Could not get full path
macro-name-after-macro = Expected macro name after .macro
macro-name-after-macrodef = Expected macro name after .macrodef
macro-unterminated = Unterminated macro definition
unsupported-directive = Unsupported directive `.{ $name }` (not yet implemented)

## Custom pseudo-ops (scripting)

unknown-custom = Unknown custom pseudo-instruction `{ $pseudo }`
custom-not-exist = Command for custom pseudo-instruction `{ $pseudo }` does not exist
emit-source-parse-error = `.{ $pseudo }` emitted source that failed to parse (line { $line }): { $message }
emit-source-unsupported-directive = `.{ $pseudo }` emitted `{ $directive }`, which is not supported in a directive's own emitted source
emit-source-too-deep = `.{ $pseudo }` emitted source too many levels deep (a script emitting source that emits source, recursively?)

## Loops (`.for` / `.endfor`)

for-unterminated = `.for` has no matching `.endfor`
endfor-unmatched = `.endfor` has no open `.for`
for-bound-count = `.for` takes two bounds (`.for START, END`), but { $count } were given
for-bound-unknown = `.for` bound depends on `{ $name }`, which is not known on the first pass; use constants and labels defined above the loop
for-too-many = `.for` would run { $count } iterations; the limit is { $limit } per loop
for-nest-too-many = Nested `.for` loops would run { $count } iterations in total; the limit is { $limit }
loop-index-outside = `{ $counter }` can only be used inside a `.for` loop
loop-index-too-deep = `{ $counter }` needs { $needed } enclosing `.for` loops, but only { $open } are open
label-in-for = Label `{ $name }` is defined inside a `.for` body, so it would be defined once per iteration; write `{ $name }\i:` for one label per iteration
for-paste-multiple = `{ $name }` pastes more than one loop counter; a name can end in only one (`\i`, `\j` or `\k`)
for-paste-negative = Cannot paste the negative counter value { $value } onto `{ $name }`
for-paste-defined = Pasted name `{ $name }` is already defined
macrodef-in-for = `.macrodef` is not allowed inside a `.for` body
for-if-unbalanced = A conditional opened in a `.for` body must close in the same body (an `.if` and its `.endif` straddle the `.endfor`)
for-iteration-suffix = { $message } (in `.for` iteration { $counters })

## Enums (`.enum` / `.endenum`)

enum-member-twice = Enum member `{ $name }` is defined twice in the same `.enum`
enum-member-invalid = Expected an `.enum` member name (two or more letters, digits or `_`, not starting with a digit)
enum-name-twice = Enum `{ $name }` is already defined on line { $line } of `{ $file }`
enum-name-invalid = The `.enum` name must be a name, as in `.enum $00, 1, OP`
enum-nested-named = An `.enum` cannot be opened inside another `.enum` when either is named; close the `.enum` on line { $line } with `.endenum` first
enum-unterminated = `.enum { $name }` has no matching `.endenum`
enum-member-indented = Unknown opcode `{ $mnemonic }` (an `.enum` member must start in the first column)

## Tables (`.table` / `.endtable`)

table-tag-index = Entry tagged `{ $key }` ({ $value }) is at index { $index }
table-tag-index-literal = Entry tagged { $value } is at index { $index }
table-missing-member = Table `{ $set }` is missing `{ $member }` ({ $value })
table-count-enum = Table has { $count } entries; `{ $set }` has { $expected }
table-count = Table has { $count } entries; `.table` expects { $expected }
table-entry-size = Entry at index { $index } is { $size } bytes; entries in this table are { $expected }
table-entry-empty = Entry at index { $index } emits no bytes
table-untagged = Untagged data inside `.table`; start each entry with a `[KEY]` tag
table-not-contiguous = `{ $set }` is not a contiguous enum
table-first-not-member = `{ $set }` has no member with value { $value }
table-arg-unknown = `{ $name }` is neither a constant nor a named enum
table-arg-count = `.table` takes at most two arguments (`.table [COUNT | ENUM][, FIRST]`), but { $count } were given
table-count-invalid = `.table` expects at least 1 entry, not { $value }
table-empty = `.table` has no entries
table-unterminated = `.table` has no matching `.endtable`
endtable-unmatched = `.endtable` has no open `.table`
table-nested = `.table` inside another `.table` (opened on line { $line })
table-for-unbalanced = A `.table` and a `.for` body must not overlap; close the inner one first
table-if-unbalanced = A `.table` and an `.if` block must not overlap; close the inner one first
tag-outside-table = A `[KEY]` tag must be inside a `.table`
tag-before-definition = A `[KEY]` tag can't prefix a label or constant definition
tag-before-block = A `[KEY]` tag can't prefix `{ $directive }`
tag-twice = Only one `[KEY]` tag may start a line
tag-unclosed = Expected `]` to close the `[KEY]` tag
table-layout = `{ $directive }` can't be used inside `.table`; it moves the location counter

## Diagnostic framing (CLI)

error-line = Error in `{ $file }` on line { $line }: { $message }
warning-line = Warning in `{ $file }` on line { $line }: { $message }
note-line = {"  "}note: { $message }
no-errors = No errors

## Version

# The `-h`/`--help` usage text is generated by clap; only the version/license
# banner's copyright line is rendered from a locale string.
label-copyright = Copyright

## init

init-created = Created `{ $file }`
init-overwrite = `{ $file }` already exists. Overwrite? [Yn]{ " " }
init-prompt-filename = Filename:{ " " }
init-prompt-prg = PRG Banks:{ " " }
init-prompt-chr = CHR Banks:{ " " }
init-prompt-mapper = Mapper (0-255):{ " " }
init-prompt-mirroring = Mirroring (0-15):{ " " }
init-choose-banks = Choose a positive number of CHR banks
init-choose-mapper = Choose a mapper between 0-255
init-choose-mirroring = Choose a mirroring between 0-15

## scripts

scripts-installed = Installed scripts to { $path }
cache-info = Cache: { $path }
cache-entries = { $count } entries, { $bytes } bytes
cache-cleared = Cleared { $count } entries, { $bytes } bytes
script-timing-header = Script timing:
script-timing-row = { $name }: { $calls } calls, { $hits } hits, { $misses } misses, { $total } total

## Miscellaneous

no-home = Could not find home directory
reference-not-found = Could not find info for `{ $term }`
root-not-found = `--root { $dir }` does not exist or is not a directory
