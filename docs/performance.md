# Performance budgets

Automatic CI measures the same standard and Alpine OCI artifacts that runtime CI tests, using the default SQLite backend. PostgreSQL and S3 measurements are optional, explicitly selected manual checks; CI does not provision or connect to those backends. The automatic gate checks image size, readiness, idle CPU and memory, workload CPU and memory, verified operation latency, throughput, and memory retained after load. Failed automatic checks prevent image publication. Optional backend runs enforce their selected targets locally and do not gate CI publication. PostgreSQL integration follows the approval rule in [testing conventions](testing.md#database-contracts).

## Run locally

Use Linux amd64 with cgroup v2, Python 3.11 or newer, Podman, and Skopeo. Docker also works; use `docker build` and copy the OCI archive into the Docker daemon before measurement.

Run from the repository root:

```sh
mkdir -p test-output
python3 tests/performance/test_budgets.py
python3 tests/performance/test_benchmark.py
podman build --ignorefile containers/Containerfile.dockerignore -f containers/Containerfile -t localhost/slopdrive:standard .
podman build --ignorefile containers/Containerfile.alpine.dockerignore -f containers/Containerfile.alpine -t localhost/slopdrive:alpine .
for variant in standard alpine; do
  podman unshare skopeo copy --dest-compress --dest-compress-format gzip "containers-storage:localhost/slopdrive:$variant" "oci-archive:$PWD/test-output/$variant.tar"
  python3 tests/performance/benchmark.py --engine podman --image "localhost/slopdrive:$variant" --archive "test-output/$variant.tar" --variant "$variant" --policy tests/performance/policy.json --output "test-output/performance-$variant.json"
done
```

Build both variants before measuring either one. Do not compile, run another benchmark, or run concurrent variants on the same host during measurement. The local profile is calibrated for its recorded host; slower machines can use `--collect-only` for diagnosis. Diagnostic reports cannot pass the evaluator or substitute for CI results.

Each invocation has a 20-minute timeout. Ctrl+C or SIGTERM cleans up its containers and volumes and preserves completed repetitions in the report. The report records its PID, image/config digests, policy and harness hashes, host, raw latencies, memory samples, and CPU counters. CI retains reports for seven days.

## Optional PostgreSQL and S3 checks

Follow the external-fixture approval rule in [testing conventions](testing.md). Provision a dedicated PostgreSQL database and a private Garage bucket outside the benchmark. The runner starts only SlopDrive containers. Supply the existing `DATABASE_TEST_POSTGRES_URL` and `S3_TEST_ENDPOINT`, `S3_TEST_BUCKET`, `S3_TEST_REGION`, `S3_TEST_ACCESS_KEY_ID`, `S3_TEST_SECRET_ACCESS_KEY` environment settings. Plaintext Garage fixtures also need `S3_TEST_ALLOW_HTTP=true`.

Attach both backends to a dedicated internal container network and set `PERFORMANCE_NETWORK` to its name. Endpoint hostnames or addresses must be reachable inside that network; localhost would refer to the SlopDrive container itself. The runner rejects a network that permits external traffic. Do not use production databases or buckets.

After building and exporting both images as above, run the external modes serially:

```sh
: "${PERFORMANCE_NETWORK:?Set the dedicated internal backend network}"
for variant in standard alpine; do
  for mode in postgres-database sqlite-s3 postgres-s3; do
    python3 tests/performance/benchmark.py --engine podman --image "localhost/slopdrive:$variant" --archive "test-output/$variant.tar" --variant "$variant" --mode "$mode" --backend-network "$PERFORMANCE_NETWORK" --policy tests/performance/policy.json --output "test-output/performance-$variant-$mode.json"
  done
done
```

The optional backend targets below apply to both images. Startup may take at most 1 second. The common 500 ms p95, 50 MB idle memory, 128 MB busy memory, 80 MiB recovery-growth and image-size limits still apply. Rejections retain the common 4 ms CPU and 150 op/s limits.

| Storage mode | Read CPU max | Write CPU max | Read throughput min | Verified write throughput min |
| --- | --- | --- | --- | --- |
| PostgreSQL/database | 6 ms/op | 8 ms/op | 150 op/s | 80 op/s |
| SQLite/S3 | 5 ms/op | 12 ms/op | 120 op/s | 40 op/s |
| PostgreSQL/S3 | 6 ms/op | 10 ms/op | 100 op/s | 50 op/s |

These are coarse regression limits. They were calibrated with three repetitions on both images, with same-batch SQLite baselines before and after the external modes. The maximum shift in median baseline CPU or throughput was 5.3%. The policy records backend versions, resource limits, image and harness identities, host information and measured extrema. Raw results remain generated evidence, not source documentation. The default SQLite targets were not loosened for this reference host.

Readiness timing starts after key creation initializes the sample schema. Backend service boot and data-migration time are outside that metric.

Each repetition uses a fresh `perf_<random>` PostgreSQL schema and `performance/<random>` S3 prefix. Key management and the server receive identical settings through a temporary file with mode 0600. Reports identify the storage mode without recording credentials. The runner removes its SlopDrive container, data volume and settings file, and preserves the supplied network and backend services. Dispose of the dedicated database and bucket afterward; the runner does not drop schemas or delete objects from supplied resources.

Backend targets use the same-host private-network reference deployment with one CPU and 512 MiB for each backend, PostgreSQL durability defaults, and Garage's consistent mode with one storage replica. PostgreSQL test connections disable TLS. These numbers do not define WAN, replicated-storage or TLS performance. TLS correctness has its own [tests](testing.md). The benchmark verifies SlopDrive's resource limits; fixture owners must enforce the backend limits when comparing results.

Backends remain running across repetitions; their caches, compression and deduplication are part of the reference deployment. These targets describe the verified synthetic workload after seeding, not cold backend startup or maximum sustainable production traffic.

Targets cover one SlopDrive container. The [replica checks](testing.md#s3-contracts-and-independent-processes) exercise distributed correctness separately. Only SlopDrive's cgroup contributes to the CPU and memory requirements. Database and object-store resource consumption is separate. Use `--collect-only` for calibration; diagnostic reports cannot pass the evaluator or substitute for an enforced run. All modes retain body, hash, history and restart checks. Changing mode never substitutes a cheaper workload.

## Workload and default SQLite limits

The approved policy is [tests/performance/policy.json](../tests/performance/policy.json). Both variants get one CPU, 256 MiB of memory, no swap, and three fresh-storage repetitions. Each workload performs 600 verified operations at concurrency four, with five-second idle and recovery intervals.

Reads alternate current and pinned versions of 1 KiB, 64 KiB, and 1 MiB pages. Writes alternate HTTP uploads, MCP uploads, and MCP updates. Each timed write includes retrieval and history verification; written versions must also survive a restart. Histories start with 100 versions. Rejection workloads cover invalid authentication, missing pages, and oversized MCP content.

| Absolute requirement | Limit |
|---|---|
| Standard image | 50 MB compressed, 125 MB unpacked |
| Alpine image | 15 MB compressed, 30 MB unpacked |
| Readiness | 500 ms, excluding image pull |
| Idle memory and CPU | 50 MB, 1% of one CPU |
| Busy memory | 128 MB |
| Retained memory growth | 80 MiB above seeded idle memory |
| Read operations | p95 <= 500 ms, CPU <= 3 ms/op, throughput >= 300 op/s |
| Verified write operations | p95 <= 500 ms, CPU <= 12 ms/op, throughput >= 70 op/s |
| Rejection operations | p95 <= 500 ms, CPU <= 4 ms/op, throughput >= 150 op/s |

For the default SQLite/database mode, CPU cost also cannot exceed twice its fixed reference allowance. CPU allowances are shared across CI and local profiles because runner hardware varies. Standard read/write/rejection allowances are 1/5/2 ms per operation; Alpine allowances are 2/7/2 ms. The absolute CPU caps also apply. Throughput uses only the absolute floors in the table; profile-specific throughput references remain calibration data. References never advance automatically with a PR or a moving image tag. Latency and busy memory use the selected absolute caps.

MB means 1,000,000 bytes; MiB means 1,048,576 bytes. Policy values use bytes.

For 1.0.0, the owner approved Alpine caps of 15 MB compressed and 30 MB unpacked. The preparation images measured 9.42 MB and 19.63 MB respectively. The caps leave room for dependency updates while still failing CI if image size exceeds them; runtime limits are unchanged.

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
