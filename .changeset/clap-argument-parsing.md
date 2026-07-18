---
nessemble: patch
---
Replace the hand-rolled CLI argument parser with [clap](https://docs.rs/clap).
The same flags, subcommands, and exit codes are accepted, but `--help`/usage
text is now generated from the argument definitions instead of being
hand-maintained. Two cosmetic differences follow from clap's conventions: the
help layout is clap's (still listing every in-scope option and command), and
argument errors are written to stderr rather than stdout. The `-v`/`--version`
and `-L`/`--license` banners are unchanged.
