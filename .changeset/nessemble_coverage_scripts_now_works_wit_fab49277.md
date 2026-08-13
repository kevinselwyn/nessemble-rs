---
nessemble: minor
---

`nessemble coverage --scripts` now works without `--cdl`, seeds every mapped script (including ones a build never reaches) so they appear in the report at 0% instead of being absent, dedupes a script mapped under two directive names, and the summary/JSON split ROM coverage from script coverage.
