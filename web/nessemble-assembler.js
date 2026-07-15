// The <nessemble-assembler> Web Component: an in-browser 6502/NES assembler.
//
// A vanilla custom element (no framework, no build step) that assembles its
// editable source with the WebAssembly build of nessemble and shows the ROM as
// a hex dump, with a download link and error/warning output. It also upgrades
// legacy `<div class="nessemble-assembler">` elements from the old docs.
//
// On each run it dispatches a `nessemble:assembled` event whose `detail` carries
// `{ rom: Uint8Array, ok, errors, warnings }`, so an embedding page can react —
// e.g. play the ROM in an emulator.
//
// The WebAssembly glue (`nessemble.js`) and binary (`nessemble_bg.wasm`) are
// expected next to this script; their location is resolved from this script's
// own URL, so it works at any page depth.
(function () {
  "use strict";

  // The directory holding this script (and the wasm glue), captured now:
  // `document.currentScript` is only valid during synchronous execution.
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

  function escapeHtml(s) {
    return s.replace(/[&<>]/g, function (c) {
      return c === "&" ? "&amp;" : c === "<" ? "&lt;" : "&gt;";
    });
  }

  // Build highlighted HTML from `value` and a flat `[start, len, class, …]`
  // token array (UTF-16 offsets from `tokenize`). Every character is
  // HTML-escaped; the gaps between tokens (whitespace) are copied verbatim, so
  // the backdrop text is identical to the textarea's, just colored.
  function highlightHtml(value, triples, names) {
    var html = "";
    var pos = 0;
    for (var i = 0; i < triples.length; i += 3) {
      var start = triples[i];
      var end = start + triples[i + 1];
      var name = names[triples[i + 2]];
      if (start > pos) html += escapeHtml(value.slice(pos, start));
      html +=
        '<span class="na-tok-' +
        name +
        '">' +
        escapeHtml(value.slice(start, end)) +
        "</span>";
      pos = end;
    }
    if (pos < value.length) html += escapeHtml(value.slice(pos));
    return html;
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

      this._editor = el("textarea", "na-editor");
      this._editor.spellcheck = false;
      this._editor.value = this._source;
      this._editor.rows = Math.min(24, Math.max(3, this._source.split("\n").length));

      // A colored backdrop rendered directly behind the (transparent-text)
      // textarea. Purely decorative, so it's hidden from assistive tech and can't
      // take pointer events.
      this._highlight = el("pre", "na-highlight");
      this._highlight.setAttribute("aria-hidden", "true");
      var editorWrap = el("div", "na-editor-wrap");
      editorWrap.append(this._highlight, this._editor);

      var bar = el("div", "na-toolbar");
      this._assembleBtn = el("button", "na-btn na-primary", LABEL_ASSEMBLE);
      this._assembleBtn.type = "button";
      var reset = el("button", "na-btn", "Reset");
      reset.type = "button";
      var clear = el("button", "na-btn", "Clear");
      clear.type = "button";
      // Collapses/expands the byte output; only shown once there are bytes.
      this._toggle = el("button", "na-btn na-toggle", "Hide bytes");
      this._toggle.type = "button";
      this._toggle.hidden = true;
      this._download = el("a", "na-btn na-download", "Download");
      this._download.setAttribute("download", "assemble.rom");
      this._download.hidden = true;

      bar.append(this._assembleBtn, reset, clear, this._toggle, this._download);

      this._output = el("pre", "na-output");
      this._output.hidden = true;

      this.append(editorWrap, bar, this._output);

      this._assembleBtn.addEventListener("click", this._assemble.bind(this));
      reset.addEventListener("click", () => {
        this._editor.value = this._source;
        this._clearOutput();
        this._renderHighlight();
      });
      clear.addEventListener("click", () => {
        this._editor.value = "";
        this._clearOutput();
        this._renderHighlight();
      });
      this._toggle.addEventListener("click", () => {
        this._output.hidden = !this._output.hidden;
        this._updateToggle();
      });

      this._editor.addEventListener("input", this._scheduleHighlight.bind(this));
      this._editor.addEventListener("scroll", this._syncScroll.bind(this));

      // Seed the backdrop so the transparent-text textarea shows its source right
      // away (plain until the module loads), then eagerly load the wasm
      // highlighter so colors appear on page load — not only after the first
      // interaction. The module is shared across every editor on the page, so
      // this stays one fetch regardless of how many editors a page embeds.
      this._renderHighlight();
      this._ensureHighlighter();
    }

    // Re-highlight on the next frame, coalescing bursts of keystrokes.
    _scheduleHighlight() {
      if (this._rafPending) return;
      this._rafPending = true;
      var self = this;
      requestAnimationFrame(function () {
        self._rafPending = false;
        self._renderHighlight();
      });
    }

    // Render the backdrop from the current text: colored if the module is
    // loaded, otherwise the plain (escaped) text so it stays visible.
    _renderHighlight() {
      var value = this._editor.value;
      var html =
        wasmMod && tokenClassNames
          ? highlightHtml(value, wasmMod.tokenize(value), tokenClassNames)
          : escapeHtml(value);
      // A trailing newline (or empty buffer) leaves an empty last line in the
      // textarea; give the backdrop the same line height with a zero-width space
      // so the two stay aligned, without adding a visible glyph.
      if (value === "" || value.charCodeAt(value.length - 1) === 10) {
        html += "&#8203;";
      }
      this._highlight.innerHTML = html;
      this._syncScroll();
    }

    // Keep the backdrop's scroll position locked to the textarea's.
    _syncScroll() {
      this._highlight.scrollTop = this._editor.scrollTop;
      this._highlight.scrollLeft = this._editor.scrollLeft;
    }

    // Ensure the wasm highlighter is loading; re-render once it's ready.
    _ensureHighlighter() {
      if (wasmMod) {
        this._renderHighlight();
        return;
      }
      var self = this;
      loadWasm().then(function () {
        self._renderHighlight();
      });
    }

    // Reflect the current output visibility on the toggle button's label.
    _updateToggle() {
      var hidden = this._output.hidden;
      this._toggle.textContent = hidden ? "Show bytes" : "Hide bytes";
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
          var result = mod.assemble(self._editor.value, self._opts);
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
