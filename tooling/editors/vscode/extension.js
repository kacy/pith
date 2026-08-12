// pith extension: starts `<serverPath> lsp` and attaches it to .pith files.
// plain javascript, no build step. the heavy lifting is vscode-languageclient.
const vscode = require("vscode");
const { LanguageClient } = require("vscode-languageclient/node");

let client;

function activate(context) {
  const serverPath = vscode.workspace
    .getConfiguration("pith")
    .get("serverPath", "pith");

  const serverOptions = {
    command: serverPath,
    args: ["lsp"],
  };

  const clientOptions = {
    documentSelector: [{ scheme: "file", language: "pith" }],
  };

  client = new LanguageClient(
    "pith",
    "Pith Language Server",
    serverOptions,
    clientOptions
  );

  // start() returns a promise; the client surfaces spawn failures
  // (e.g. serverPath not found) in the "Pith Language Server" output panel.
  client.start();

  context.subscriptions.push({
    dispose: () => (client ? client.stop() : undefined),
  });
}

function deactivate() {
  if (!client) {
    return undefined;
  }
  return client.stop();
}

module.exports = { activate, deactivate };
