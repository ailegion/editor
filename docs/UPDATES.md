# Automatic updates

The editor uses GitHub Releases for automatic updates on Windows x86-64, Linux x86-64,
and macOS Intel/Apple Silicon. Updates download in the background; the user clicks
**Restart to update** to install. The editor saves its recovery snapshot before exiting
and refuses to exit if unsaved edits cannot be preserved. Close any other editor
instances as well; installation waits up to 90 seconds for them.

## One-time release setup

The public verification key is `update-public-key.txt` and must be committed with the
code. A private signing key was generated locally at `target/update-signing-key.base64`.
The `target` directory is ignored. Do not commit, upload as an artifact, or share that file.

1. Back up the private key somewhere secure before cleaning the `target` directory.
2. In this repository's GitHub **Settings → Secrets and variables → Actions**, create
   a repository secret named **EDITOR_UPDATE_SIGNING_KEY**.
3. Paste the contents of `target/update-signing-key.base64` into that secret.
4. Commit the public key and code, then publish the next version through the tag workflow.

No paid Apple or Windows signing account is needed. The update signatures establish
trust inside the editor; they do not remove operating-system first-install warnings.
Branch/PR builds never receive the signing secret. Tag builds fail if it is missing or
does not match the committed public key. Do not regenerate the key after distributing
builds: existing installations trust the original public key. If it is lost, existing
users need a manual installation carrying the replacement public key.

For a new independent fork only, clear `update-public-key.txt` and run
`cargo run --example update_signer -- keygen` to generate a new identity. Existing
private-key files are never overwritten by that command.

## Release behavior

- Each archive has an `update-<platform>.json` manifest and a detached `.json.sig`
  Ed25519 signature. These are attached to the draft release alongside the archives.
- The signed manifest binds the version, platform, filename, byte length, and SHA-256.
  Verification uses the public key compiled into the editor, not a downloaded key.
- Only published stable releases newer than the running version are installed.
  Drafts, prereleases, equal versions, and downgrades are ignored.
- Existing releases that predate this feature need one manual upgrade to a build
  containing the updater. After that, new stable releases can update automatically.
- Development builds do not check for updates. Extracted CI release artifacts contain
  the updater, but they do not become update sources until published as signed releases.

## Installation and recovery

Use an extracted release installation in a location where you can write both the
installation and its parent directory. On macOS, move the app out of the ZIP/download
preview into a writable Applications folder. The updater never requests administrator
privileges; unwritable installations report an error instead.

The copied Rust helper waits for running editor instances to release their shared lock,
verifies the signed archive again, and extracts into a sibling temporary directory.
It rejects archive traversal, links, and excessive extraction sizes. It replaces only
the packaged files on Windows/Linux, preserving unrelated files; macOS replaces the
app bundle. User themes/settings in the user configuration directory stay separate.

If replacement or starting the new process fails, the helper attempts to restore the
previous installation and restart it. This does not detect every crash occurring after
successful process creation, and cannot guarantee rollback after power loss.
The `.editor-update-*` sibling directory retains the previous files under `backup` for
manual recovery. Once the new version works and all updater processes have exited,
that staging directory can be removed manually. Do not remove an active update folder.

Before publishing the first update, test installing from one signed version to a newer
signed version on each OS, including an unsaved tab, a failed download, an unwritable
installation, and a second editor instance still running.
