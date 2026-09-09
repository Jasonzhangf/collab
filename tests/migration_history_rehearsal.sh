#!/bin/sh

# Read-only copied-fixture rehearsal. These bounded inputs are not current
# source evidence and cannot admit S2. The only files opened by this harness
# are the local fixture JSON files and the repository schema. Values named
# source_path or raw_log inside a fixture are treated as strings; they are
# never dereferenced, and no live project root is contacted.
set -eu

script_dir=$(cd "$(dirname "$0")" && pwd -P)
repo_root=$(cd "$script_dir/.." && pwd -P)
fixture_root="$repo_root/docs/fixtures/governance-history-migration"
schema_path="$repo_root/docs/migration-v1-history-manifest.schema.json"

fail() {
    printf '%s\n' "migration-history rehearsal: FAIL: $*" >&2
    exit 1
}

for required_tool in jq jsonschema shasum; do
    command -v "$required_tool" >/dev/null 2>&1 || fail "missing required tool: $required_tool"
done

[ -f "$schema_path" ] || fail "missing local schema: $schema_path"

expected_records='{
  "collab": [
    {"source_record_id":".agent-collab/mailbox","mapping_class":"adapt","mapping_status":"planned","target_epoch":"unassigned-rehearsal-epoch","target_sequence":null,"target_entity_id":null,"raw_archive_ref":null,"exact_error":null,"first_failed_boundary":null},
    {"source_record_id":".agent-collab/server/events.jsonl","mapping_class":"unknown","mapping_status":"planned","target_epoch":"unassigned-rehearsal-epoch","target_sequence":null,"target_entity_id":null,"raw_archive_ref":null,"exact_error":"SOURCE_PROJECTION_MUTABLE:events_digest_changed_during_inspect","first_failed_boundary":"snapshot"},
    {"source_record_id":".agent-collab/server/journal.jsonl","mapping_class":"reset","mapping_status":"planned","target_epoch":"unassigned-rehearsal-epoch","target_sequence":null,"target_entity_id":null,"raw_archive_ref":null,"exact_error":"MULTIPLE_WRITERS:20_collab_serve_processes","first_failed_boundary":"writer-admission"}
  ],
  "appsdk": [
    {"source_record_id":".agent-collab/mailbox","mapping_class":"adapt","mapping_status":"planned","target_epoch":"unassigned-rehearsal-epoch","target_sequence":null,"target_entity_id":null,"raw_archive_ref":null,"exact_error":null,"first_failed_boundary":null},
    {"source_record_id":".agent-collab/server/events.jsonl","mapping_class":"unknown","mapping_status":"planned","target_epoch":"unassigned-rehearsal-epoch","target_sequence":null,"target_entity_id":null,"raw_archive_ref":null,"exact_error":"SOURCE_PROJECTION_MUTABLE:events_digest_changed_during_inspect","first_failed_boundary":"snapshot"},
    {"source_record_id":".agent-collab/server/journal.jsonl","mapping_class":"reset","mapping_status":"planned","target_epoch":"unassigned-rehearsal-epoch","target_sequence":null,"target_entity_id":null,"raw_archive_ref":null,"exact_error":"UNRESOLVED_GIT_CONFLICTS:5","first_failed_boundary":"candidate"}
  ],
  "routecodex": [
    {"source_record_id":".agent-collab/mailbox","mapping_class":"adapt","mapping_status":"planned","target_epoch":"unassigned-rehearsal-epoch","target_sequence":null,"target_entity_id":null,"raw_archive_ref":null,"exact_error":null,"first_failed_boundary":null},
    {"source_record_id":".agent-collab/server/events.jsonl","mapping_class":"unknown","mapping_status":"planned","target_epoch":"unassigned-rehearsal-epoch","target_sequence":null,"target_entity_id":null,"raw_archive_ref":null,"exact_error":"SOURCE_PROJECTION_MUTABLE:events_digest_changed_during_inspect","first_failed_boundary":"snapshot"},
    {"source_record_id":".agent-collab/server/journal.jsonl","mapping_class":"reset","mapping_status":"planned","target_epoch":"unassigned-rehearsal-epoch","target_sequence":null,"target_entity_id":null,"raw_archive_ref":null,"exact_error":"DIRTY_MIXED_V3_V4_CANDIDATE:status_entries=50","first_failed_boundary":"candidate"}
  ],
  "codexapp": [
    {"source_record_id":"/Users/fanzhang/.codex-communication/journal.jsonl","mapping_class":"unknown","mapping_status":"planned","target_epoch":"unassigned-rehearsal-epoch","target_sequence":null,"target_entity_id":null,"raw_archive_ref":null,"exact_error":"ENDPOINT_CONNECT_FAILED:connect ECONNREFUSED /Users/fanzhang/.codex-communication/sockets/commd.sock","first_failed_boundary":"native-endpoint-bootstrap"}
  ]
}'

