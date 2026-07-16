# Editor bundle build

The `<nessemble-assembler>` web component uses [CodeMirror 6][cm] as its editing
surface. CodeMirror ships as ES modules on npm, so this directory bundles the
handful of primitives the component needs (see `codemirror.entry.mjs`) into a
single, minified ESM file: **`../vendor/codemirror.js`**.

That output is **committed** to the repository. The runtime and `xtask dist`
never run a JavaScript toolchain — they just copy `web/vendor/codemirror.js`
next to the component, the same way the wasm glue is copied. You only need the
steps below when changing which CodeMirror APIs are used or bumping a
CodeMirror version.

## Rebuilding

```sh
cd web/build
npm ci          # install the pinned CodeMirror + esbuild versions
npm run build   # regenerate ../vendor/codemirror.js
```

Commit the regenerated `web/vendor/codemirror.js` alongside any dependency
changes in `package.json` / `package-lock.json`.

[cm]: https://codemirror.net/
