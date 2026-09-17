#!/usr/bin/env node

import {
  mkdir,
  readFile,
  readdir,
  realpath,
  rm,
  stat,
  writeFile,
} from "node:fs/promises";
import { join } from "node:path";
import { spawn } from "node:child_process";

const collab = process.env.COLLAB_RESET_REPLAY_COLLAB;
if (!collab) {
  throw new Error("COLLAB_RESET_REPLAY_COLLAB is required");
}

const root = `/tmp/crr-${process.pid}-${Date.now().toString(36)}`;
const project = join(root, "project");
const unrelatedProject = join(root, "unrelated");
const state = join(root, "state");

function run(args, options = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(collab, args, {
      cwd: options.cwd || project,
      env: {
        ...process.env,
        HOME: root,
        COLLAB_STATE_DIR: state,
        TMUX: undefined,
        TMUX_PANE: undefined,
      },
      stdio: ["ignore", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk) => {
      stdout += chunk.toString();
    });
    child.stderr.on("data", (chunk) => {
      stderr += chunk.toString();
    });
    child.once("error", reject);
    child.once("exit", (code, signal) => {
      if (code !== 0) {
        reject(
          new Error(
            `${args.join(" ")} failed (${code ?? signal}): ${stderr || stdout}`,
          ),
        );
        return;
      }
      resolve({ stdout, stderr });
    });
  });
}

async function runJson(args) {
  const result = await run(args);
  return JSON.parse(result.stdout);
}

function assert(condition, message, detail) {
  if (!condition) {
    throw new Error(
      `${message}${detail === undefined ? "" : `: ${JSON.stringify(detail)}`}`,
    );
  }
}

async function pathExists(path) {
  try {
    await stat(path);
    return true;
  } catch (error) {
    if (error.code === "ENOENT") return false;
    throw error;
  }
}

try {
  await mkdir(join(project, ".agent-collab", "server"), { recursive: true });
  await mkdir(join(project, ".agent-collab-v2", "nested"), { recursive: true });
  await mkdir(join(unrelatedProject, ".agent-collab"), { recursive: true });
  await mkdir(state, { recursive: true });
  const canonicalProject = await realpath(project);
  const canonicalUnrelated = await realpath(unrelatedProject);
  const legacyJournal = '{"ev":"LegacyRecord","value":1}\n';
  await writeFile(
    join(project, ".agent-collab", "server", "journal.jsonl"),
    legacyJournal,
  );
  await writeFile(
    join(project, ".agent-collab-v2", "nested", "legacy.txt"),
    "legacy-v2\n",
  );
  await writeFile(
    join(state, "routes.jsonl"),
    [
      JSON.stringify({
        version: 1,
        op: "register",
        app_scope_id: "appserver-cli",
        project_scope: canonicalProject,
        canonical_root: canonicalProject,
        storage_root: canonicalProject,
        registered_ms: 1,
      }),
      JSON.stringify({
        version: 1,
        op: "register",
        app_scope_id: "appserver-cli",
        project_scope: canonicalUnrelated,
        canonical_root: canonicalUnrelated,
        storage_root: canonicalUnrelated,
        registered_ms: 2,
      }),
      JSON.stringify({
        version: 1,
        op: "register",
        app_scope_id: "appserver-cli",
        project_scope: join(root, "missing"),
        canonical_root: join(root, "missing"),
        storage_root: join(root, "missing"),
        registered_ms: 3,
      }),
      "",
    ].join("\n"),
  );

  const reset = await runJson([
    "reset",
    "--discard-legacy",
    "--approval",
    "isolated reset replay authorized",
  ]);
  assert(
    reset.delivery_verified === false &&
      reset.already_reset === false &&
      reset.removed_host_routes === 1 &&
      reset.removed_stale_host_routes === 1 &&
      reset.retired?.length === 2,
    "reset did not retire the named control planes and routes",
    reset,
  );
  assert(
    !(await pathExists(join(project, ".agent-collab-v2"))),
    "legacy .agent-collab-v2 was not removed",
  );
  assert(
    !(await pathExists(
      join(project, ".agent-collab", "server", "journal.jsonl"),
    )),
    "legacy project journal survived the reset",
  );
  assert(
    await pathExists(join(project, ".agent-collab", "server")),
    "current .agent-collab baseline was not rebuilt",
  );
  assert(
    (await readFile(join(project, "docs", "collab.md"), "utf8")).includes(
      "Runtime boundary",
    ),
    "current Collab guidance was not rebuilt",
  );

  const routes = (await readFile(join(state, "routes.jsonl"), "utf8"))
    .trim()
    .split("\n")
    .filter(Boolean)
    .map((line) => JSON.parse(line));
  assert(
    routes.length === 1 && routes[0].canonical_root === canonicalUnrelated,
    "route journal did not preserve only the unrelated live route",
    routes,
  );

  const resetRecords = (await readFile(join(state, "reset.jsonl"), "utf8"))
    .trim()
    .split("\n")
    .map((line) => JSON.parse(line));
  assert(
    resetRecords.length === 1 &&
      resetRecords[0].delivery_verified === false &&
      resetRecords[0].already_reset === false,
    "reset journal record is missing or fabricated delivery",
    resetRecords,
  );

  const archiveEntries = await readdir(join(state, "archives"));
  assert(archiveEntries.length === 1, "reset archive was not created");
  const archiveRoot = join(state, "archives", archiveEntries[0]);
  const manifest = JSON.parse(
    await readFile(join(archiveRoot, "manifest.json"), "utf8"),
  );
  assert(
    manifest.delivery_verified === false &&
      manifest.retired?.length === 2 &&
      (await readFile(
        join(archiveRoot, ".agent-collab", "server", "journal.jsonl"),
        "utf8",
      )) === legacyJournal,
    "archive manifest or archived legacy bytes are invalid",
    manifest,
  );

  const second = await runJson([
    "reset",
    "--discard-legacy",
    "--approval",
    "isolated reset replay idempotence",
  ]);
  assert(
    second.delivery_verified === false &&
      second.already_reset === true &&
      second.retired?.length === 0,
    "second reset was not idempotent",
    second,
  );

  await run(["up"]);
  await run(["status"]);
  await run(["down"]);

  console.log(
    JSON.stringify(
      {
        root,
        reset: {
          delivery_verified: reset.delivery_verified,
          removed_host_routes: reset.removed_host_routes,
          removed_stale_host_routes: reset.removed_stale_host_routes,
          retired: reset.retired,
          archive_root: reset.archive_root,
        },
        idempotent_second_reset: {
          already_reset: second.already_reset,
          delivery_verified: second.delivery_verified,
        },
        baseline_started: true,
      },
      null,
      2,
    ),
  );
} finally {
  await run(["down"]).catch(() => {});
  if (process.env.COLLAB_RESET_REPLAY_KEEP !== "1") {
    await rm(root, { recursive: true, force: true });
  } else {
    console.error(`preserved reset replay root: ${root}`);
  }
}
