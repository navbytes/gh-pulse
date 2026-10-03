# gh-pulse to gh-tui rename plan

This plan tracks the v0.3.0 rename. It is not a release announcement. Do not publish a tag or move the GitHub repository until the gates below are complete.

## Scope and phases

1. Rename the Rust package and executable to `gh-tui`, and expose it as the GitHub CLI extension command `gh tui`. Keep command-line flags and behavior stable.
2. Use `gh-tui` for new XDG config, state, and cache directories. When the new directory is absent but its `gh-pulse` counterpart exists, keep using the legacy directory for reads and writes. Do not migrate or delete files automatically. Keep account-specific state and file permissions intact. If neither exists, create the new directory.
3. Update docs, website, screenshot driver, and fake-gh fixture paths. The website moves from `/gh-pulse/` to `/gh-tui/`. GitHub Pages does not redirect the old project path automatically. Do not reclaim the old repository slug merely to host a redirect because it would break GitHub repository redirects; update prominent links to the new URL instead.
4. Build and release v0.3.0 under `navbytes/gh-tui`. Publish `gh-tui-<os>-<arch>` assets, checksums, and provenance.

## Compatibility limits

The old `gh pulse` command is provided by an installed `gh-pulse` extension. GitHub CLI selects release assets by platform suffix, so duplicate `gh-pulse-*` assets are unnecessary. Whether `gh extension upgrade pulse` follows the repository redirect is still unverified. Confirm it in an isolated GitHub CLI home with a real published release before promising it to users. The new install command is `gh extension install navbytes/gh-tui`; if the old extension remains installed, remove it only after its configuration and state have been checked. Do not change the user's installed extensions during automated tests.

## Release and publishing gates

- Confirm the renamed binary builds and all Rust tests pass on the v0.3.0 commit. Run the release workflow manually first; it uploads artifacts without publishing.
- Inspect every built asset: executable name, binary startup, platform mapping, checksums, and provenance subjects.
- Regenerate public-repo screenshots from the new binary using the old repository endpoint until the remote rename, then recapture frames that show the old repository name. Keep the privacy blocklist active and manually inspect each image. Global-home screenshots must continue to use the synthetic fake-gh backend.
- Validate README commands and links, website links and images at `/gh-tui/`. Check and update any old `/gh-pulse/` links; the old Pages path will not redirect.
- Tag and publish only after the exact commit passes the native release gates. Test `gh extension install navbytes/gh-tui`, `gh tui`, and the old extension upgrade in disposable GitHub CLI directories against the published assets.

## Rollback

If the new install path fails, pause promotion of v0.3.0, retain the v0.2.0 release, and document the known working `gh pulse` path. The legacy XDG files remain available. Fix the release assets or installer path in a follow-up before directing users to migrate.
