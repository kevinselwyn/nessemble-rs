---
nessemble: minor
---
Script images are now handles, and can match cells natively. `decode_png` /
`decode_png_file` return an opaque image rather than a map of every pixel:
passing one to a helper function shares the decoded pixels instead of copying
them, so image work can be factored out of `custom()` without a silent 200×
slowdown, and `img.pixels` is built only if a script asks for it — decoding a
4096×2304 PNG drops from ~2.7s and ~650MB to under 100ms and memory proportional
to the image. `img.width`, `img.height`, `img.pixels`, `img.r()`, `img.pixel()`
and `img.tile()` are unchanged, so existing scripts need no edit.

Three new methods answer "which cell of this sheet is this?" — the scan every
image script hand-rolls today — against a bank image gridded into `w`×`h` cells:
`bank.find_cell(src, col, row, w, h)` (lowest matching index, or `-1`),
`bank.cell_equals(index, src, col, row, w, h)` (validate an index you already
have), and `bank.nearest_cell(src, col, row, w, h)` (closest by summed shade
distance). All three compare NES shade indices, agreeing exactly with
`nes_shade(bank.tile(…)) == nes_shade(src.tile(…))`.
