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

  // Load and initialize the wasm module once; every element shares it.
  var wasmPromise = null;
  function loadWasm() {
    if (!wasmPromise) {
      wasmPromise = import(ASSET_BASE + "nessemble.js").then(function (mod) {
        return mod.default(ASSET_BASE + "nessemble_bg.wasm").then(function () {
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

      this.append(this._editor, bar, this._output);

      this._assembleBtn.addEventListener("click", this._assemble.bind(this));
      reset.addEventListener("click", () => {
        this._editor.value = this._source;
        this._clearOutput();
      });
      clear.addEventListener("click", () => {
        this._editor.value = "";
        this._clearOutput();
      });
      this._toggle.addEventListener("click", () => {
        this._output.hidden = !this._output.hidden;
        this._updateToggle();
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
