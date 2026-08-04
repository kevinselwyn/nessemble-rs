---
nessemble: minor
---

Filename arguments can now use a `@/` prefix to resolve from the project root instead of the containing file's directory — honoured by `.include`/`.inestrn`, the media importers, and any `file://`-declared custom pseudo-op argument. A declared `@/` argument reaches its script already resolved to an absolute path.
