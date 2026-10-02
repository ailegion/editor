# Releasing

How to cut a release. The source of truth is `.github/workflows/release.yml`; update this
file when that workflow changes.

## Steps

1. Bump `version` in `Cargo.toml`. Also update the `editor` entry in `Cargo.lock` (only that
   entry; other crates may share the same version string), or let any `cargo build` do it.
2. Commit and push.
3. Tag the commit with `v` + the exact `Cargo.toml` version and push the tag. Existing tags are
   lightweight (`v0.1.0` … `v0.1.7`); `v0.1.7` was made on a `dev` commit.

   ```sh
   git tag v0.1.8
   git push origin v0.1.8
   ```

4. Wait for the **Build and release** workflow on the tag to finish.
5. On GitHub, review the **draft** release it created and publish it by hand. Nothing is
   public, and no installed editor auto-updates, until the draft is published.

## What the tag workflow does

- **licenses**: generates `THIRD_PARTY_LICENSES.html` with `cargo-about` and collects
  dependency license/notice files.
- **build** (Windows x86-64, Linux x86-64, macOS aarch64, macOS x86-64):
  - runs `cargo test --release --locked` on each platform;
  - fails if the tag is not `v<Cargo.toml version>`;
  - fails if the `EDITOR_UPDATE_SIGNING_KEY` repository secret is missing or
    `update-public-key.txt` is empty;
  - packages `editor-<version>-<platform>.zip` (`.tar.gz` on Linux) with themes, licenses,
    `README.md` and `docs/INSTALL.md`; the macOS build is an ad-hoc signed `editor.app`;
  - signs the archive, producing `update-<platform>.json` and `.json.sig` (see `docs/UPDATES.md`).
- **release**: checks that all four archives and all update manifests and signatures exist,
  writes `SHA256SUMS.txt`, and creates a draft release titled `editor v<version>`. The body is
  `docs/RELEASE_NOTES.md` plus GitHub's generated notes. A tag containing `-`
  (for example `v0.2.0-beta.1`) becomes a prerelease, which the auto-updater ignores.

Pushes to `main`, pull requests and manual runs build and test only; they never release.

## If something fails

- **Tag/version mismatch**: delete the tag, fix the version, then tag again:

  ```sh
  git tag -d v0.1.8
  git push origin :refs/tags/v0.1.8
  ```

- **Re-running on the same tag** uploads over the existing draft's assets (`--clobber`).
  If that release is already published, the workflow refuses to overwrite it; bump to a
  new version instead.
- **Signing key**: see the one-time setup in `docs/UPDATES.md`. Never regenerate the key;
  installed editors trust only the original public key.
