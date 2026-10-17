# Load testing

Use synthetic data and a dedicated disposable environment. Define the dataset size, name distribution, concurrent users, read/write mix and acceptance thresholds before interpreting results.

Measure throughput, p95/p99 latency, errors, retries, CPU, memory, connection waits and matching candidate counts. Check database/audit/outbox totals and candidate/evidence correctness as well as response status. Compare cold and warm state and include concurrent updates.

The infrastructure load regression lives in `crates/infrastructure/tests/matching_load.rs`; it requires its documented PostgreSQL test environment. Workstation-specific packaged HTTP profiling harnesses and dated measurements are retained locally and excluded from Git tracking. They are not a portable public load-test command or an approved capacity benchmark.

No production SLA follows from test counts. See [Beta limits](../../docs/BETA.md).
