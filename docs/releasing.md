# Releasing

A release is a `vX.Y.Z` tag on `main` whose version matches `Cargo.toml`. The **Release** workflow builds the four
binaries (macOS arm64 and Intel, Linux arm64 and x86_64), writes `SHA256SUMS`, attests their provenance and publishes
the GitHub release with generated notes.

## 1. Prepare

Open and merge a PR that sets `version` in `Cargo.toml` (and `Cargo.lock`) and moves `[Unreleased]` in `CHANGELOG.md`
under the new version.

## 2. Publish, from the GitHub website

1. **Actions** > **Release** > **Run workflow**.
2. Pick the `main` branch and type the version, `0.5.0` (a leading `v` is fine).
3. The workflow checks that the version matches `Cargo.toml` on `main`, that the tag does not exist yet, and that you
   ran it from `main`; then it builds the binaries.
4. It stops at **approve release** and waits for the `release` environment's reviewer (below). Approve it in the
   run's page or from the GitHub mobile app.
5. After approval it attests the binaries, creates the tag at the validated commit and publishes the release.

Leaving the version empty only builds the binaries and uploads them as workflow artifacts: a dry run that publishes
nothing and creates no tag.

## 2b. Publish, from a terminal

```sh
git fetch origin && git tag v0.5.0 <sha-on-main> && git push origin v0.5.0
```

The tag push runs the same workflow with the same checks, and the same approval.

## One-time setup: the approval gate

In the repository, **Settings** > **Environments** > **New environment** > name it `release`, tick **Required
reviewers** and add yourself (optionally untick **Allow administrators to bypass** and restrict **Deployment
branches and tags** to `main` and `v*`). Until the environment has a required reviewer the publish step runs without
asking, so set this up before the first release from the website.

If a tag ruleset restricts who can create `v*` tags, allow the `github-actions` app (Settings > Rules), since the
workflow creates the tag with the workflow's own token. A tag made that way does not start a second run.

## Notes

- A release started from the website is attested against `refs/heads/main` at the release commit; a tag pushed by hand
  is attested against the tag. The binaries are built the same way.
- Nothing publishes if a check fails: a wrong version, a version already tagged, a run that was not started from
  `main`, or a commit that is not on `main`.
