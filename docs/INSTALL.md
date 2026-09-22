# Download and run

Download the archive for your computer from https://github.com/ailegion/editor/releases.
Choose a release asset, not GitHub's automatically generated source-code archives.
Extract the whole archive; keep the themes and license files with the application.
Rust is not required to run a downloaded build.

## Windows (x86-64)

Extract the ZIP and run `editor.exe`. Builds do not have a paid signing certificate.
If Windows SmartScreen warns about an unknown publisher, use **More info → Run anyway**
only after verifying you downloaded it from the project's release page. Organization
security policies may prevent this override.

## macOS (13 or newer)

Choose `macos-aarch64` for Apple Silicon or `macos-x86_64` for Intel.
Extract the ZIP and move `editor.app` to Applications. Builds use free ad-hoc signing
and are not notarized by Apple. After attempting to open it, use
**System Settings → Privacy & Security → Open Anyway** if macOS blocks it.
Only approve a download you trust. Managed Macs may not allow an override.
Licenses and themes are inside `editor.app/Contents/Resources`.

## Linux (x86-64)

Extract the `.tar.gz` archive and run `./editor` from the extracted directory.
The binary is built on Ubuntu 22.04 (glibc 2.35); it is not a fully static build
or an AppImage. Use Ubuntu 22.04 or a compatible newer distribution with a desktop
session, graphics drivers, Fontconfig, and XKB/Wayland/X11 libraries installed.
If it does not start, run `ldd ./editor` to identify missing shared libraries.
Other distributions may need equivalent runtime packages installed.

## Optional features

- Git features need Git installed and available on PATH.
- Claude/Codex ACP features need Node.js/npm on PATH and the agent's credentials.
  On macOS, Finder-launched applications may not inherit paths from shell startup files.
- Local AI requires your own running model server; remote APIs require your own credentials.
- Editing files does not require configuring an AI provider.

## Verify downloads

Each release includes `SHA256SUMS.txt`. On Linux use `sha256sum -c SHA256SUMS.txt`
with the downloaded files present; on macOS use `shasum -a 256 <archive>`.
On Windows use `Get-FileHash <archive> -Algorithm SHA256` and compare the hash.
Checksums detect damaged downloads; they are not a publisher signature.
