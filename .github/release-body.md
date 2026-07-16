### Downloads

| Platform       | Artifact(s)                                |
|----------------|--------------------------------------------|
| macOS          | `nessemble_*.pkg`, `nessemble_*_macos.tar.gz` |
| Linux amd64    | `nessemble_*_amd64.deb`                    |
| Linux i386     | `nessemble_*_i386.deb`                     |
| Windows 32-bit | `nessemble_*_win32.exe`, `…_win32.msi`     |
| Windows 64-bit | `nessemble_*_win64.exe`, `…_win64.msi`     |
| WebAssembly    | `nessemble_*_wasm.tar.gz` (in-browser assembler) |

#### macOS: "Apple could not verify…" on the `.pkg`

The `.pkg` is not signed with an Apple Developer ID, so macOS
Gatekeeper blocks it after download. Either clear the quarantine flag
and install the `.pkg` normally:

```sh
xattr -d com.apple.quarantine nessemble_*.pkg
sudo installer -pkg nessemble_*.pkg -target /
```

…or skip the installer entirely and use the plain binary tarball:

```sh
tar -xzf nessemble_*_macos.tar.gz
xattr -d com.apple.quarantine nessemble        # clear quarantine
sudo mv nessemble /usr/local/bin/               # onto your PATH
```
