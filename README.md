# SSH File Explorer for Linux

A native desktop app for browsing and managing files on remote Linux machines over SSH. Connect with a password, then drag, drop, upload, download, copy, paste, and edit files as if they were local.

Built with [Tauri v2](https://v2.tauri.app/) (Rust backend) and React + TypeScript + Tailwind CSS (frontend).

## Download

Prebuilt binaries for each release are published on the
[Releases page](https://github.com/JPOgnibene/SSH-File-Explorer-for-Linux/releases).

- **Windows:** the `.msi` or NSIS `-setup.exe` installer.
- **Linux:** the `.AppImage` (portable — `chmod +x` and run), or the `.deb` /
  `.rpm` to install with your package manager. See
  [Installing and running on Linux](#installing-and-running-on-linux) for
  details, and [Flatpak](#flatpak) for a sandboxed install.

To build it yourself instead, see [Build from source](#build-from-source).

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
- **Drag files out to the desktop** (**Windows only**) — drag files directly onto your desktop or into other applications. On Linux/macOS, use **Download** (toolbar or right-click) instead
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
- **Linux**: the Tauri v2 system libraries. On Debian/Ubuntu:

  ```bash
  sudo apt-get install libwebkit2gtk-4.1-dev build-essential curl wget file \
    libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev patchelf
  ```

  (See the [Tauri Linux prerequisites](https://v2.tauri.app/start/prerequisites/#linux) for other distributions.)

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

**Windows**

| Artifact | Path |
|----------|------|
| Executable | `ssh-file-explorer.exe` |
| MSI installer | `bundle/msi/ssh-file-explorer_*_x64_en-US.msi` |
| NSIS installer | `bundle/nsis/ssh-file-explorer_*_x64-setup.exe` |

**Linux**

| Artifact | Path |
|----------|------|
| AppImage | `bundle/appimage/ssh-file-explorer_*_amd64.AppImage` |
| Debian package | `bundle/deb/ssh-file-explorer_*_amd64.deb` |
| RPM package | `bundle/rpm/ssh-file-explorer-*.x86_64.rpm` |

## Installing and running on Linux

The Linux build ships in three formats, all for x86-64 (amd64).

### AppImage (portable — recommended for quick testing)

No installation required. It bundles its own GTK/WebKit, so it runs on most
distributions without installing extra system packages:

```bash
chmod +x ssh-file-explorer_0.1.0_amd64.AppImage
./ssh-file-explorer_0.1.0_amd64.AppImage
```

### Debian / Ubuntu (.deb)

```bash
sudo apt install ./ssh-file-explorer_0.1.0_amd64.deb
```

The package declares its runtime dependencies (`libwebkit2gtk-4.1-0`,
`libgtk-3-0`), which `apt` installs automatically. It registers a desktop menu
entry under the *Utility* category.

### Fedora / RHEL / openSUSE (.rpm)

```bash
sudo dnf install ./ssh-file-explorer-0.1.0-1.x86_64.rpm
```

### Linux runtime requirements

- **WebKitGTK 4.1** (`libwebkit2gtk-4.1-0`) and **GTK 3** — required by the
  `.deb`/`.rpm` (resolved automatically by the package manager) and bundled
  inside the AppImage.
- **A Secret Service provider** (GNOME Keyring or KWallet) — only needed if you
  want the app to remember saved-connection passwords (see
  [Security & credential storage](#security--credential-storage)). The app runs
  fine without one; it simply cannot store passwords.

### Flatpak

A Flatpak manifest is provided in [`flatpak/`](flatpak/) for easy installation on
distributions like Linux Mint. It builds from the `.deb` above rather than from
source. Because Flatpak/D-Bus app IDs cannot contain hyphens, the Flatpak uses
the ID `io.github.jpognibene.SshFileExplorer`; the app's Tauri identifier (and
where it stores saved connections) is unchanged.

One-time setup:

```bash
sudo apt install flatpak-builder   # or: flatpak install flathub org.flatpak.Builder
flatpak install flathub org.gnome.Platform//47 org.gnome.Sdk//47
```

Build and install (after `npm run tauri build` has produced the `.deb`):

```bash
cd flatpak
./build.sh            # build and install for the current user
./build.sh --bundle   # also export a shareable single-file .flatpak
```

Then launch it:

```bash
flatpak run io.github.jpognibene.SshFileExplorer
```

The sandbox is granted network access (SSH), your home directory (uploads,
downloads, and `~/.ssh` key discovery), and `org.freedesktop.secrets` (so saved
passwords work via the OS keyring). Publishing to Flathub is a separate step that
additionally requires screenshots and an SPDX project license in the metainfo
file.

### Platform differences

- **Drag files out to the desktop:** the native drag-onto-the-desktop drop is
  **Windows-only** (it relies on Windows OLE). On Linux/macOS this gesture is
  disabled; download files with the **Download** button in the toolbar or via
  right-click → **Download**, which prompts for a destination folder. Every
  other feature is identical across platforms.

## Security & credential storage

The app never writes connection passwords to its own files. Saved passwords are
handed to the operating system's native secret store via the
[`keyring`](https://crates.io/crates/keyring) crate:

| Platform | Secret store |
|----------|--------------|
| Windows  | Windows Credential Manager |
| macOS    | Keychain |
| Linux    | Secret Service API (GNOME Keyring / KWallet) over D-Bus |

- **Connection metadata** (label, host, port, username) is stored in
  `connections.json` in the app config directory. The password field is never
  serialized to this file.
- **Saved passwords** live only in the OS secret store, encrypted at rest and
  unlocked by your login session.
- **The sudo password** entered at login is held in memory for the session
  only — it is never written to disk or to the secret store.

On Linux, if no Secret Service provider is running (common on minimal or
headless setups), saving or loading a stored password will fail; install and
start `gnome-keyring` or `kwallet` to enable it.

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
flatpak/               # Flatpak manifest, metainfo, and build script
```

## License

Released under the [MIT License](LICENSE).
