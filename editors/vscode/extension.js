// VS Code client for the nessemble language server.
//
// The extension itself is deliberately thin: it registers the `nessemble`
// language for `.asm`/`.s` files and spawns `nessemble lsp`, which speaks LSP
// over stdio. Every feature — diagnostics, lint hints, completion, hover,
// formatting, semantic highlighting, outline, go-to-definition, rename, code
// actions — comes from that server, so the editor can never drift from the
// assembler and the CLI. There is no bundled grammar for the same reason: the
// coloring is the server's semantic tokens, produced by the assembler's own
// lexer (see plans/003-ui-syntax-highlighting.md).

const { workspace, window, commands, Uri } = require("vscode");
const { LanguageClient, TransportKind } = require("vscode-languageclient/node");

/** Where users are sent when the executable can't be found. */
const INSTALL_URL = "https://kevinselwyn.github.io/nessemble-rs/docs/installation/";

/** @type {LanguageClient | undefined} */
let client;

function config() {
  return workspace.getConfiguration("nessemble");
}

/**
 * Start the language server and connect a client to it. Resolves once the
 * client has started, or after reporting a failure to the user — a missing
 * executable is by far the most likely cause, so it gets its own message with
 * a link to the install docs rather than a raw spawn error.
 */
async function start() {
  const command = config().get("serverPath", "nessemble");
  const args = config().get("serverArgs", ["lsp"]);

  const serverOptions = {
    run: { command, args, transport: TransportKind.stdio },
    debug: { command, args, transport: TransportKind.stdio },
  };

  const clientOptions = {
    documentSelector: [
      { scheme: "file", language: "nessemble" },
      { scheme: "untitled", language: "nessemble" },
    ],
    outputChannelName: "nessemble",
    // The server discovers a project's entry points from the workspace's
    // `.include` graph, and reads `.nessemblerc` for lint configuration.
    synchronize: {
      fileEvents: workspace.createFileSystemWatcher(
        "**/{.nessemblerc,*.asm,*.s}",
      ),
    },
  };

  client = new LanguageClient(
    "nessemble",
    "nessemble",
    serverOptions,
    clientOptions,
  );

  try {
    await client.start();
  } catch (err) {
    client = undefined;
    const missing = err && (err.code === "ENOENT" || /ENOENT/.test(String(err)));
    const detail = missing
      ? `Could not run \`${command}\`. Install nessemble and make sure it is on your PATH, or set \`nessemble.serverPath\`.`
      : `The nessemble language server failed to start: ${err}`;
    const choice = await window.showErrorMessage(
      detail,
      "Open installation docs",
      "Open settings",
    );
    if (choice === "Open installation docs") {
      await commands.executeCommand("vscode.open", Uri.parse(INSTALL_URL));
    } else if (choice === "Open settings") {
      await commands.executeCommand(
        "workbench.action.openSettings",
        "nessemble.serverPath",
      );
    }
  }
}

async function stop() {
  const current = client;
  client = undefined;
  if (current) {
    await current.stop();
  }
}

async function activate(context) {
  // Restart on a server-path change so the new executable takes effect without
  // a window reload.
  context.subscriptions.push(
    workspace.onDidChangeConfiguration(async (event) => {
      if (
        event.affectsConfiguration("nessemble.serverPath") ||
        event.affectsConfiguration("nessemble.serverArgs")
      ) {
        await stop();
        await start();
      }
    }),
  );

  await start();
}

function deactivate() {
  return stop();
}

module.exports = { activate, deactivate };
