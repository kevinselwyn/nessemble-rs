// Vendored-bundle entry point: re-exports exactly the CodeMirror 6 primitives
// the <nessemble-assembler> component uses. esbuild bundles this (and its
// transitive CodeMirror deps) into ../vendor/codemirror.js, a single ESM module
// the component imports at runtime. Keeping the surface explicit here means the
// component file stays free of a bundler while the integration logic (theme,
// tokenizer-driven highlighting) lives in the readable component source.
export { EditorState, RangeSetBuilder, StateEffect } from "@codemirror/state";
export {
  EditorView,
  Decoration,
  ViewPlugin,
  keymap,
} from "@codemirror/view";
export { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
export {
  search,
  searchKeymap,
  highlightSelectionMatches,
} from "@codemirror/search";
