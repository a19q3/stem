# Proposal-to-quorum tracing

Set `consensus_trace_path` in each replica configuration to a
distinct file that does not already exist. The local trace records
the epoch, view, parent view and Header ID. A bounded queue feeds
the diagnostic writer and caps its output at 262,144 events per
replica. The status response reports trace and per-kind wire
counters when tracing is enabled.

Transport events cover queue admission, dequeue, kernel write
and complete receipt. Consensus events cover candidate execution,
durable voting, counted shares and durable certificates.

Use the existing payment fixture and take a copy of the same
authenticated funded state for each arm. Verify the signed archive
independently before interpreting the timings. Use the same
workload, validator and driver CPU budgets and storage configuration
throughout a comparison. [Distributed fixture driver](../storage/external/drive.py)

```sh
python3 research/consensus-transport/analyze.py \
  --traces "$TRACES" --report "$REPORT"
```

The analyzer accepts four files named `trace-0.jsonl` through
`trace-3.jsonl`. It times proposal-to-QC on the Leader's monotonic
clock. When a single write can explain each closing vote and its
proposal receipt, it subtracts the Follower's local read-to-send
duration from the Leader's round trip. The residual includes
kernel transfer and link delay. The output counts ambiguous
pairs and uses one common wall-clock scoring window.

Set `proposal_retry_interval_ms` to select a minimum interval
between proactive full proposal sends for the same epoch and
Point. Omit it to use the existing announce schedule. The
initial proposal and explicit GET repair follow their own paths.
