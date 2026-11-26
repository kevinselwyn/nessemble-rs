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
trailing-tokens = Unexpected `{ $token }` after the end of the statement
label-not-alone = Label `{ $name }:` must be on a line of its own; move what follows it to the next line
duplicate-symbol = `{ $name }` is already defined (line { $line } of `{ $file }`); this definition replaces it
value-out-of-range = Value { $value } does not fit in { $field } ({ $min } to { $max }); only the low { $bits } bits are kept
macro-arg-missing = Macro `{ $name }` uses `\{ $needed }`, but this call passes { $given }
macro-arg-extra = Macro `{ $name }` was passed more arguments ({ $given }) than its body uses ({ $used })
nes2-required = { $what } requires NES 2.0 mode (`.ines2 1`)
nes2-range = { $field } value { $value } is out of range ({ $min }-{ $max })
nes2-ram-size = Invalid { $field } size { $value } (must be 0 or a power-of-two byte count from 128 to 2097152)
nes2-console-conflict = Conflicting console type: VS, PlayChoice-10, and .inesconsole are mutually exclusive
nes2-extended-console = Extended console type (.inesconsole 3) is not yet supported
nes2-vs-ignored = VS PPU/hardware type set but console type is not VS; value ignored

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

## Diagnostic framing (CLI)

error-line = Error in `{ $file }` on line { $line }: { $message }
warning-line = Warning in `{ $file }` on line { $line }: { $message }
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
