# Release strategy

Changes follow personal branch -> PR into `dev` -> PR into `main`. Successful branch builds publish moving container channels. A versioned release selects a tested commit already merged into `main`; merging alone does not declare that commit stable.

## Image channels

All images use `ghcr.io/andypenno/slopbox` and target Linux amd64.

| Source | Standard tag | Alpine tag |
|---|---|---|
| Latest successful `dev` build | `dev` | `dev-alpine` |
| Latest successful `main` build | `latest` | `latest-alpine` |
| Highest versioned release | `stable` | `stable-alpine` |
| Release tag `vX.Y.Z` | `X.Y.Z` | `X.Y.Z-alpine` |
| Tested commit | `sha-<full-sha>` | `sha-<full-sha>-alpine` |

Branch channels can contain unreleased changes. Operators should pin an exact version or digest. Older maintenance releases do not move `stable` backwards, and version promotion never changes `latest`. Existing `edge` images remain available but receive no further updates.

Version and commit tags are immutable. A repeated branch publication reuses the existing image for that source revision and package version. Changing base images requires a new commit; shipping that change requires a new patch release. GitHub does not update running deployments when a channel changes.

## Builds and checks

The reusable [container workflow](../.github/workflows/containers.yml) handles both branches and PR checks. Container definitions, Compose, and the release helper live in [containers/](../containers/). Every build uses the repository root as its context.

The standard variant uses Debian slim and glibc. Alpine builds its own musl binary and uses an Alpine runtime. Both keep UID/GID 10001, `/data`, port 3000, the CLI, and the same environment variables. Keep both Rust builder versions aligned with CI.

CI runs formatting, Clippy with warnings denied, locked Rust tests, homepage checks, release-policy checks, and [performance budgets](performance.md). It exports each image once as an OCI archive with provenance and an SBOM. Docker and Podman exercise those archives for HTTP, MCP, authentication, non-root execution, persistence, and switching between variants. Only after every check passes can CI copy the archives to GHCR while preserving their digests.

Publication is serialized across branch and version workflows. Before moving a branch channel, the helper checks that the source commit is still the branch head. PRs and merge queues run the same build and runtime checks without publishing credentials. Failed checks publish no images. Keep generated logs in gitignored `test-output/`; image artifacts contain no service data or API keys.

## Choose a stable release

Use `X.Y.Z` semantic versions. Patches fix compatible bugs, minor versions add compatible functionality, and major versions permit intentional breaking changes. Before `1.0.0`, minor versions may break compatibility; patches must remain compatible. Document breaking changes and storage compatibility in release notes.

1. Prepare the version and release notes through the PR process. Update `Cargo.toml` and the root package entry in `Cargo.lock` before choosing the commit.
2. Wait for that main commit's standard and Alpine images to finish publication.
3. Create a protected `vX.Y.Z` tag on that commit.
4. The [release workflow](../.github/workflows/release.yml) checks the version against `Cargo.toml` and confirms that the commit belongs to `main`.
5. It promotes the existing tested digests to `X.Y.Z` and `X.Y.Z-alpine`, then advances both stable channels if appropriate. It never rebuilds a release.

Missing images, source/version mismatches, and attempts to replace an existing version with different bytes fail promotion. Retrying the same promotion is safe. The registry cannot move two tags atomically; a failed copy can leave a partial pair, so retry the workflow before announcing the release.

Back up data before upgrading. Switching variants within one release preserves the data format. Rolling back across releases also requires compatible stored data; an older binary is not a data migration tool.

## Repository and package settings

Require PRs into `dev` and `main`, the Rust checks, both image builds, and both container-engine tests. The workflow also handles `merge_group` for merge queues. Restrict release-tag creation and deletion through a ruleset; never move an existing release tag. Keep actions pinned to reviewed SHAs and checkout credentials disabled. Dependabot tracks actions, Rust dependencies, and container base images.

Publishing uses job-scoped `GITHUB_TOKEN` permissions and is limited to `andypenno/SlopBox`. No personal access token is needed for CI publication. Repository protection and package visibility require administrator settings; editing the workflow does not apply them.

GHCR visibility is independent of repository visibility. Make the package public when anonymous pulls are intended and verify pulls of both variants before announcing public installation. If the package already exists, grant this repository Actions access. Private-package users need registry authentication with `read:packages` access.

Before public release, inspect tracked files and reachable history for credentials and private addresses. Rotate exposed credentials and enable secret scanning and push protection where available.
