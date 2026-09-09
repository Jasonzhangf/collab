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

schema_cases_root=$(mktemp -d "${TMPDIR:-/tmp}/migration-history-schema.XXXXXX")
trap 'rm -rf "$schema_cases_root"' EXIT HUP INT TERM

schema_accept() {
    case_id=$1
    manifest_path=$2
    jsonschema -i "$manifest_path" "$schema_path" >/dev/null 2>&1 || fail "schema should accept: $case_id"
}

schema_reject() {
    case_id=$1
    manifest_path=$2
    if jsonschema -i "$manifest_path" "$schema_path" >/dev/null 2>&1; then
        fail "schema should reject: $case_id"
    fi
}

# These mutations are isolated copies of a bounded fixture. They exercise the
# schema contract directly, including the fact that project admission and the
# migration transaction status are independent dimensions.
collab_fixture="$fixture_root/collab/manifest.json"
schema_case="$schema_cases_root/project-admission-verified-planned.json"
jq '.project_admission = "verified" | .mapping_status = "planned" | .archive_ref = "archive/rehearsal" | .archive_digest = "sha256:archive-rehearsal"' \
    "$collab_fixture" >"$schema_case"
schema_accept 'project-admission-verified-with-planned-transaction' "$schema_case"

schema_case="$schema_cases_root/unknown-adapt-null-blocker.json"
jq ' .records |= map(if .mapping_class == "unknown" then .source_disposition = "adapt_reconcile" | .blocker_code = null else . end)' \
    "$collab_fixture" >"$schema_case"
schema_reject 'unknown-class-cannot-adapt-with-null-blocker' "$schema_case"

schema_case="$schema_cases_root/reset-adapt.json"
jq ' .records |= map(if .mapping_class == "reset" then .source_disposition = "adapt_reconcile" else . end)' \
    "$collab_fixture" >"$schema_case"
schema_reject 'reset-class-requires-rebuild-disposition' "$schema_case"

schema_case="$schema_cases_root/archive-mapped.json"
jq ' .records |= map(if .source_disposition == "archive_only" then .mapping_status = "mapped" | .target_sequence = 0 | .target_entity_id = "forbidden-active-target" else . end)' \
    "$collab_fixture" >"$schema_case"
schema_reject 'archive-only-record-cannot-be-mapped' "$schema_case"

schema_case="$schema_cases_root/direct-replay-adapt-class.json"
jq ' .records |= map(if .source_disposition == "adapt_reconcile" then .source_disposition = "direct_replay" else . end)' \
    "$collab_fixture" >"$schema_case"
schema_reject 'direct-replay-requires-direct-class' "$schema_case"

schema_case="$schema_cases_root/rebuild-mapped-direct-class.json"
jq ' .records |= map(if .mapping_class == "reset" then .mapping_class = "direct" | .mapping_status = "mapped" | .target_sequence = 0 | .target_entity_id = "forbidden-active-target" else . end)' \
    "$collab_fixture" >"$schema_case"
schema_reject 'rebuild-required-record-cannot-be-mapped' "$schema_case"

schema_case="$schema_cases_root/archive-missing-error.json"
jq ' .records |= map(if .source_disposition == "adapt_reconcile" then .source_disposition = "archive_only" | .blocker_code = "PROJECT_ADMISSION_BLOCKED" | .first_failed_boundary = "candidate" | .exact_error = null else . end)' \
    "$collab_fixture" >"$schema_case"
schema_reject 'archive-only-record-requires-exact-error' "$schema_case"

