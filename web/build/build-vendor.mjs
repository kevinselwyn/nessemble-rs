// Produces web/vendor/codemirror.js: a minified ESM bundle of the CodeMirror 6
// primitives listed in codemirror.entry.mjs. Run `npm run build` after changing
// the entry or bumping a CodeMirror dependency; the output is committed so the
// runtime and `xtask dist` never need a JS toolchain (they just copy the file,
// like the wasm glue).
import { build } from "esbuild";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const outfile = resolve(here, "..", "vendor", "codemirror.js");

await build({
  entryPoints: [resolve(here, "codemirror.entry.mjs")],
  bundle: true,
  format: "esm",
  minify: true,
  sourcemap: false,
  target: ["es2019"],
  legalComments: "none",
  outfile,
});

console.log("wrote", outfile);
