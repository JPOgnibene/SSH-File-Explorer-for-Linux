# SSH File Explorer for Linux

Connect to a remote Linux machine using SSH and browse its filesystem in a local, native desktop GUI.

Built with [Tauri v2](https://v2.tauri.app/) (Rust backend) and React + TypeScript (frontend).

## Features

- **SSH connect/disconnect** with password authentication
- **Interactive file browser** with breadcrumb navigation, file sizes, timestamps, and permissions
- **Saved connections** with optional password storage for one-click reconnect
- **In-app text editor** powered by CodeMirror with syntax highlighting (JS/TS, Python, JSON, HTML, CSS, XML, Markdown)
- **File operations** — create files, create directories (including nested paths), and delete with confirmation
- **Sudo auto-retry** — operations that fail with "permission denied" are silently retried with elevated privileges
- **Auto-refresh** — directory listings poll every 3 seconds to stay in sync with remote changes

## Prerequisites

- [Node.js](https://nodejs.org/) (v18 or later)
- [Rust](https://www.rust-lang.org/tools/install) (stable toolchain)
- Platform-specific build dependencies:
  - **Windows**: [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) with the "Desktop development with C++" workload
  - **macOS**: Xcode Command Line Tools (`xcode-select --install`)
  - **Linux**: `build-essential`, `libwebkit2gtk-4.1-dev`, `libssl-dev`, `libgtk-3-dev`, `libayatana-appindicator3-dev`, `librsvg2-dev`

## Build from source

```bash
# Clone the repository
git clone https://github.com/JPOgnibene/SSH-File-Explorer-for-Linux.git
cd SSH-File-Explorer-for-Linux/ssh-file-explorer

# Install frontend dependencies
npm install

# Run in development mode (hot-reload)
npm run tauri dev

# Build release binaries and installers
npm run tauri build
```

Build output is written to `ssh-file-explorer/src-tauri/target/release/`:

| Platform | Binary | Installer |
|----------|--------|-----------|
| Windows  | `ssh-file-explorer.exe` | `bundle/msi/*.msi` and `bundle/nsis/*.exe` |
| macOS    | `ssh-file-explorer` | `bundle/dmg/*.dmg` and `bundle/macos/*.app` |
| Linux    | `ssh-file-explorer` | `bundle/deb/*.deb` and `bundle/appimage/*.AppImage` |

## Project structure

```
ssh-file-explorer/
  src/
    App.tsx          # React frontend — UI, state, and editor
    App.css          # Tailwind CSS import
    main.tsx         # React entry point
  src-tauri/
    src/
      lib.rs         # Rust backend — SSH commands, file ops, saved connections
      main.rs        # Tauri entry point
    Cargo.toml       # Rust dependencies
    tauri.conf.json  # Tauri app configuration
  package.json       # Frontend dependencies
```
