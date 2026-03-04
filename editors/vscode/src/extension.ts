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

// ---------------------------------------------------------------------------
// Activation
// ---------------------------------------------------------------------------

export async function activate(context: vscode.ExtensionContext): Promise<void> {
    outputChannel = vscode.window.createOutputChannel("Fidan Language Server");
    context.subscriptions.push(outputChannel);

    // Register commands before starting the server so they're immediately available.
    context.subscriptions.push(
        vscode.commands.registerCommand("fidan.restartServer", async () => {
            await stopClient();
            await startClient(context);
            vscode.window.showInformationMessage("Fidan language server restarted.");
        }),
        vscode.commands.registerCommand("fidan.showOutput", () => {
            outputChannel.show();
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
    const serverOptions: ServerOptions = {
        run: {
            command: binaryPath,
            args: ["lsp", ...extraArgs],
            transport: TransportKind.stdio,
        },
        debug: {
            command: binaryPath,
            args: ["lsp", "--debug", ...extraArgs],
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
            vscode.workspace.onWillSaveTextDocument(async (event) => {
                if (event.document.languageId !== "fidan") return;
                if (!client || !client.isRunning()) return;
                event.waitUntil(
                    vscode.commands.executeCommand<vscode.TextEdit[]>(
                        "vscode.executeFormatDocumentProvider",
                        event.document.uri,
                        { tabSize: 4, insertSpaces: true },
                    ).then((edits) => edits ?? []),
                );
            }),
        );
    }

    // Watch configuration changes and restart the server when the binary path
    // or extra args change.
    context.subscriptions.push(
        vscode.workspace.onDidChangeConfiguration(async (e) => {
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
    } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        outputChannel.appendLine(`[fidan] Failed to start language server: ${message}`);
        outputChannel.appendLine(
            `[fidan] Make sure the 'fidan' binary is on your PATH or set 'fidan.server.path' in settings.`,
        );
        // Do not throw — users may not have the binary installed yet (syntax
        // highlighting still works without the server).
    }
}

async function stopClient(): Promise<void> {
    if (client) {
        outputChannel.appendLine("[fidan] Stopping language server.");
        await client.stop();
        client = undefined;
    }
}
