# Performance budgets

The performance job measures the same standard and Alpine OCI artifacts that runtime CI tests, using the default SQLite backend. It checks image size, readiness, idle CPU and memory, workload CPU and memory, verified operation latency, throughput, and memory retained after load. Failed performance checks prevent image publication. PostgreSQL integration follows the approval rule in [testing conventions](testing.md#database-contracts).

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
| Standard image | 50 MB compressed, 125 MB unpacked |
| Alpine image | 10 MB compressed, 20 MB unpacked |
| Readiness | 500 ms, excluding image pull |
| Idle memory and CPU | 50 MB, 1% of one CPU |
| Busy memory | 128 MB |
| Retained memory growth | 80 MiB above seeded idle memory |
| Read operations | p95 <= 500 ms, CPU <= 3 ms/op, throughput >= 300 op/s |
| Verified write operations | p95 <= 500 ms, CPU <= 12 ms/op, throughput >= 70 op/s |
| Rejection operations | p95 <= 500 ms, CPU <= 4 ms/op, throughput >= 150 op/s |

In addition, CPU cost cannot exceed twice its fixed reference allowance. CPU allowances are shared across CI and local profiles because runner hardware varies. Standard read/write/rejection allowances are 1/5/2 ms per operation; Alpine allowances are 2/7/2 ms. The absolute CPU caps also apply. Throughput uses only the absolute floors in the table; profile-specific throughput references remain calibration data. References never advance automatically with a PR or a moving image tag. Latency and busy memory use the selected absolute caps.

MB means 1,000,000 bytes; MiB means 1,048,576 bytes. Policy values use bytes.

Initial local calibration used five repetitions per variant. The first successful PR run supplied three hosted-runner repetitions per variant. Across those initial runs, standard and Alpine images measured 31.9 MB and 5.7 MB compressed, and 81.6 MB and 12.5 MB unpacked. Maximum readiness was 321 ms, idle memory 12.7 MB, busy memory 77.5 MB, and retained growth 57.8 MB. The initial size and memory caps allowed at least 40% headroom. The initial 100 ms latency cap allowed 88% above the worst write p95 of 53.3 ms. These measurements describe the filesystem backend, not current database headroom. The policy preserves initial calibration metadata and records CPU allowance revisions separately.

Workload p95 latency has a 500 ms cap on every repetition. Hosted-runner PR #14 run `38052106658` measured standard write p95 values of 112.2, 14.4, and 38.6 ms with CPU costs of 1.68 to 1.72 ms per operation. The latency cap allows elapsed-time variation; CPU cost and absolute throughput floors remain separate checks. Shared-host contention can still exceed the cap, so a failure requires diagnosis rather than proving a code defect.

SQLite PR #15 run `38053261871` measured peak idle memory of 20.71 MB. The 50 MB idle cap allows feature growth while keeping the 128 MB busy-memory cap separate.

The SQLite implementation measured local maximum CPU costs of 0.996/4.176/1.558 ms for standard reads/writes/rejections, and 2.540/6.569/1.943 ms for Alpine. PR #15 run `38050250307` measured CI maxima of 0.848/2.874/0.910 ms and 1.674/4.636/1.594 ms respectively. Rounded CPU allowances accommodate those costs without treating the original hosted runner as fixed hardware. Alpine reads still have only 18% headroom under the absolute 3 ms cap. The database read path adds SQL retrieval and content integrity validation; these controls remain required.

Compressed size covers unique runtime layers for Linux amd64, excluding SBOMs, attestations, and archive overhead. Memory covers the whole cgroup, including filesystem cache. Busy memory uses the cumulative cgroup peak, so an earlier workload's peak remains visible. Recovery bounds retained growth; it does not prove absence of leaks. Latency is client-side verified operation latency under a closed-loop workload, not a claim about maximum sustainable traffic.

The service runs on an internal container network. Podman exposes its measured port on localhost; Docker measurements use the container's bridge address because Docker disables port publishing on internal networks. CI verifies the source revision label and the loaded image's configuration digest against the OCI artifact.

## Owner control and activation

[CODEOWNERS](../.github/CODEOWNERS) assigns the policy, runner, container definitions, measurement tests, and every workflow to `andypenno`. Review covers changes to what gets measured as well as changes to the numbers. A limit increase requires new reference-versus-reference measurements, raw evidence, and owner approval; it must not be used to conceal an unexplained regression.

CODEOWNERS alone does not block merging. When repository protection is activated, require owner review, dismiss stale approvals, require the performance and existing correctness checks, require results for the current merge tree, prohibit force pushes, and remove ordinary bypass rights. Protect both `dev` and `main`. GitHub currently requires Pro or public visibility to enable these settings for this private repository.

Give agents contributor credentials, not the owner's administrator login. GitHub does not allow authors to approve their own PRs. A separate contributor identity is needed for owner approval to be an effective restriction on agent-created PRs.

The accepted design relies on owner review of gate changes. It does not provide an independent GitHub App check identity. Until protection and credential separation are configured, CI detects failures but merging remains possible. Keep that distinction when assessing the PR rehearsal.
