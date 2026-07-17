---
nessemble: none
---
Publish a binary-only container image (static musl `nessemble` on `scratch`) to GHCR from the release pipeline, so other projects can `COPY --from` the executable without building the workspace.
