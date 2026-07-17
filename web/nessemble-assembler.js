// The <nessemble-assembler> Web Component: an in-browser 6502/NES assembler.
//
// A vanilla custom element (no framework, no build step) that assembles its
// editable source with the WebAssembly build of nessemble and shows the ROM as
// a hex dump, with a download link and error/warning output. It also upgrades
// legacy `<div class="nessemble-assembler">` elements from the old docs.
//
// The editing surface is CodeMirror 6, loaded from a vendored, prebuilt ESM
// bundle (`vendor/codemirror.js`) that sits next to this script — so the browser
// gets a real editor (visible text selection, a working Cmd-F search panel)
// instead of the transparent-textarea overlay this component used to roll by
// hand. Syntax highlighting still comes from the assembler's own lexer: a
// CodeMirror view plugin decorates the document from `tokenize`, so the colors
// match the language server exactly and reuse the same `na-tok-*` palette as the
// static docs code blocks.
//
// On each run it dispatches a `nessemble:assembled` event whose `detail` carries
// `{ rom: Uint8Array, ok, errors, warnings }`, so an embedding page can react —
// e.g. play the ROM in an emulator.
//
// The WebAssembly glue (`nessemble.js`), the wasm binary (`nessemble_bg.wasm`),
// and the CodeMirror bundle (`vendor/codemirror.js`) are expected next to this
// script; their location is resolved from this script's own URL, so it works at
// any page depth.
(function () {
  "use strict";

  // The directory holding this script (and the wasm glue / editor bundle),
  // captured now: `document.currentScript` is only valid during synchronous
  // execution.
  var ASSET_BASE = new URL(".", document.currentScript.src).href;

  // Load and initialize the wasm module once; every element shares it. Once
  // ready, `wasmMod` and `tokenClassNames` (the `tokenize` class-id → name
  // legend) are cached so highlighting can run synchronously on each keystroke.
  var wasmPromise = null;
  var wasmMod = null;
  var tokenClassNames = null;
  function loadWasm() {
    if (!wasmPromise) {
      wasmPromise = import(ASSET_BASE + "nessemble.js").then(function (mod) {
        return mod.default(ASSET_BASE + "nessemble_bg.wasm").then(function () {
          wasmMod = mod;
          tokenClassNames = mod.token_classes();
          return mod;
        });
      });
    }
    return wasmPromise;
  }

  // Load the vendored CodeMirror 6 bundle once; every element shares it. The
  // bundle re-exports just the primitives this component uses (see
  // web/build/codemirror.entry.mjs).
  var cmPromise = null;
  function loadEditor() {
    if (!cmPromise) {
      cmPromise = import(ASSET_BASE + "vendor/codemirror.js");
    }
    return cmPromise;
  }

  // Trim surrounding blank lines only. Indentation is preserved on purpose:
  // nessemble is column-sensitive — a token in column 0 is a *label*, an
  // indented token is an *instruction* — so stripping the leading indent would
  // turn instructions into labels.
  function trimBlankLines(text) {
    var lines = text.split("\n");
    while (lines.length && lines[0].trim() === "") lines.shift();
    while (lines.length && lines[lines.length - 1].trim() === "") lines.pop();
    return lines.join("\n");
  }

  // A classic `xxd`-style hex dump: offset, 16 hex bytes, ASCII gutter.
  function hexdump(bytes) {
    var out = [];
    for (var i = 0; i < bytes.length; i += 16) {
      var slice = bytes.subarray(i, i + 16);
      var hex = [];
      var ascii = "";
      for (var j = 0; j < 16; j++) {
        if (j < slice.length) {
          var b = slice[j];
          hex.push((b < 16 ? "0" : "") + b.toString(16));
          ascii += b >= 0x20 && b < 0x7f ? String.fromCharCode(b) : ".";
        } else {
          hex.push("  ");
        }
        if (j === 7) hex.push("");
      }
      out.push(
        ("0000000" + i.toString(16)).slice(-8) +
          "  " +
          hex.join(" ") +
          "  |" +
          ascii +
          "|"
      );
    }
    return out.join("\n");
  }

  function el(tag, className, text) {
    var node = document.createElement(tag);
    if (className) node.className = className;
    if (text != null) node.textContent = text;
    return node;
  }

  // Toolbar icons, as the inner markup of a 24×24 stroke SVG (Feather-style).
  // They inherit the button's text color via `stroke="currentColor"` and are
  // sized in CSS. Kept as strings so a button's glyph can be swapped in place
  // (the toggle flips between `eye`/`eyeOff`).
  var ICONS = {
    reset:
      '<polyline points="1 4 1 10 7 10"/>' +
      '<path d="M3.51 15a9 9 0 1 0 2.13-9.36L1 10"/>',
    clear:
      '<polyline points="3 6 5 6 21 6"/>' +
      '<path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/>' +
      '<line x1="10" y1="11" x2="10" y2="17"/>' +
      '<line x1="14" y1="11" x2="14" y2="17"/>',
    eye:
      '<path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z"/>' +
      '<circle cx="12" cy="12" r="3"/>',
    eyeOff:
      '<path d="M17.94 17.94A10.07 10.07 0 0 1 12 20c-7 0-11-8-11-8a18.45 18.45 0 0 1 5.06-5.94M9.9 4.24A9.12 9.12 0 0 1 12 4c7 0 11 8 11 8a18.5 18.5 0 0 1-2.16 3.19m-6.72-1.07a3 3 0 1 1-4.24-4.24"/>' +
      '<line x1="1" y1="1" x2="23" y2="23"/>',
    download:
      '<path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/>' +
      '<polyline points="7 10 12 15 17 10"/>' +
      '<line x1="12" y1="15" x2="12" y2="3"/>',
    code: '<polyline points="16 18 22 12 16 6"/><polyline points="8 6 2 12 8 18"/>',
  };

  function svgIcon(paths) {
    return (
      '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" ' +
      'stroke-width="2" stroke-linecap="round" stroke-linejoin="round" ' +
      'aria-hidden="true" focusable="false">' +
      paths +
      "</svg>"
    );
  }

  // An icon-only toolbar control (`<button>` or `<a>`): the glyph is decorative,
  // so the accessible name and hover tooltip both come from `label`.
  function iconBtn(tag, className, iconKey, label) {
    var node = el(tag, className);
    node.innerHTML = svgIcon(ICONS[iconKey]);
    node.title = label;
    node.setAttribute("aria-label", label);
    return node;
  }

  // Build the CodeMirror decorations for `text` from the assembler's `tokenize`
  // output — a flat `[start, len, class, …]` array of UTF-16 offsets, which map
  // 1:1 onto CodeMirror document positions. Returns an empty set until the wasm
  // module has loaded (the document then renders plain, matching the old
  // pre-load behavior). Tokens are already sorted and non-overlapping, as
  // `RangeSetBuilder` requires.
  function computeDecorations(cm, text) {
    var builder = new cm.RangeSetBuilder();
    if (wasmMod && tokenClassNames) {
      var triples = wasmMod.tokenize(text);
      var len = text.length;
      for (var i = 0; i < triples.length; i += 3) {
        var start = triples[i];
        var end = start + triples[i + 1];
        if (start >= len) break;
        if (end > len) end = len;
        if (end <= start) continue;
        var name = tokenClassNames[triples[i + 2]];
        builder.add(start, end, cm.Decoration.mark({ class: "na-tok-" + name }));
      }
    }
    return builder.finish();
  }

  // A view plugin that keeps the token decorations in sync with the document. It
  // recomputes on every edit, and on a `refreshHighlight` effect — dispatched
  // once the wasm module finishes loading so an editor built beforehand repaints
  // with color instead of staying plain.
  function makeHighlighter(cm, refreshHighlight) {
    return cm.ViewPlugin.fromClass(
      class {
        constructor(view) {
          this.decorations = computeDecorations(cm, view.state.doc.toString());
        }
        update(update) {
          var refreshed = update.transactions.some(function (tr) {
            return tr.effects.some(function (e) {
              return e.is(refreshHighlight);
            });
          });
          if (update.docChanged || refreshed) {
            this.decorations = computeDecorations(
              cm,
              update.state.doc.toString()
            );
          }
        }
      },
      {
        decorations: function (plugin) {
          return plugin.decorations;
        },
      }
    );
  }

  // A CodeMirror theme that reproduces the component's previous look: the
  // Source Code Pro monospace stack at 0.875em / 1.45 line-height, 0.75em
  // padding, a transparent background (the wrapper supplies the tint), the
  // foreground/caret pinned to `--na-fg`, and the same focus ring. The token
  // colors themselves come from the shared `.na-tok-*` rules in the stylesheet,
  // applied to the spans this component's decorations create.
  function makeTheme(cm) {
    return cm.EditorView.theme({
      "&": {
        fontSize: "0.875em",
        color: "var(--na-fg)",
        backgroundColor: "transparent",
        height: "100%",
      },
      ".cm-scroller": {
        fontFamily:
          '"Source Code Pro", "SFMono-Regular", ui-monospace, Menlo, Consolas, monospace',
        lineHeight: "1.45",
        overflow: "auto",
      },
      ".cm-content": {
        padding: "0.75em 0",
        caretColor: "var(--na-fg)",
        tabSize: "4",
      },
      ".cm-line": { padding: "0 0.75em" },
      "&.cm-focused": { outline: "2px solid #4a90d9", outlineOffset: "-2px" },
      "&.cm-editor.cm-focused": { outline: "2px solid #4a90d9" },
      ".cm-cursor, .cm-dropCursor": { borderLeftColor: "var(--na-fg)" },
      // Blend the search panel (Cmd-F) into the toolbar's tint.
      ".cm-panels": {
        backgroundColor: "rgba(127, 127, 127, 0.08)",
        color: "var(--na-fg)",
      },
      ".cm-panel.cm-search": { padding: "0.4em 0.75em" },
      ".cm-panel.cm-search input, .cm-panel.cm-search button, .cm-panel.cm-search label":
        { fontSize: "0.85em" },
      ".cm-searchMatch": { backgroundColor: "rgba(74, 144, 217, 0.3)" },
      ".cm-searchMatch-selected": { backgroundColor: "rgba(74, 144, 217, 0.5)" },
    });
  }

  var LABEL_ASSEMBLE = "Assemble";

  class NessembleAssembler extends HTMLElement {
    connectedCallback() {
      if (this._built) return;
      this._built = true;

      this._source = trimBlankLines(this.textContent || "");
      this._opts = this.getAttribute("data-opts") || "";
      // `collapsed` starts the byte output hidden (a toggle button expands it).
      // The output of a full NES ROM is large, so the marketing demo sets this;
      // the docs snippets leave it off so the bytes show next to the source.
      this._collapsed = this.hasAttribute("collapsed");
      this.textContent = "";
      this._build();
    }

    _build() {
      this.classList.add("na-host");

      // The editor mounts into this wrapper. Its initial height mirrors the old
      // textarea's row sizing (3–24 lines); it stays user-resizable and scrolls
      // internally once the source outgrows it.
      this._editorWrap = el("div", "na-editor-wrap");
      var rows = Math.min(24, Math.max(3, this._source.split("\n").length));
      // 1.45 line-height × 0.875em text + 1.5em of vertical padding, in the
      // host's em so it tracks the surrounding prose like the rest of the widget.
      this._editorWrap.style.height = (rows * 1.45 * 0.875 + 1.5).toFixed(2) + "em";
      // Show the source immediately (plain) so there's no blank flash before the
      // CodeMirror bundle loads; `_mountEditor` replaces this with the editor.
      this._placeholder = el("pre", "na-editor-placeholder", this._source);
      this._editorWrap.append(this._placeholder);

      var bar = el("div", "na-toolbar");
      this._assembleBtn = el("button", "na-btn na-primary", LABEL_ASSEMBLE);
      this._assembleBtn.type = "button";
      var reset = iconBtn("button", "na-btn na-icon", "reset", "Reset");
      reset.type = "button";
      var clear = iconBtn("button", "na-btn na-icon", "clear", "Clear");
      clear.type = "button";
      var format = iconBtn("button", "na-btn na-icon", "code", "Format code");
      format.type = "button";
      // Collapses/expands the byte output; only shown once there are bytes.
      this._toggle = iconBtn(
        "button",
        "na-btn na-icon na-toggle",
        "eyeOff",
        "Hide output"
      );
      this._toggle.type = "button";
      this._toggle.hidden = true;
      this._download = iconBtn(
        "a",
        "na-btn na-icon na-download",
        "download",
        "Download"
      );
      this._download.setAttribute("download", "assemble.rom");
      this._download.hidden = true;

      bar.append(
        this._assembleBtn,
        reset,
        clear,
        format,
        this._toggle,
        this._download
      );

      this._output = el("pre", "na-output");
      this._output.hidden = true;

      this.append(this._editorWrap, bar, this._output);

      this._assembleBtn.addEventListener("click", this._assemble.bind(this));
      reset.addEventListener("click", () => {
        this._setDoc(this._source);
        this._clearOutput();
      });
      clear.addEventListener("click", () => {
        this._setDoc("");
        this._clearOutput();
      });
      // Reformat the current source in place with `nessemble format` (via wasm).
      format.addEventListener("click", () => {
        loadWasm().then((mod) => {
          this._setDoc(mod.format(this._value()));
        });
      });
      this._toggle.addEventListener("click", () => {
        this._output.hidden = !this._output.hidden;
        this._updateToggle();
      });

      // Mount the real editor, and eagerly load the wasm highlighter so colors
      // appear on page load — not only after the first interaction. Both modules
      // are shared across every editor on the page, so this stays one fetch each
      // regardless of how many editors a page embeds.
      this._mountEditor();
    }

    // Swap the plain placeholder for a CodeMirror editor once its bundle loads,
    // then refresh the highlighting when the wasm tokenizer is ready.
    _mountEditor() {
      var self = this;
      loadEditor().then(function (cm) {
        self._cm = cm;
        self._refreshHighlight = cm.StateEffect.define();

        self._view = new cm.EditorView({
          doc: self._source,
          parent: self._editorWrap,
          extensions: [
            cm.history(),
            cm.keymap.of(
              cm.defaultKeymap
                .concat(cm.historyKeymap)
                .concat(cm.searchKeymap)
            ),
            cm.search({ top: true }),
            cm.highlightSelectionMatches(),
            cm.EditorView.lineWrapping,
            cm.EditorState.tabSize.of(4),
            makeHighlighter(cm, self._refreshHighlight),
            makeTheme(cm),
          ],
        });
        if (self._placeholder) {
          self._placeholder.remove();
          self._placeholder = null;
        }

        loadWasm().then(function () {
          if (self._view) {
            self._view.dispatch({
              effects: self._refreshHighlight.of(null),
            });
          }
        });
      });
    }

    // The current editor contents — from CodeMirror once mounted, else the
    // placeholder text (so an assemble triggered before the bundle loads still
    // works).
    _value() {
      return this._view ? this._view.state.doc.toString() : this._source;
    }

    // Replace the whole document (Reset/Clear). Falls back to updating the
    // placeholder if the editor hasn't mounted yet.
    _setDoc(text) {
      if (this._view) {
        this._view.dispatch({
          changes: {
            from: 0,
            to: this._view.state.doc.length,
            insert: text,
          },
        });
      } else {
        this._source = text;
        if (this._placeholder) this._placeholder.textContent = text;
      }
    }

    // Reflect the current output visibility on the toggle button: an `eye` icon
    // (labelled "Show output") when the bytes are hidden, an `eyeOff` icon
    // ("Hide output") when they're shown.
    _updateToggle() {
      var hidden = this._output.hidden;
      var label = hidden ? "Show output" : "Hide output";
      this._toggle.innerHTML = svgIcon(hidden ? ICONS.eye : ICONS.eyeOff);
      this._toggle.title = label;
      this._toggle.setAttribute("aria-label", label);
      this._toggle.setAttribute("aria-expanded", hidden ? "false" : "true");
    }

    _clearOutput() {
      this._output.hidden = true;
      this._output.textContent = "";
      this._output.classList.remove("na-error");
      this._toggle.hidden = true;
      this._download.hidden = true;
      if (this._url) {
        URL.revokeObjectURL(this._url);
        this._url = null;
      }
    }

    _assemble() {
      var self = this;
      this._assembleBtn.disabled = true;
      this._assembleBtn.textContent = "Assembling…";
      loadWasm()
        .then(function (mod) {
          var result = mod.assemble(self._value(), self._opts);
          self._show(result);
        })
        .catch(function (err) {
          self._showError(String(err && err.message ? err.message : err));
        })
        .then(function () {
          self._assembleBtn.disabled = false;
          self._assembleBtn.textContent = LABEL_ASSEMBLE;
        });
    }

    _show(result) {
      var rom = result.rom;
      var errors = result.errors;
      var warnings = result.warnings;

      if (!result.ok) {
        this._showError(errors.join("\n"));
      } else {
        this._clearOutput();
        var text = hexdump(rom) + "\n\n" + rom.length + " bytes";
        if (warnings.length) text = warnings.join("\n") + "\n\n" + text;
        this._output.textContent = text;
        // Start expanded unless this element opted into collapsed output; the
        // toggle button lets the reader flip it either way.
        this._output.hidden = this._collapsed;
        this._toggle.hidden = false;
        this._updateToggle();

        this._url = URL.createObjectURL(
          new Blob([rom], { type: "application/octet-stream" })
        );
        this._download.href = this._url;
        this._download.hidden = false;
      }

      this.dispatchEvent(
        new CustomEvent("nessemble:assembled", {
          bubbles: true,
          detail: {
            rom: rom,
            ok: result.ok,
            errors: errors,
            warnings: warnings,
          },
        })
      );
    }

    _showError(message) {
      this._clearOutput();
      this._output.textContent = message || "assembly failed";
      this._output.classList.add("na-error");
      this._output.hidden = false;
    }
  }

  customElements.define("nessemble-assembler", NessembleAssembler);

  // Upgrade the old docs' embedding syntax: turn each
  // `<div class="nessemble-assembler" data-opts="…">code</div>` into a
  // `<nessemble-assembler>` so existing pages keep working.
  function upgradeLegacy() {
    document.querySelectorAll("div.nessemble-assembler").forEach(function (div) {
      var replacement = document.createElement("nessemble-assembler");
      if (div.hasAttribute("data-opts")) {
        replacement.setAttribute("data-opts", div.getAttribute("data-opts"));
      }
      if (div.hasAttribute("collapsed")) {
        replacement.setAttribute("collapsed", "");
      }
      replacement.textContent = div.textContent;
      div.replaceWith(replacement);
    });
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", upgradeLegacy);
  } else {
    upgradeLegacy();
  }
})();
