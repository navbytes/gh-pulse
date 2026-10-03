# Contributing

Thanks for helping. gh-pulse is small on purpose: a thin TUI over `gh`. Prefer the shortest change that works.

## Setup

```sh
git clone https://github.com/navbytes/gh-pulse && cd gh-pulse
cargo run -- -R owner/repo          # needs `gh auth login`
```

Rust 1.89+ (edition 2024; 1.89 is the oldest toolchain the project is tested on, not a verified minimum; CI's `msrv` job builds with `rust-version` from `Cargo.toml`). Read [docs/architecture.md](docs/architecture.md) first; it is short.

## Checks before a PR

```sh
cargo fmt --check
# the default build includes syntax highlighting; check the small plain build too
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --no-default-features -- -D warnings
cargo test
cargo test --no-default-features
```

Optional live tests (they call `gh`, read-only) take their target from the environment:

```sh
GH_PULSE_REPO=owner/repo cargo test -- --ignored --nocapture
GH_PULSE_HUGE_PR=owner/repo#123 cargo test -- --ignored --nocapture
cargo test --release perf_5k -- --ignored --nocapture
```

CI runs clippy on `stable`, so a newer toolchain can flag lints your local one does not; fix them rather than
silencing them. The `msrv` job also runs clippy on the `rust-version` toolchain, and releases build with a pinned
toolchain.

## Guidelines

- Shell out to `gh`; no tokens, no async runtime, no new dependency unless a few lines can't do it.
- Every write goes through `act.rs` (an argv builder) so it gets the confirm popup and the command log.
  Never pass user text through a shell.
- Test fixtures must be synthetic (`octocat/hello-world` style). Do not commit real repository names, PR text or
  personal data.
- Colors come from `theme.rs`; icons from `Theme::ic`. Keep an ASCII fallback working.
- Add a small test for parsing or layout logic you touch. Layout tests use `TestBackend` and assert on key
  strings, not full screens.
- Keep comments to the non-obvious *why*.

## Releases

1. Bump `version` in `Cargo.toml` (and `Cargo.lock`, via `cargo build`) and add the release's section to `CHANGELOG.md` in a PR and merge it.
2. Tag the merge commit `vX.Y.Z` (or `vX.Y.Z-rc.1`, published as a pre-release) and push the tag.
3. The `Release` workflow builds `gh-pulse-<os>-<arch>` for macOS and Linux (arm64, amd64), writes `SHA256SUMS`,
   attests provenance and creates the GitHub release. It fails if the tag does not match the `Cargo.toml` version.

To rehearse without publishing, run the workflow manually (Actions, Release): it builds and uploads artifacts only
and never publishes. `gh extension install` needs the tagged release to exist, and the
extension repo needs the `gh-extension` topic to be discoverable.

## Reporting bugs

Include your `gh --version`, terminal and size, and the output of `L` (command log) if a fetch looked wrong.
