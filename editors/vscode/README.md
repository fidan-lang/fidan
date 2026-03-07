# Fidan Language — VS Code Extension

First-class VS Code support for the [Fidan](https://github.com/fidan-lang/fidan) programming language.

## Features

| Feature | Status |
|---|---|
| Syntax highlighting | ✅ |
| Bracket / comment configuration | ✅ |
| Format on save (`fidan format`) | ✅ via LSP |
| Error and warning diagnostics | ✅ via LSP |
| Hover documentation | 🚧 planned |
| Auto-completion | 🚧 planned |
| Go to definition / references | 🚧 planned |
| Inline hints | 🚧 planned |

## Requirements

- The `fidan` binary must be on your `PATH`, or you can point the extension at it via the `fidan.server.path` setting.
- Build from source: `cargo build --release` in the workspace root, then add `target/release/` to PATH.

## Extension Settings

| Setting | Default | Description |
|---|---|---|
| `fidan.server.path` | `"fidan"` | Path to the `fidan` binary |
| `fidan.server.extraArgs` | `[]` | Extra args forwarded to `fidan lsp` |
| `fidan.trace.server` | `"off"` | LSP trace level: `off`, `messages`, `verbose` |
| `fidan.format.indentWidth` | `4` | Spaces per indent level |
| `fidan.format.maxLineLen` | `100` | Target max line length |
| `fidan.format.onSave` | `true` | Format automatically on save |

## Commands

| Command | Description |
|---|---|
| `Fidan: Restart Language Server` | Kill and restart the LSP process |
| `Fidan: Show Language Server Output` | Open the LSP output channel |

## Development

```bash
cd editors/vscode
npm install
npm run compile
# Press F5 in VS Code to launch the Extension Development Host.
```

## License

This project uses a custom license file based on Apache License 2.0,
with additional terms regarding trademark use and commercial distribution
of the Fidan language itself.

See [LICENSE](../../LICENSE) for full license text.

Copyright © 2026 Kaan Gönüldinc. All rights reserved.

Fidan™ is a trademark of Kaan Gönüldinc (AppSolves).
