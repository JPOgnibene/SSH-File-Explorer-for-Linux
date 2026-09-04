# Publishing to Flathub

[Flathub](https://flathub.org/) is the app store most Linux desktops (including
Linux Mint) search by default, so a listing there is the easiest way for
non-technical users to install and auto-update the app.

This is more involved than the local Flatpak (`../build.sh`, which builds from
the `.deb`) because **Flathub builds from source in a network-isolated sandbox**.
Every dependency must therefore be vendored offline. The draft manifest in
[`io.github.jpognibene.SshFileExplorer.yml`](io.github.jpognibene.SshFileExplorer.yml)
is a correct-shaped starting point but still needs the generated source lists
below and a local test build before it will pass Flathub CI.

## Prerequisites

- A tagged release (e.g. `v1.1.0`) so the manifest can pin a `commit`.
- These SDK extensions installed for local test builds:

  ```bash
  flatpak install flathub org.freedesktop.Sdk.Extension.node20//24.08 \
    org.freedesktop.Sdk.Extension.rust-stable//24.08
  ```

  (Match the `//` version to your GNOME 47 SDK's freedesktop base.)

## 1. Generate the offline dependency lists

Install the Flatpak builder tools:

```bash
pip install --user aiohttp toml
git clone https://github.com/flatpak/flatpak-builder-tools.git
```

Rust crates (from `Cargo.lock`):

```bash
python flatpak-builder-tools/cargo/flatpak-cargo-generator.py \
  ../../ssh-file-explorer/src-tauri/Cargo.lock -o cargo-sources.json
```

npm packages (from `package-lock.json`):

```bash
python flatpak-builder-tools/node/flatpak-node-generator.py npm \
  ../../ssh-file-explorer/package-lock.json -o node-sources.json
```

Both files are regenerated whenever the corresponding lockfile changes.

## 2. Pin the commit

Set `commit:` in the manifest to the exact SHA the release tag points to:

```bash
git rev-list -n 1 v1.1.0
```

## 3. Test-build locally

```bash
flatpak-builder --force-clean --user --install build \
  io.github.jpognibene.SshFileExplorer.yml
flatpak run io.github.jpognibene.SshFileExplorer
```

Iterate until it builds offline and launches. Common fixes: SDK-extension
version alignment, the `npm ci --offline` cache setup emitted by the node
generator, and icon/desktop paths.

## 4. Validate metadata

```bash
flatpak run org.freedesktop.appstream-glib validate \
  io.github.jpognibene.SshFileExplorer.metainfo.xml
```

Flathub also requires **at least one screenshot** in the metainfo (`<screenshots>`),
hosted at a public URL (e.g. a raw file in this repo).

## 5. Submit

1. Fork [`flathub/flathub`](https://github.com/flathub/flathub) and create a
   branch named `io.github.jpognibene.SshFileExplorer`.
2. Add the manifest, `cargo-sources.json`, `node-sources.json`, the `.desktop`,
   and the `.metainfo.xml`.
3. Open a PR against the `new-pr` branch. Flathub's bot builds it and a
   reviewer checks it. Because the app ID is `io.github.jpognibene.*`, ownership
   is verified through the matching GitHub account.

See the [Flathub submission docs](https://docs.flathub.org/docs/for-app-authors/submission)
for the authoritative process.

## Notes

- The app ID must stay `io.github.jpognibene.SshFileExplorer` (hyphen-free,
  matching your GitHub account); the app's own Tauri identifier
  (`com.jp.ssh-file-explorer`) is unrelated and unchanged.
- Until the Flathub listing is live, the local Flatpak (`../build.sh`) and the
  GitHub Releases artifacts are the distribution channels.
