# Cutting a release

Every release is a Git tag. The `release` workflow (`.github/workflows/release.yml`) builds that tag, writes the notes with `git-cliff` and publishes the GitHub Release. Nothing bumps the version for you, so a release is always: bump, merge, tag.

All crates inherit one version from `[workspace.package]` in the root `Cargo.toml`. The workflow checks the tag against it.

## Choosing the version

`git-cliff` groups the changelog from PR titles (see [writing-prs.md](writing-prs.md)), but you choose the number:

- **Patch** (`0.1.0` → `0.1.1`): only `fix`, `perf`, `docs` or `chore` changes.
- **Minor** (`0.1.1` → `0.2.0`): at least one `feat`.
- **Major** (`0.x` → `1.0.0`, `1.x` → `2.0.0`): any `!` title or `BREAKING CHANGE:` footer.

`git cliff --bumped-version` prints a suggestion from the commits since the last tag.

## Release from `main`

1. Open a PR that sets `version` in `[workspace.package]` and updates `Cargo.lock`. Title it `chore(release): prepare vX.Y.Z`.
2. After it merges, tag the merge commit and push the tag:

   ```bash
   git switch main && git pull
   git tag vX.Y.Z
   git push origin vX.Y.Z
   ```

3. The tag push publishes the release.

To inspect it before it goes public, run the `release` workflow by hand (**Actions → release → Run workflow**) with the tag. It defaults to a **draft**; publish it from the Releases page when the notes and binaries look right. The tag must already exist, because the workflow builds that tag's commit, not the branch you run it from.

A tag with a suffix, such as `v0.2.0-rc.1`, needs `0.2.0-rc.1` as the Cargo version. It is published as a prerelease, so the installers don't pick it up as the latest release.

## Hotfix an older release

Use a release branch only when `main` has already moved on and you must patch an older line.

1. Create `release/vX.Y.x` from the last tag of that line, for example `git switch -c release/v0.1.x v0.1.0`, and push it. The `x` is literal; one branch serves every `v0.1.*` release.
2. Land the fix on it through a PR that targets the release branch.
   - If the bug also exists on `main`, fix it on `main` first, then **cherry-pick** that commit onto the release branch (`git cherry-pick <sha>`). This copies only that fix, not the rest of `main`.
   - If the bug only exists in the old release, fix it directly on the release branch.
3. Bump the version to `X.Y.(Z+1)`, merge, then tag and push as above, from the release branch.
