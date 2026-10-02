# Performance budgets

The performance job measures the same standard and Alpine OCI artifacts that runtime CI tests. It checks image size, readiness, idle CPU and memory, workload CPU and memory, verified operation latency, throughput, and memory retained after load. Failed performance checks prevent image publication.

## Run locally

Use Linux amd64 with cgroup v2, Python 3.11 or newer, Podman, and Skopeo. Docker also works; use `docker build` and copy the OCI archive into the Docker daemon before measurement.

Run from the repository root:

```sh
mkdir -p test-output
python3 tests/performance.py
python3 tests/benchmark.py
podman build --ignorefile containers/Containerfile.dockerignore -f containers/Containerfile -t localhost/slopbox:standard .
podman build --ignorefile containers/Containerfile.alpine.dockerignore -f containers/Containerfile.alpine -t localhost/slopbox:alpine .
for variant in standard alpine; do
  podman unshare skopeo copy --dest-compress --dest-compress-format gzip "containers-storage:localhost/slopbox:$variant" "oci-archive:$PWD/test-output/$variant.tar"
  python3 containers/benchmark.py --engine podman --image "localhost/slopbox:$variant" --archive "test-output/$variant.tar" --variant "$variant" --policy containers/performance-policy.json --output "test-output/performance-$variant.json"
done
```

Build both variants before measuring either one. Do not compile, run another benchmark, or run concurrent variants on the same host during measurement. The local profile is calibrated for its recorded host; slower machines can use `--collect-only` for diagnosis. Diagnostic reports cannot pass the evaluator or substitute for CI results.

Each invocation has a 20-minute timeout. Ctrl+C or SIGTERM cleans up its containers and volumes and preserves completed repetitions in the report. The report records its PID, image/config digests, policy and harness hashes, host, raw latencies, memory samples, and CPU counters. CI retains reports for seven days.

## Workload and limits

The approved policy is [containers/performance-policy.json](../containers/performance-policy.json). Both variants get one CPU, 256 MiB of memory, no swap, and three fresh-storage repetitions. Each workload performs 600 verified operations at concurrency four, with five-second idle and recovery intervals.

Reads alternate current and pinned versions of 1 KiB, 64 KiB, and 1 MiB pages. Writes alternate HTTP uploads, MCP uploads, and MCP updates. Each timed write includes retrieval and history verification; written versions must also survive a restart. Histories start with 100 versions. Rejection workloads cover invalid authentication, missing pages, and oversized MCP content.

| Absolute requirement | Limit |
|---|---|
| Standard image | 50 MB compressed, 100 MiB unpacked |
| Alpine image | 10 MB compressed, 16 MiB unpacked |
| Readiness | 2 seconds, excluding image pull |
| Idle memory and CPU | 32 MiB, 1% of one CPU |
| Busy memory | 128 MB |
| Retained memory growth | 80 MiB above seeded idle memory |
| Read operations | p95 <= 100 ms, CPU <= 3 ms/op, throughput >= 300 op/s |
| Verified write operations | p95 <= 100 ms, CPU <= 12 ms/op, throughput >= 70 op/s |
| Rejection operations | p95 <= 100 ms, CPU <= 4 ms/op, throughput >= 150 op/s |

In addition, CPU cost cannot exceed twice the fixed profile/variant reference, and throughput cannot fall below half its reference. CI and local references are separate. References never advance automatically with a PR or a moving image tag. Latency and busy memory use the selected absolute caps.

MB means 1,000,000 bytes; MiB means 1,048,576 bytes. Policy values use bytes.

Initial local calibration used five repetitions per variant. Compressed runtime layers measured about 30.4 MiB for standard and 5.4 MiB for Alpine. Write p95 maxima were about 25 ms and 45 ms respectively. The first successful PR run supplied three separate hosted-runner repetitions per variant. The policy records the reference commit, image digests, hosts, run ID, and median measurements.

Compressed size covers unique runtime layers for Linux amd64, excluding SBOMs, attestations, and archive overhead. Memory covers the whole cgroup, including filesystem cache. Busy memory uses the cumulative cgroup peak, so an earlier workload's peak remains visible. Recovery bounds retained growth; it does not prove absence of leaks. Latency is client-side verified operation latency under a closed-loop workload, not a claim about maximum sustainable traffic.

The service runs on an internal container network with only its measured port published on localhost. CI verifies the source revision label and the loaded image's configuration digest against the OCI artifact.

## Owner control and activation

[CODEOWNERS](../.github/CODEOWNERS) assigns the policy, runner, container definitions, measurement tests, and every workflow to `andypenno`. Review covers changes to what gets measured as well as changes to the numbers. A limit increase requires new reference-versus-reference measurements, raw evidence, and owner approval; it must not be used to conceal an unexplained regression.

CODEOWNERS alone does not block merging. When repository protection is activated, require owner review, dismiss stale approvals, require the performance and existing correctness checks, require results for the current merge tree, prohibit force pushes, and remove ordinary bypass rights. Protect both `dev` and `main`. GitHub currently requires Pro or public visibility to enable these settings for this private repository.

Give agents contributor credentials, not the owner's administrator login. GitHub does not allow authors to approve their own PRs. A separate contributor identity is needed for owner approval to be an effective restriction on agent-created PRs.

The accepted design relies on owner review of gate changes. It does not provide an independent GitHub App check identity. Until protection and credential separation are configured, CI detects failures but merging remains possible. Keep that distinction when assessing the PR rehearsal.