check_source_binding_digest() {
    manifest_path=$1
    actual_digest=$(jq -j '[.records[] | {id: .source_record_id, digest: .source_record_digest}] | sort_by(.id)[] | .id, "\u0000", .digest, "\u0000"' "$manifest_path" | shasum -a 256 | cut -d' ' -f1)
    expected_digest=$(jq -r '.source_snapshot_digest | sub("^sha256:"; "")' "$manifest_path")
    [ "$actual_digest" = "$expected_digest" ] || fail "source_snapshot_digest mismatch: $manifest_path"
}

for project in collab appsdk routecodex codexapp; do
    manifest_path="$fixture_root/$project/manifest.json"
    rehearsal_path="$fixture_root/$project/rehearsal.json"
    [ -f "$manifest_path" ] || fail "missing manifest: $manifest_path"
    [ -f "$rehearsal_path" ] || fail "missing rehearsal: $rehearsal_path"

    case "$project" in
        collab|appsdk|routecodex)
            expected_capture='capture-03 (journal/events) and capture-11 (mailbox, 2026-09-09T19:56:01Z)'
            ;;
        codexapp)
            expected_capture='capture-03 and capture-09'
            ;;
    esac

    jsonschema -i "$manifest_path" "$schema_path" >/dev/null
    jq empty "$manifest_path" "$rehearsal_path"

    jq -e --arg project "$project" --argjson expected "$expected_records" '
        (.source_project_id == $project)
        and (.mapping_status == "planned")
        and (.target_epoch == "unassigned-rehearsal-epoch")
        and (.archive_ref == null)
        and (.archive_digest == null)
        and (.source_snapshot_digest | startswith("sha256:"))
        and (all(.records[];
            (.mapping_status == "planned")
            and (.target_epoch == "unassigned-rehearsal-epoch")
            and (.target_sequence == null)
            and (.target_entity_id == null)
            and (.agent_id == null)
            and (.runtime_id == null)
            and (.binding_id == null)
            and (.endpoint_generation == null)
            and (.appsdk_record_ref == null)
            and (.appsdk_record_digest == null)
            and (.raw_archive_ref == null)
        ))
        and ([.records[] | {
            source_record_id,
            mapping_class,
            mapping_status,
            target_epoch,
            target_sequence,
            target_entity_id,
            raw_archive_ref,
            exact_error,
            first_failed_boundary
        }] | sort_by(.source_record_id)) == ($expected[$project] | sort_by(.source_record_id))
    ' "$manifest_path" >/dev/null || fail "manifest invariants: $project"

    jq -e --slurpfile manifest "$manifest_path" '
        ($manifest[0]) as $m
        | ($m.records | map({key: .source_record_id, value: .source_record_digest}) | from_entries) as $expected
        | (.source_observation.inputs | map(select(.digest != null) | {key: .source_record_id, value: .digest}) | from_entries) as $actual
        | ($m.records | map(.source_record_id)) as $manifest_ids
        | (.source_observation.inputs | map(select(.digest != null) | .source_record_id)) as $input_ids
        | (.manifest == "manifest.json")
        and (.project_id == $m.source_project_id)
        and (($manifest_ids | unique | length) == ($manifest_ids | length))
        and (($input_ids | unique | length) == ($input_ids | length))
        and (($manifest_ids | sort) == ($input_ids | sort))
        and ($actual == $expected)
    ' "$rehearsal_path" >/dev/null || fail "manifest/rehearsal source linkage: $project"

    check_source_binding_digest "$manifest_path"

    case "$project" in
        collab)
            expected_negative_cases='[
              {"case_id":"changed-cwd","expected_mapping_class":"unknown","expected_boundary":"scope"},
              {"case_id":"duplicate-record-id","expected_mapping_class":"unknown","expected_boundary":"schema"},
              {"case_id":"dirty-candidate","expected_mapping_class":"reset","expected_boundary":"candidate"},
              {"case_id":"malformed-middle","expected_mapping_class":"unknown","expected_boundary":"schema"},
              {"case_id":"missing-owner","expected_mapping_class":"unknown","expected_boundary":"owner"},
              {"case_id":"multiple-writers","expected_mapping_class":"reset","expected_boundary":"writer-admission"},
              {"case_id":"source-digest-drift","expected_mapping_class":"unknown","expected_boundary":"snapshot"},
              {"case_id":"unknown-outcome","expected_mapping_class":"unknown","expected_boundary":"operation"}
            ]'
            ;;
        appsdk)
            expected_negative_cases='[
              {"case_id":"changed-cwd","expected_mapping_class":"unknown","expected_boundary":"scope"},
              {"case_id":"duplicate-record-id","expected_mapping_class":"unknown","expected_boundary":"schema"},
              {"case_id":"malformed-middle","expected_mapping_class":"unknown","expected_boundary":"schema"},
              {"case_id":"missing-owner","expected_mapping_class":"unknown","expected_boundary":"owner"},
              {"case_id":"mutable-mailbox-projection","expected_mapping_class":"adapt","expected_boundary":"projection-rebuild"},
              {"case_id":"source-digest-drift","expected_mapping_class":"unknown","expected_boundary":"snapshot"},
              {"case_id":"unknown-outcome","expected_mapping_class":"unknown","expected_boundary":"operation"},
              {"case_id":"unresolved-candidate-conflicts","expected_mapping_class":"reset","expected_boundary":"candidate"}
            ]'
            ;;
        routecodex)
            expected_negative_cases='[
              {"case_id":"changed-cwd","expected_mapping_class":"unknown","expected_boundary":"scope"},
              {"case_id":"dirty-mixed-v3-v4-candidate","expected_mapping_class":"reset","expected_boundary":"candidate"},
              {"case_id":"duplicate-record-id","expected_mapping_class":"unknown","expected_boundary":"schema"},
              {"case_id":"lost-workers-with-active-tasks","expected_mapping_class":"unknown","expected_boundary":"owner"},
              {"case_id":"malformed-middle","expected_mapping_class":"unknown","expected_boundary":"schema"},
              {"case_id":"missing-owner","expected_mapping_class":"unknown","expected_boundary":"owner"},
              {"case_id":"preserve-v3-v4-boundary","expected_mapping_class":"adapt","expected_boundary":"scope"},
              {"case_id":"source-digest-drift","expected_mapping_class":"unknown","expected_boundary":"snapshot"},
              {"case_id":"unknown-outcome","expected_mapping_class":"unknown","expected_boundary":"operation"}
            ]'
            ;;
        codexapp)
            expected_negative_cases='[
              {"case_id":"active-import-before-bootstrap","expected_mapping_class":"reset","expected_boundary":"active-import"},
              {"case_id":"duplicate-record-id","expected_mapping_class":"unknown","expected_boundary":"schema"},
              {"case_id":"endpoint-listener-absent","expected_mapping_class":"unknown","expected_boundary":"native-endpoint-bootstrap"},
              {"case_id":"malformed-middle","expected_mapping_class":"unknown","expected_boundary":"schema"},
              {"case_id":"mock-adapter-only","expected_mapping_class":"unknown","expected_boundary":"native-capability-negotiation"},
              {"case_id":"source-digest-drift","expected_mapping_class":"unknown","expected_boundary":"snapshot"},
              {"case_id":"unknown-outcome","expected_mapping_class":"unknown","expected_boundary":"operation"}
            ]'
            ;;
    esac

    jq -e --arg capture "$expected_capture" --argjson expected "$expected_negative_cases" '
        (.mode == "no_write")
        and (.expected.manifest_mapping_status == "planned")
        and (.expected.archive_ref == null)
        and (.expected.archive_digest == null)
        and (.expected.target_mappings_created == false)
        and (.expected.source_mutations == false)
        and ((.expected.active_import // false) == false)
        and (.source_observation.raw_log | type == "string")
        and (.source_observation.capture | contains($capture))
        and ([.cases[] | {case_id, expected_mapping_class, expected_boundary}] | sort_by(.case_id)) == ($expected | sort_by(.case_id))
    ' "$rehearsal_path" >/dev/null || fail "no-write/negative-boundary invariants: $project"

    printf '%s\n' "migration-history rehearsal: PASS: $project"
done

printf '%s\n' 'migration-history rehearsal: PASS: four copied fixtures validated; no live source path was opened'
