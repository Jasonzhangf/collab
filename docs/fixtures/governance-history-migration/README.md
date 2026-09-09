# Governance-history migration rehearsal fixtures

This directory contains four copied-input descriptors for the S1 inspect and
S3 no-write rehearsal stages described by
`docs/goals/collab-v1-governance-history-migration-plan.md`. The fixtures do
not contain bytes copied from a live `.agent-collab` directory, and they do
not authorize an archive, reset, replay, identity rebind, daemon operation or
active import.

Each project directory has two files:

- `manifest.json` is an S1 draft validated against
  `docs/migration-v1-history-manifest.schema.json`.
- `rehearsal.json` is a declarative no-write fixture descriptor. It binds the
  manifest to the source projection observations and lists the negative cases
  that a later copied-input harness must exercise.

Together with this README and the evidence document, the two files in each of
the four project directories make up the candidate's 10 docs-only paths.

The manifests intentionally use `mapping_status=planned`. Every record keeps
`mapping_status=planned`, leaves target sequence/entity and archive fields
null, and carries its observed `mapping_class`. The shared
`target_epoch=unassigned-rehearsal-epoch` value is a sentinel required by the
schema; it is not an allocated target epoch and is not a migration admission
record. A future S2 operation must allocate a fresh epoch and write a new
manifest after an immutable archive and operator-controlled lease exist.

`source_snapshot_digest` is a reproducible fixture binding, not an archive
digest. It is the SHA-256 of the UTF-8 byte sequence formed from the manifest
records sorted by `source_record_id`, with each field followed by a NUL byte:

```text
source_record_id NUL source_record_digest NUL ... final NUL
```

The journal and event per-record digests are the exact SHA-256 values recorded
by the repair capture at
`/private/tmp/governance-history-live-refresh-20260909-repair-raw.log`,
`capture-03`, lines 37-53. The mailbox per-record digests come from repair
`capture-11` at `2026-09-09T19:56:01Z`, lines 835-846. The source Git identity
fields come from
`capture-02`, lines 8-36. The codexapp endpoint error is preserved from
`capture-09`, lines 783-800. Those source projections were mutable at capture
time; the fixture descriptors therefore record the observation path and
counts but make no claim that the files are an immutable source snapshot.

The `exact_error` values on reset/unknown records preserve the observed
blocking fact or the contract boundary that a rehearsal must assert. They do
not turn a classification into a completed reset. In particular, the
codexapp fixture keeps Git fields explicitly null because that source has no
Git checkout.

The copied-input harness should assert all of the following for every
manifest:

1. JSON Schema validation passes with no additional properties.
2. The top-level and record statuses remain `planned`.
3. Every record's target sequence and target entity are absent from the live
   state and represented as JSON `null` in the fixture.
4. No archive directory, journal, mailbox, identity, task, daemon, socket or
   target projection is created or changed.
5. The negative cases retain the named first failed boundary and never
   synthesize an owner, outcome, runtime, binding or endpoint generation.
