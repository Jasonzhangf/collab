#!/usr/bin/env node

import { appendFile, mkdir, readFile, realpath, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { spawn } from "node:child_process";
import { randomBytes } from "node:crypto";
import { createConnection } from "node:net";

const collab = process.env.COLLAB_APPSERVER_E2E_COLLAB;
const codex = process.env.COLLAB_APPSERVER_E2E_CODEX || "codex";
if (!collab) {
  throw new Error("COLLAB_APPSERVER_E2E_COLLAB is required");
}

const root = `/tmp/ca-${process.pid}`;
await rm(root, { recursive: true, force: true });
await mkdir(root, { recursive: true });
const projectA = join(root, "project-a");
const projectB = join(root, "project-b");
const state = join(root, "host-state");
const socket = join(root, "appserver.sock");
const namespace = "codex_tui";
  await Promise.all([
    mkdir(projectA, { recursive: true }),
    mkdir(projectB, { recursive: true }),
    mkdir(state, { recursive: true }),
  ]);
  for (const project of [projectA, projectB]) {
    await git(project, ["init", "-q", "-b", "main"]);
    await writeFile(join(project, "README.md"), `${project}\n`);
    await git(project, ["add", "README.md"]);
    await git(project, [
      "-c",
      "user.name=Collab E2E",
      "-c",
      "user.email=collab-e2e@example.invalid",
      "commit",
      "-q",
      "-m",
      "initial",
    ]);
  }

const appServer = spawn(codex, ["app-server", "--listen", `unix://${socket}`], {
  cwd: root,
  stdio: ["ignore", "pipe", "pipe"],
});
let appServerStderr = "";
appServer.stderr.on("data", (chunk) => {
  appServerStderr += chunk.toString();
});

function waitForSocket(path) {
  return (async () => {
    for (let attempt = 0; attempt < 100; attempt += 1) {
      const connected = await new Promise((resolve) => {
        const client = createConnection(path);
        client.once("connect", () => {
          client.destroy();
          resolve(true);
        });
        client.once("error", () => {
          client.destroy();
          resolve(false);
        });
      });
      if (connected) return;
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
    throw new Error(`AppServer socket did not become ready: ${appServerStderr}`);
  })();
}

function websocketConnect(path) {
  return new Promise((resolve, reject) => {
    const stream = createConnection(path);
    const key = randomBytes(16).toString("base64");
    stream.once("error", reject);
    stream.once("connect", () => {
      stream.write(
        `GET / HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: ${key}\r\nSec-WebSocket-Version: 13\r\n\r\n`,
      );
    });
    let buffer = Buffer.alloc(0);
    const onData = (chunk) => {
      buffer = Buffer.concat([buffer, chunk]);
      const end = buffer.indexOf("\r\n\r\n");
      if (end === -1) return;
      const header = buffer.subarray(0, end).toString();
      if (!header.startsWith("HTTP/1.1 101")) {
        reject(new Error(`WebSocket upgrade failed: ${header}`));
        return;
      }
      stream.off("data", onData);
      buffer = buffer.subarray(end + 4);
      resolve({ stream, buffer });
    };
    stream.on("data", onData);
  });
}

function encodeFrame(payload) {
  const data = Buffer.from(JSON.stringify(payload));
  const mask = randomBytes(4);
  let header;
  if (data.length < 126) {
    header = Buffer.from([0x81, 0x80 | data.length]);
  } else if (data.length <= 0xffff) {
    header = Buffer.alloc(4);
    header[0] = 0x81;
    header[1] = 0x80 | 126;
    header.writeUInt16BE(data.length, 2);
  } else {
    header = Buffer.alloc(10);
    header[0] = 0x81;
    header[1] = 0x80 | 127;
    header.writeBigUInt64BE(BigInt(data.length), 2);
  }
  const masked = Buffer.alloc(data.length);
  for (let index = 0; index < data.length; index += 1) {
    masked[index] = data[index] ^ mask[index % 4];
  }
  return Buffer.concat([header, mask, masked]);
}

function decodeFrame(buffer) {
  if (buffer.length < 2) return null;
  const opcode = buffer[0] & 0x0f;
  let length = buffer[1] & 0x7f;
  let offset = 2;
  if (length === 126) {
    if (buffer.length < 4) return null;
    length = buffer.readUInt16BE(2);
    offset = 4;
  } else if (length === 127) {
    if (buffer.length < 10) return null;
    length = Number(buffer.readBigUInt64BE(2));
    offset = 10;
  }
  if (buffer.length < offset + length) return null;
  return {
    opcode,
    payload: buffer.subarray(offset, offset + length),
    rest: buffer.subarray(offset + length),
  };
}

function createClient(connection) {
  let buffer = connection.buffer;
  let nextId = 1;
  const pending = new Map();
  connection.stream.on("data", (chunk) => {
    buffer = Buffer.concat([buffer, chunk]);
    for (;;) {
      const frame = decodeFrame(buffer);
      if (!frame) return;
      buffer = frame.rest;
      if (frame.opcode !== 0x1) continue;
      const value = JSON.parse(frame.payload.toString());
      if (typeof value.id !== "number" || !pending.has(value.id)) continue;
      const { resolve, reject } = pending.get(value.id);
      pending.delete(value.id);
      if (value.error) reject(new Error(JSON.stringify(value.error)));
      else resolve(value.result);
    }
  });
  return {
    call(method, params) {
      const id = nextId++;
      return new Promise((resolve, reject) => {
        pending.set(id, { resolve, reject });
        connection.stream.write(encodeFrame({ method, id, params }));
      });
    },
    notify(method, params) {
      connection.stream.write(encodeFrame({ method, params }));
    },
    close() {
      connection.stream.end();
    },
  };
}

function run(command, args, options = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      cwd: options.cwd,
      env: options.env,
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
            `${command} ${args.join(" ")} failed (${code ?? signal}): ${stderr || stdout}`,
          ),
        );
        return;
      }
      resolve({ stdout, stderr });
    });
  });
}

