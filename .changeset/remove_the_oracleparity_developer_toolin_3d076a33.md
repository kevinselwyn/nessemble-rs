---
nessemble: patch
---

Remove the oracle/parity developer tooling from xtask (the fetch-oracle, verify-goldens, and parity commands) and its supporting corpus-runner code. The repo has diverged from the original C implementation, so cross-checking output against the v1.1.1 reference binary is no longer useful. The hermetic golden-ROM tests in crates/nessemble-core/tests/corpus.rs remain the source of assembler-output regression coverage. Also drops the now-unused nessemble-core::REFERENCE_VERSION constant and the reference-version workspace metadata.
