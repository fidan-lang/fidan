import * as vscode from "vscode";
import {
    LanguageClient,
    LanguageClientOptions,
    RevealOutputChannelOn,
    ServerOptions,
    TransportKind,
} from "vscode-languageclient/node";

let client: LanguageClient | undefined;
let outputChannel: vscode.OutputChannel;
let statusBarItem: vscode.StatusBarItem;

// ---------------------------------------------------------------------------
// Activation
// ---------------------------------------------------------------------------

export async function activate(context: vscode.ExtensionContext): Promise<void> {
    outputChannel = vscode.window.createOutputChannel("Fidan Language Server");
    context.subscriptions.push(outputChannel);

    // Status bar item showing LSP server state.
    statusBarItem = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 10);
    statusBarItem.command = "fidan.showOutput";
    statusBarItem.tooltip = "Fidan Language Server — click to show output";
    setStatusBarStarting();
    statusBarItem.show();
    context.subscriptions.push(statusBarItem);

    // Register commands before starting the server so they're immediately available.
    context.subscriptions.push(
        vscode.commands.registerCommand("fidan.restartServer", async () => {
            setStatusBarStarting();
            await stopClient();
            await startClient(context);
            vscode.window.showInformationMessage("Fidan language server restarted.");
        }),
        vscode.commands.registerCommand("fidan.showOutput", () => {
            outputChannel.show();
        }),
        vscode.commands.registerCommand("fidan.runFile", async () => {
            const editor = vscode.window.activeTextEditor;
            if (!editor) {
                vscode.window.showWarningMessage("Fidan: No active file to run.");
                return;
            }
            if (editor.document.languageId !== "fidan") {
                vscode.window.showWarningMessage("Fidan: Active file is not a Fidan (.fdn) file.");
                return;
            }
            await editor.document.save();
            const filePath = editor.document.uri.fsPath;
            const config = vscode.workspace.getConfiguration("fidan");
            const terminalName: string = config.get("run.terminalName") ?? "Fidan";
            const fidan: string = config.get("server.path") ?? "fidan";
            let terminal = vscode.window.terminals.find(t => t.name === terminalName);
            if (!terminal) {
                terminal = vscode.window.createTerminal(terminalName);
            }
            terminal.show(true);
            terminal.sendText(`${fidan} run "${filePath}"`);
        }),
    );

    await startClient(context);
}

// ---------------------------------------------------------------------------
// Deactivation
// ---------------------------------------------------------------------------

export async function deactivate(): Promise<void> {
    await stopClient();
}

// ---------------------------------------------------------------------------
// Client lifecycle helpers
// ---------------------------------------------------------------------------

async function startClient(context: vscode.ExtensionContext): Promise<void> {
    const config = vscode.workspace.getConfiguration("fidan");
    const binaryPath: string = config.get("server.path") ?? "fidan";
    const extraArgs: string[] = config.get("server.extraArgs") ?? [];

    // The server is launched as `fidan lsp [extraArgs]`.
    // The `debug` options mirror `run` exactly — the `Lsp` CLI subcommand
    // accepts no flags, so never pass `--debug` or similar.
    const serverOptions: ServerOptions = {
        run: {
            command: binaryPath,
            args: ["lsp", ...extraArgs],
            transport: TransportKind.stdio,
        },
        debug: {
            command: binaryPath,
            args: ["lsp", ...extraArgs],
            transport: TransportKind.stdio,
        },
    };

    const clientOptions: LanguageClientOptions = {
        documentSelector: [{ scheme: "file", language: "fidan" }],
        outputChannel,
        revealOutputChannelOn: RevealOutputChannelOn.Error,
        traceOutputChannel: outputChannel,
        initializationOptions: {
            indentWidth: config.get<number>("format.indentWidth") ?? 4,
            maxLineLen: config.get<number>("format.maxLineLen") ?? 100,
        },
        synchronize: {
            fileEvents: vscode.workspace.createFileSystemWatcher("**/*.fdn"),
        },
        markdown: { isTrusted: true },
    };

    client = new LanguageClient(
        "fidan",
        "Fidan Language Server",
        serverOptions,
        clientOptions,
    );

    // Register format-on-save if enabled.
    if (config.get<boolean>("format.onSave") ?? true) {
        context.subscriptions.push(
            vscode.workspace.onWillSaveTextDocument(async (event: vscode.TextDocumentWillSaveEvent) => {
                if (event.document.languageId !== "fidan") return;
                if (!client || !client.isRunning()) return;
                event.waitUntil(
                    vscode.commands.executeCommand<vscode.TextEdit[]>(
                        "vscode.executeFormatDocumentProvider",
                        event.document.uri,
                        { tabSize: 4, insertSpaces: true },
                    ).then((edits: vscode.TextEdit[] | undefined) => edits ?? []),
                );
            }),
        );
    }

    // Watch configuration changes and restart the server when the binary path
    // or extra args change.
    context.subscriptions.push(
        vscode.workspace.onDidChangeConfiguration(async (e: vscode.ConfigurationChangeEvent) => {
            if (
                e.affectsConfiguration("fidan.server.path") ||
                e.affectsConfiguration("fidan.server.extraArgs")
            ) {
                await stopClient();
                await startClient(context);
            }
        }),
    );

    try {
        await client.start();
        outputChannel.appendLine("[fidan] Language server started.");
        setStatusBarRunning();
    } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        outputChannel.appendLine(`[fidan] Failed to start language server: ${message}`);
        outputChannel.appendLine(
            `[fidan] Make sure the 'fidan' binary is on your PATH or set 'fidan.server.path' in settings.`,
        );
        setStatusBarError();
        // Do not throw — users may not have the binary installed yet (syntax
        // highlighting still works without the server).
    }
}

async function stopClient(): Promise<void> {
    if (client) {
        outputChannel.appendLine("[fidan] Stopping language server.");
        await client.stop();
        client = undefined;
        setStatusBarStopped();
    }
}

// ---------------------------------------------------------------------------
// Status bar helpers
// ---------------------------------------------------------------------------

function setStatusBarRunning(): void {
    if (!statusBarItem) return;
    statusBarItem.text = "$(check) Fidan";
    statusBarItem.backgroundColor = undefined;
    statusBarItem.tooltip = "Fidan Language Server — running. Click to show output.";
}

function setStatusBarStarting(): void {
    if (!statusBarItem) return;
    statusBarItem.text = "$(sync~spin) Fidan";
    statusBarItem.backgroundColor = undefined;
    statusBarItem.tooltip = "Fidan Language Server — starting…";
}

function setStatusBarStopped(): void {
    if (!statusBarItem) return;
    statusBarItem.text = "$(circle-slash) Fidan";
    statusBarItem.backgroundColor = undefined;
    statusBarItem.tooltip = "Fidan Language Server — stopped. Click to show output.";
}

function setStatusBarError(): void {
    if (!statusBarItem) return;
    statusBarItem.text = "$(error) Fidan";
    statusBarItem.backgroundColor = new vscode.ThemeColor("statusBarItem.errorBackground");
    statusBarItem.tooltip = "Fidan Language Server — failed to start. Click to show output.";
}