async function runJson(command, args, options) {
  const result = await run(command, args, options);
  try {
    return JSON.parse(result.stdout);
  } catch (error) {
    throw new Error(
      `${command} ${args.join(" ")} returned non-JSON output: ${result.stdout}\n${error}`,
    );
  }
}

async function git(cwd, args) {
  return run("git", args, { cwd, env: process.env });
}

function projectEnv(project, threadId, workerId) {
  return {
    ...process.env,
    HOME: root,
    COLLAB_STATE_DIR: state,
    COLLAB_APPSERVER_SOCKET: socket,
    COLLAB_APPSERVER_NAMESPACE: namespace,
    CODEX_THREAD_ID: threadId,
    COLLAB_WORKER: workerId,
    TMUX: undefined,
    TMUX_PANE: undefined,
    PATH: `${process.env.PATH || ""}`,
  };
}

function redact(value) {
  return JSON.parse(
    JSON.stringify(value, (key, item) =>
      key.toLowerCase().includes("token") ? "<redacted>" : item,
    ),
  );
}

function assert(condition, message, detail) {
  if (!condition) {
    throw new Error(
      `${message}${detail === undefined ? "" : `: ${JSON.stringify(detail)}`}`,
    );
  }
}

function workerById(status, workerId) {
  return (status.workers || []).find((worker) => worker.id === workerId);
}

function messageBody(message) {
  return String(message.body || "");
}

let appClient;
let appServerStopped = false;
let envA;
let envB;
const report = {
  root,
  appserver_socket: socket,
  appserver_namespace: namespace,
  projects: {},
  checks: {},
};

