---
nessemble: minor
---

Custom pseudo-op scripts can now resolve `@/`-prefixed project-root paths themselves (`read_blob`, `decode_png_file`, `parse_xml_file`, `parse_json_file`, and `open_file`), matching every other path-taking argument.