expected_records='{
  "collab": [
    {"source_record_id":".agent-collab/mailbox","source_disposition":"adapt_reconcile","mapping_class":"adapt","mapping_status":"planned","blocker_code":null,"target_epoch":"unassigned-rehearsal-epoch","target_sequence":null,"target_entity_id":null,"raw_archive_ref":null,"exact_error":null,"first_failed_boundary":null},
    {"source_record_id":".agent-collab/server/events.jsonl","source_disposition":"archive_only","mapping_class":"unknown","mapping_status":"planned","blocker_code":"SOURCE_PROJECTION_MUTABLE","target_epoch":"unassigned-rehearsal-epoch","target_sequence":null,"target_entity_id":null,"raw_archive_ref":null,"exact_error":"SOURCE_PROJECTION_MUTABLE:events_digest_changed_during_inspect","first_failed_boundary":"snapshot"},
    {"source_record_id":".agent-collab/server/journal.jsonl","source_disposition":"rebuild_required","mapping_class":"reset","mapping_status":"planned","blocker_code":"MULTIPLE_WRITERS","target_epoch":"unassigned-rehearsal-epoch","target_sequence":null,"target_entity_id":null,"raw_archive_ref":null,"exact_error":"MULTIPLE_WRITERS:20_collab_serve_processes","first_failed_boundary":"writer-admission"}
  ],
  "appsdk": [
    {"source_record_id":".agent-collab/mailbox","source_disposition":"adapt_reconcile","mapping_class":"adapt","mapping_status":"planned","blocker_code":null,"target_epoch":"unassigned-rehearsal-epoch","target_sequence":null,"target_entity_id":null,"raw_archive_ref":null,"exact_error":null,"first_failed_boundary":null},
    {"source_record_id":".agent-collab/server/events.jsonl","source_disposition":"archive_only","mapping_class":"unknown","mapping_status":"planned","blocker_code":"SOURCE_PROJECTION_MUTABLE","target_epoch":"unassigned-rehearsal-epoch","target_sequence":null,"target_entity_id":null,"raw_archive_ref":null,"exact_error":"SOURCE_PROJECTION_MUTABLE:events_digest_changed_during_inspect","first_failed_boundary":"snapshot"},
    {"source_record_id":".agent-collab/server/journal.jsonl","source_disposition":"rebuild_required","mapping_class":"reset","mapping_status":"planned","blocker_code":"UNRESOLVED_GIT_CONFLICTS","target_epoch":"unassigned-rehearsal-epoch","target_sequence":null,"target_entity_id":null,"raw_archive_ref":null,"exact_error":"UNRESOLVED_GIT_CONFLICTS:5","first_failed_boundary":"candidate"}
  ],
  "routecodex": [
    {"source_record_id":".agent-collab/mailbox","source_disposition":"adapt_reconcile","mapping_class":"adapt","mapping_status":"planned","blocker_code":null,"target_epoch":"unassigned-rehearsal-epoch","target_sequence":null,"target_entity_id":null,"raw_archive_ref":null,"exact_error":null,"first_failed_boundary":null},
    {"source_record_id":".agent-collab/server/events.jsonl","source_disposition":"archive_only","mapping_class":"unknown","mapping_status":"planned","blocker_code":"SOURCE_PROJECTION_MUTABLE","target_epoch":"unassigned-rehearsal-epoch","target_sequence":null,"target_entity_id":null,"raw_archive_ref":null,"exact_error":"SOURCE_PROJECTION_MUTABLE:events_digest_changed_during_inspect","first_failed_boundary":"snapshot"},
    {"source_record_id":".agent-collab/server/journal.jsonl","source_disposition":"rebuild_required","mapping_class":"reset","mapping_status":"planned","blocker_code":"DIRTY_MIXED_V3_V4_CANDIDATE","target_epoch":"unassigned-rehearsal-epoch","target_sequence":null,"target_entity_id":null,"raw_archive_ref":null,"exact_error":"DIRTY_MIXED_V3_V4_CANDIDATE:status_entries=50","first_failed_boundary":"candidate"}
  ],
  "codexapp": [
    {"source_record_id":"/Users/fanzhang/.codex-communication/journal.jsonl","source_disposition":"archive_only","mapping_class":"unknown","mapping_status":"planned","blocker_code":"BOOTSTRAP_REQUIRED","target_epoch":"unassigned-rehearsal-epoch","target_sequence":null,"target_entity_id":null,"raw_archive_ref":null,"exact_error":"ENDPOINT_CONNECT_FAILED:connect ECONNREFUSED /Users/fanzhang/.codex-communication/sockets/commd.sock","first_failed_boundary":"native-endpoint-bootstrap"}
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

    case "$project" in
        collab)
            expected_admission='reset_required'
            expected_authority='collab-migration-controller'
            expected_blocker='MULTIPLE_WRITERS'
            expected_boundary='writer-admission'
            ;;
        appsdk)
            expected_admission='reset_required'
            expected_authority='appsdk-quality-owner'
            expected_blocker='UNRESOLVED_GIT_CONFLICTS'
            expected_boundary='candidate'
            ;;
        routecodex)
            expected_admission='reset_required'
            expected_authority='routecodex-governance-owner'
            expected_blocker='DIRTY_MIXED_V3_V4_CANDIDATE'
            expected_boundary='candidate'
            ;;
        codexapp)
            expected_admission='needs_operator'
            expected_authority='codexapp-operator'
            expected_blocker='BOOTSTRAP_REQUIRED'
            expected_boundary='native-endpoint-bootstrap'
            ;;
    esac

    jsonschema -i "$manifest_path" "$schema_path" >/dev/null
    jq empty "$manifest_path" "$rehearsal_path"

    jq -e --arg project "$project" --arg admission "$expected_admission" --arg authority "$expected_authority" --arg blocker "$expected_blocker" --arg boundary "$expected_boundary" --argjson expected "$expected_records" '
        (.source_project_id == $project)
        and (.mapping_status == "planned")
        and (.source_epoch == null)
        and (.project_admission == $admission)
        and (.owner_authority == $authority)
        and (.blocker_code == $blocker)
        and (.first_failed_boundary == $boundary)
        and (.target_epoch == "unassigned-rehearsal-epoch")
        and (.archive_ref == null)
        and (.archive_digest == null)
        and (.source_snapshot_digest | startswith("sha256:"))
        and (all(.records[];
            (.mapping_status == "planned")
            and (.source_disposition | type == "string")
            and (.owner_authority == $authority)
            and ((.blocker_code == null) or (.blocker_code | type == "string"))
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
            source_disposition,
            mapping_class,
            mapping_status,
            blocker_code,
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
