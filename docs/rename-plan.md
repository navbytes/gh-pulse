# gh-pulse to gh-tui rename plan

This plan records the v0.3.0 rename, including the release gates and remaining documentation work.

## Scope and phases

1. Rename the Rust package and executable to `gh-tui`, and expose it as the GitHub CLI extension command `gh tui`. Keep command-line flags and behavior stable.
2. Use `gh-tui` for new XDG config, state, and cache directories. When the new directory is absent but its `gh-pulse` counterpart exists, keep using the legacy directory for reads and writes. Do not migrate or delete files automatically. Keep account-specific state and file permissions intact. If neither exists, create the new directory.
3. Update docs, website, screenshot driver, and fake-gh fixture paths. The website moves from `/gh-pulse/` to `/gh-tui/`. GitHub Pages does not redirect the old project path automatically. Do not reclaim the old repository slug merely to host a redirect because it would break GitHub repository redirects; update prominent links to the new URL instead.
4. Build and release v0.3.0 under `navbytes/gh-tui`. Publish `gh-tui-<os>-<arch>` assets, checksums, and provenance.

## Compatibility limits

The old `gh pulse` command is provided by an installed `gh-pulse` extension. GitHub CLI selects release assets by platform suffix, so duplicate `gh-pulse-*` assets are unnecessary. With GitHub CLI 2.100.0 in an isolated macOS arm64 CLI home, `gh extension upgrade pulse` followed the repository redirect from v0.2.0 to v0.3.0 successfully; the command stayed `gh pulse` and reported gh-tui 0.3.0. A fresh `gh extension install navbytes/gh-tui` provided `gh tui` alongside it. Do not assume the upgrade is verified on other platforms. Remove the old extension only after checking its configuration and state. Do not change the user's installed extensions during automated tests.

## Execution status (2026-10-03)

- Repository moved to `navbytes/gh-tui`; the v0.3.0 release is live. CI, all four platform builds, checksums, provenance, and a downloaded macOS arm64 binary passed. Native attestation verification passed.
- Isolated old-extension upgrade and new-extension install both passed on macOS arm64 without changing the user's installed extensions.
- All 19 screenshots were regenerated with the new binary: 12 from public repositories and 7 from synthetic fake-gh data. The privacy blocklist passed, and the full public set was visually reviewed. The screenshot update awaits a follow-up commit or PR.
- The `/gh-tui/` Pages URL is live and passed browser checks at 320, 390, and 1440 px for branding, images, and overflow. Deploy the final activity-label and screenshot update; the old `/gh-pulse/` Pages path will not redirect.

## Release and publishing gates

- Confirm the renamed binary builds and all Rust tests pass on the v0.3.0 commit. Run the release workflow manually first; it uploads artifacts without publishing.
- Inspect every built asset: executable name, binary startup, platform mapping, checksums, and provenance subjects.
- Regenerate public-repo screenshots from the new binary using the old repository endpoint until the remote rename, then recapture frames that show the old repository name. Keep the privacy blocklist active and manually inspect each image. Global-home screenshots must continue to use the synthetic fake-gh backend.
- Validate README commands and links, website links and images at `/gh-tui/`. Check and update any old `/gh-pulse/` links; the old Pages path will not redirect.
- Tag and publish only after the exact commit passes the native release gates. Test `gh extension install navbytes/gh-tui`, `gh tui`, and the old extension upgrade in disposable GitHub CLI directories against the published assets.

## Rollback

If the new install path fails, pause promotion of v0.3.0, retain the v0.2.0 release, and document the known working `gh pulse` path. The legacy XDG files remain available. Fix the release assets or installer path in a follow-up before directing users to migrate.
