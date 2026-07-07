# SSH File Explorer for Linux

A native desktop app for browsing and managing files on remote Linux machines over SSH. Connect with a password, then drag, drop, upload, download, copy, paste, and edit files as if they were local.

Built with [Tauri v2](https://v2.tauri.app/) (Rust backend) and React + TypeScript + Tailwind CSS (frontend).

## Features

### Connection
- **SSH connect/disconnect** with password authentication
- **Saved connections** with optional password storage for one-click reconnect
- **Sudo password support** — provide an optional sudo password at login for elevated file operations on protected directories

### File browsing
- **Interactive file browser** displaying file names, sizes, timestamps, and permissions
- **Breadcrumb navigation** bar for quick traversal of the directory tree
- **File search** — search for files by name within the current directory tree
- **Auto-refresh** — directory listings poll every 3 seconds to stay in sync with remote changes
- **Multi-select** — click to select individual files, Shift+click for range selection

### File operations
- **Create files and folders** — including nested directory paths
- **Rename** files and folders inline via the right-click context menu
- **Delete** with confirmation modal (supports multi-select)
- **Copy and paste** files and folders within the remote filesystem, with real-time progress bars
- **Right-click context menu** with Rename, Copy, Paste, Download, Delete, and New File/Folder options
- **Sudo auto-retry** — operations that fail due to permissions are automatically retried with elevated privileges

### Transfers
- **Upload files and folders** from your local machine to the remote server
- **Download files and folders** from the remote server to your local machine
- **Drag and drop to desktop** — drag files out of the app window directly onto your desktop or into other applications (Windows)
- **Drag and drop upload** — drag files from your desktop into the app to upload them
- **Real-time progress bars** for all transfers (uploads, downloads, and copies) with cancel support

### Text editor
- **In-app text editor** powered by CodeMirror with syntax highlighting (JS/TS, Python, JSON, HTML, CSS, XML, Markdown)
- **Double-click to open** files in the editor
- **Save edits** back to the remote server, with sudo fallback for protected files

## Prerequisites

- [Node.js](https://nodejs.org/) (v18 or later)
- [Rust](https://www.rust-lang.org/tools/install) (stable toolchain)
- **Windows**: [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) with the "Desktop development with C++" workload

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

| Artifact | Path |
|----------|------|
| Executable | `ssh-file-explorer.exe` |
| MSI installer | `bundle/msi/ssh-file-explorer_*_x64_en-US.msi` |
| NSIS installer | `bundle/nsis/ssh-file-explorer_*_x64-setup.exe` |

## Project structure

```
ssh-file-explorer/
  src/
    App.tsx            # React frontend — UI, state, and editor
    App.css            # Tailwind CSS import
    main.tsx           # React entry point
  src-tauri/
    src/
      lib.rs           # Rust backend — SSH, SFTP, file ops, saved connections
      virtual_drag.rs  # Windows OLE drag-and-drop to desktop
      main.rs          # Tauri entry point
    Cargo.toml         # Rust dependencies
    tauri.conf.json    # Tauri app configuration
  package.json         # Frontend dependencies
```