try {
  await waitForSocket(socket);
  appClient = createClient(await websocketConnect(socket));
  await appClient.call("initialize", {
    clientInfo: { name: "collab-e2e", title: "Collab e2e", version: "1" },
    capabilities: { experimentalApi: true },
  });
  appClient.notify("initialized", {});

  const first = await appClient.call("thread/start", {
    cwd: projectA,
    approvalPolicy: "never",
    sandbox: "danger-full-access",
    sessionStartSource: "startup",
  });
  const second = await appClient.call("thread/start", {
    cwd: projectB,
    approvalPolicy: "never",
    sandbox: "danger-full-access",
    sessionStartSource: "startup",
  });
  const threadA = first.thread.id;
  const threadB = second.thread.id;
  if (!threadA || !threadB || threadA === threadB) {
    throw new Error("isolated AppServer did not create two distinct threads");
  }

  const workerA = "peer-a";
  const workerB = "peer-b";
  envA = projectEnv(projectA, threadA, workerA);
  envB = projectEnv(projectB, threadB, workerB);

  const initA = await runJson(collab, ["init"], { cwd: projectA, env: envA });
  const initB = await runJson(collab, ["init"], { cwd: projectB, env: envB });
  report.projects.a = redact({ init: initA, worker: workerA, thread: threadA });
  report.projects.b = redact({ init: initB, worker: workerB, thread: threadB });
  report.checks.init = {
    a_transport: initA.transport_selected?.kind,
    b_transport: initB.transport_selected?.kind,
    a_thread: initA.transport_selected?.thread_id,
    b_thread: initB.transport_selected?.thread_id,
  };
  assert(
    initA.transport_selected?.kind === "appserver" &&
      initB.transport_selected?.kind === "appserver" &&
      initA.transport_selected?.thread_id === threadA &&
      initB.transport_selected?.thread_id === threadB,
    "server did not select the expected AppServer transports",
    report.checks.init,
  );

  const contextA = await runJson(collab, ["context"], { cwd: projectA, env: envA });
  const contextB = await runJson(collab, ["context"], { cwd: projectB, env: envB });
  report.checks.context = {
    a_worker: contextA.identity?.worker_id,
    b_worker: contextB.identity?.worker_id,
    a_transport: contextA.identity?.transport?.kind,
    b_transport: contextB.identity?.transport?.kind,
    a_live: contextA.liveness?.live,
    b_live: contextB.liveness?.live,
    a_presence: contextA.liveness?.presence,
    b_presence: contextB.liveness?.presence,
  };
  assert(
    contextA.identity?.worker_id === workerA &&
      contextB.identity?.worker_id === workerB &&
      contextA.identity?.transport?.kind === "appserver" &&
      contextB.identity?.transport?.kind === "appserver" &&
      contextA.liveness?.live === true &&
      contextB.liveness?.live === true &&
      contextA.liveness?.presence === "present" &&
      contextB.liveness?.presence === "present",
    "context did not report live AppServer identities",
    report.checks.context,
  );

  const statusA = await runJson(collab, ["worker", "status", workerA], {
    cwd: projectA,
    env: envA,
  });
  const statusB = await runJson(collab, ["worker", "status", workerB], {
    cwd: projectB,
    env: envB,
  });
  const workerAStatus = workerById(statusA, workerA);
  const workerBStatus = workerById(statusB, workerB);
  report.checks.worker_status = {
    a: workerAStatus,
    b: workerBStatus,
  };
  assert(
    workerAStatus?.endpoint_live === true &&
      workerBStatus?.endpoint_live === true &&
      workerAStatus?.presence === "present" &&
      workerBStatus?.presence === "present" &&
      workerAStatus?.identity_valid === true &&
      workerBStatus?.identity_valid === true,
    "worker status did not report live identities",
    report.checks.worker_status,
  );

  const promoteA = await runJson(
    collab,
    ["master", "promote", "--approval", "isolated e2e approved peer-a"],
    { cwd: projectA, env: envA },
  );
  const promoteB = await runJson(
    collab,
    ["master", "promote", "--approval", "isolated e2e approved peer-b"],
    { cwd: projectB, env: envB },
  );
  assert(
    promoteA.role_brief?.role === "master" &&
      promoteB.role_brief?.role === "master",
    "isolated peers did not become masters",
    { a: promoteA, b: promoteB },
  );
  report.checks.master = {
    a: promoteA.mode,
    b: promoteB.mode,
  };

  const masterWorktree = join(projectA, "playground", "master-status");
  const masterBranch = `codex/master-status-${Date.now()}`;
  await git(projectA, [
    "worktree",
    "add",
    "-q",
    "-b",
    masterBranch,
    masterWorktree,
    "main",
  ]);
  await mkdir(join(masterWorktree, ".agent-collab", "server"), { recursive: true });
  const canonicalProjectA = await realpath(projectA);
  const canonicalMasterWorktree = await realpath(masterWorktree);
  const routeJournal = join(state, "routes.jsonl");
  const currentRoutes = await readFile(routeJournal, "utf8");
  const projectRoute = currentRoutes
    .trim()
    .split("\n")
    .map((line) => JSON.parse(line))
    .find((record) => record.canonical_root === canonicalProjectA);
  assert(projectRoute, "project A route was not persisted", currentRoutes);
  await appendFile(
    routeJournal,
    `${JSON.stringify({
      ...projectRoute,
      project_scope: canonicalMasterWorktree,
      canonical_root: canonicalMasterWorktree,
      storage_root: canonicalMasterWorktree,
      registered_ms: Number(projectRoute.registered_ms || 0) + 1,
    })}\n`,
  );
  const worktreeContext = await runJson(collab, ["context"], {
    cwd: masterWorktree,
    env: envA,
  });
  const worktreeMaster = await runJson(collab, ["master", "status"], {
    cwd: masterWorktree,
    env: envA,
  });
  const worktreeWho = await runJson(collab, ["who"], {
    cwd: masterWorktree,
    env: envA,
  });
  const worktreeWorkerA = workerById(worktreeWho, workerA);
  report.checks.worktree_master = {
    stale_route: canonicalMasterWorktree,
    project_root: worktreeContext.project_root,
    worker_id: worktreeContext.identity?.worker_id,
    presence: worktreeContext.liveness?.presence,
    master: worktreeMaster.master,
    recorded_unusable: worktreeMaster.recorded_unusable,
    who_worker: worktreeWorkerA,
  };
  assert(
    worktreeContext.project_root === canonicalProjectA &&
      worktreeContext.identity?.worker_id === workerA &&
      worktreeContext.liveness?.presence === "present" &&
      worktreeMaster.master?.worker_id === workerA &&
      worktreeMaster.master?.endpoint_live === true &&
      worktreeMaster.recorded_unusable == null &&
      worktreeWorkerA?.presence === "present" &&
      worktreeWorkerA?.endpoint_live === true,
    "worktree did not resolve the canonical project route and live master",
    report.checks.worktree_master,
  );

  const marker = `cross-project-${Date.now()}`;
  const sent = await runJson(
    collab,
    [
      "master",
      "send",
      "--project",
      projectB,
      "--to",
      workerB,
      "--subject",
      "e2e",
      marker,
    ],
    { cwd: projectA, env: envA },
  );
  assert(
    sent.durable === true &&
      sent.cross_project === true &&
      sent.notification !== "PROJECT_ROUTE_NOT_READY",
    "cross-project send was not durable",
    sent,
  );
  const inbox = await runJson(collab, ["inbox"], { cwd: projectB, env: envB });
  const recv = await runJson(collab, ["recv", "--timeout", "0"], {
    cwd: projectB,
    env: envB,
  });
  const recvMessages = recv.messages || [];
  const received = recvMessages.some((message) =>
    String(message.body || "").includes(marker),
  );
  assert(received, "cross-project message was not received", recv);
  report.checks.cross_project = {
    sent: redact(sent),
    inbox_count: inbox.unread,
    recv_count: recv.count,
    received_marker: marker,
  };

  const replayMarker = `cross-project-replay-${Date.now()}`;
  const replaySent = await runJson(
    collab,
    [
      "master",
      "send",
      "--project",
      projectB,
      "--to",
      workerB,
      "--subject",
      "e2e-replay",
      replayMarker,
    ],
    { cwd: projectA, env: envA },
  );
  assert(
    replaySent.durable === true && replaySent.cross_project === true,
    "replay cross-project send was not durable",
    replaySent,
  );
  const replayBeforeDown = await runJson(collab, ["inbox"], {
    cwd: projectB,
    env: envB,
  });
  assert(
    replayBeforeDown.messages?.some((message) =>
      messageBody(message).includes(replayMarker),
    ),
    "replay message was not in the durable inbox before restart",
    replayBeforeDown,
  );

  await runJson(collab, ["down"], { cwd: projectB, env: envB });
  await runJson(collab, ["up"], { cwd: projectB, env: envB });
  const afterUp = await runJson(collab, ["context"], { cwd: projectB, env: envB });
  const afterReplayInbox = await runJson(collab, ["inbox"], {
    cwd: projectB,
    env: envB,
  });
  const afterReplayRecv = await runJson(collab, ["recv", "--timeout", "0"], {
    cwd: projectB,
    env: envB,
  });
  assert(
    afterUp.identity?.worker_id === workerB &&
      afterUp.identity?.transport?.kind === "appserver" &&
      afterUp.identity?.transport?.thread_id === threadB &&
      afterUp.liveness?.live === true &&
      afterUp.liveness?.presence === "present" &&
      afterReplayInbox.messages?.some((message) =>
        messageBody(message).includes(replayMarker),
      ) &&
      afterReplayRecv.messages?.some((message) =>
        messageBody(message).includes(replayMarker),
      ),
    "restart did not preserve the AppServer identity and unconsumed mailbox message",
    {
      context: afterUp,
      inbox: afterReplayInbox,
      recv: afterReplayRecv,
    },
  );
  report.checks.restart = {
    before_inbox_count: replayBeforeDown.unread,
    after_worker: afterUp.identity?.worker_id,
    after_transport: afterUp.identity?.transport?.kind,
    after_thread: afterUp.identity?.transport?.thread_id,
    after_presence: afterUp.liveness?.presence,
    after_inbox_count: afterReplayInbox.unread,
    replay_marker: replayMarker,
  };

  const taskId = `e2e-${Date.now()}`;
  const taskWorktree = join(projectB, "playground", "e2e");
  const taskBranch = `codex/${taskId}`;
  await git(projectB, [
    "worktree",
    "add",
    "-q",
    "-b",
    taskBranch,
    taskWorktree,
    "main",
  ]);
  const canonicalTaskWorktree = `${await realpath(taskWorktree)}/`;
  await writeFile(join(taskWorktree, "task.txt"), "appserver e2e\n");
  await git(taskWorktree, ["add", "task.txt"]);
  await git(taskWorktree, [
    "-c",
    "user.name=Collab E2E",
    "-c",
    "user.email=collab-e2e@example.invalid",
    "commit",
    "-q",
    "-m",
    "e2e task",
  ]);
  const taskCommit = (await git(taskWorktree, ["rev-parse", "HEAD"])).stdout.trim();
  const baseCommit = (await git(projectB, ["rev-parse", "HEAD"])).stdout.trim();
  await git(projectB, ["merge", "--ff-only", taskBranch]);
  const mainHead = (await git(projectB, ["rev-parse", "HEAD"])).stdout.trim();
  await runJson(
    collab,
    [
      "task",
      "register",
      taskId,
      "--feature",
      "appserver-e2e",
      "--worktree",
      canonicalTaskWorktree,
      "--branch",
      taskBranch,
      "--base-commit",
      baseCommit,
      "--next",
      "run lifecycle",
    ],
    { cwd: projectB, env: envB },
  );
  await runJson(collab, ["task", "update", taskId, "--status", "verifying"], {
    cwd: projectB,
    env: envB,
  });
  await runJson(collab, ["task", "update", taskId, "--status", "reviewed"], {
    cwd: projectB,
    env: envB,
  });
  await runJson(
    collab,
    [
      "task",
      "deliver",
      taskId,
      "--evidence",
      `commit=${taskCommit}; gates=pass`,
      "--worktree",
      canonicalTaskWorktree,
    ],
    { cwd: projectB, env: envB },
  );
  await runJson(
    collab,
    [
      "task",
      "review",
      taskId,
      "--accept",
      "--evidence",
      "independent review=pass",
    ],
    { cwd: projectB, env: envB },
  );
  await runJson(
    collab,
    [
      "task",
      "integrated",
      taskId,
      "--commit",
      mainHead,
      "--evidence",
      "main gates=pass",
    ],
    { cwd: projectB, env: envB },
  );
  await runJson(collab, ["task", "close", taskId], {
    cwd: projectB,
    env: envB,
  });
  const taskStatus = await runJson(collab, ["task", "status", taskId], {
    cwd: projectB,
    env: envB,
  });
  assert(
    taskStatus.status === "closed" &&
      taskStatus.delivery?.evidence?.includes(taskCommit) &&
      taskStatus.review?.evidence === "independent review=pass" &&
      taskStatus.integration?.commit === mainHead &&
      taskStatus.cleanup?.status === "verified",
    "task lifecycle did not reach verified close",
    taskStatus,
  );
  report.checks.task = {
    id: taskId,
    status: taskStatus.status,
    worktree: taskStatus.worktree,
    branch: taskStatus.branch,
    delivery: taskStatus.delivery,
    review: taskStatus.review,
    integration: taskStatus.integration,
    cleanup: taskStatus.cleanup,
  };

  console.log(JSON.stringify(report, null, 2));
} finally {
  if (envA) {
    await runJson(collab, ["down"], { cwd: projectA, env: envA }).catch(() => {});
  }
  if (envB) {
    await runJson(collab, ["down"], { cwd: projectB, env: envB }).catch(() => {});
  }
  if (appClient) appClient.close();
  if (!appServerStopped) {
    const exited = new Promise((resolve) => {
      if (appServer.exitCode !== null || appServer.signalCode !== null) resolve();
      else appServer.once("exit", resolve);
    });
    appServer.kill("SIGTERM");
    await Promise.race([
      exited,
      new Promise((resolve) => setTimeout(resolve, 1000)),
    ]);
    if (appServer.exitCode === null && appServer.signalCode === null) {
      appServer.kill("SIGKILL");
      await exited;
    }
    appServerStopped = true;
  }
  if (process.env.COLLAB_APPSERVER_E2E_KEEP !== "1") {
    await rm(root, { recursive: true, force: true });
    await rm(socket, { force: true });
  } else {
    console.error(`preserved e2e root: ${root}`);
  }
}
