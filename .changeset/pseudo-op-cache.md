---
nessemble: minor
---

Custom pseudo-instruction results are cached in `~/.nessemble/cache`: a script re-runs only when it, one of the files it read, or the directive's arguments change, so an unchanged rebuild skips the work entirely. Scripts that draw random values, write files, list directories, or `import` modules are never cached, and `--no-cache`, `nessemble cache info` and `nessemble cache clear` control it.
