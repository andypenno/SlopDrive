# CI and releases

Pull requests run formatting, Clippy with warnings denied, locked tests, and a container build with CLI and non-root smoke checks. Fork pull requests receive read-only permissions and no publishing credentials. Keep checkout credentials disabled and actions pinned to reviewed commit SHAs. Dependabot proposes action, Rust dependency, and base image updates.

Successful pushes to `master` publish `ghcr.io/andypenno/slopbox:edge` and a full commit SHA tag. Version tags such as `v0.1.0` publish `:0.1.0` and a SHA tag after the same checks. Use full semantic version tags and never move a released tag. The image targets Linux amd64 and includes provenance and an SBOM. Keep the workflow Rust toolchain aligned with the Containerfile build image.

Publishing uses the job-scoped `GITHUB_TOKEN` with `packages: write`. No personal access token or deployment secret is needed. Publishing is restricted to this repository; forks can run checks without publishing to its package. Container publishing is the delivery step. Operators deploy a chosen image digest using the setup instructions and their own reverse proxy, data volume, and keys.

Before public release, inspect tracked files and reachable history for credentials and private addresses. Rotate any credential that reached a remote. Enable GitHub secret scanning and push protection where available. Require the Rust and container checks in branch protection and restrict release tag creation through a repository ruleset. These settings require repository administrator access.

GHCR packages have separate visibility settings. After the first publish, make the package public in its settings if anonymous pulls are intended. If a package already exists, grant this repository Actions access before publishing. Repository visibility alone does not make the package public.

Keep generated validation logs and audit reports in gitignored `test-output/`. Never upload service data or API keys as workflow artifacts.
