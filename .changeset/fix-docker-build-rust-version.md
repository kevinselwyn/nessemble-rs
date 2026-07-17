---
nessemble: none
---
Fix the container-image build: build on the latest stable Rust instead of the pinned 1.83, which cannot parse the edition-2024 manifests of lockfile dependencies (e.g. moxcms).
