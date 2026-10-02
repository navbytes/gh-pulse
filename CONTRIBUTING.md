# Contributing

Thanks for helping. gh-pulse is small on purpose: a thin TUI over `gh`. Prefer the shortest change that works.

## Setup

```sh
git clone https://github.com/navbytes/gh-pulse && cd gh-pulse
cargo run -- -R owner/repo          # needs `gh auth login`
```

Rust 1.88+ (edition 2024). Read [docs/architecture.md](docs/architecture.md) first; it is short.

## Checks before a PR

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --features syntax -- -D warnings
cargo test
cargo test --features syntax
```

Optional live tests (they call `gh`, read-only) take their target from the environment:

```sh
GH_PULSE_REPO=owner/repo cargo test -- --ignored --nocapture
GH_PULSE_HUGE_PR=owner/repo#123 cargo test -- --ignored --nocapture
cargo test --release perf_5k -- --ignored --nocapture
```

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

## Reporting bugs

Include your `gh --version`, terminal and size, and the output of `L` (command log) if a fetch looked wrong.
