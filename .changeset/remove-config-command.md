---
nessemble: minor
---
Remove the `config` subcommand. Its only purpose in the reference tool was
storing the package-registry endpoint, and that registry subsystem is out of
scope for this rewrite — nothing in the assembler, formatter, or language
server ever read a value it stored, so the command configured nothing. It was
carried over by mistake during the initial rewrite and is now gone from the
CLI, help text, and documentation.
