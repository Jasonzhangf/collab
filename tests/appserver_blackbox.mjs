#!/usr/bin/env node

import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawn } from "node:child_process";
import { randomBytes } from "node:crypto";
import { createConnection } from "node:net";

const socket = process.env.COLLAB_APPSERVER_BLACKBOX_SOCKET;
const codex = process.env.COLLAB_APPSERVER_BLACKBOX_CODEX || "codex";
if (!socket) {
  throw new Error("COLLAB_APPSERVER_BLACKBOX_SOCKET is required");
}

const root = await mkdtemp(join(tmpdir(), "collab-appserver-blackbox-"));
const server = spawn(codex, ["app-server", "--listen", `unix://${socket}`], {
  cwd: root,
  stdio: ["ignore", "pipe", "pipe"],
});
let stderr = "";
server.stderr.on("data", (chunk) => {
  stderr += chunk.toString();
});

async function waitForSocket() {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    try {
      const probe = await new Promise((resolve) => {
        const client = createConnection(socket);
        client.once("connect", () => {
          client.destroy();
          resolve(true);
        });
        client.once("error", () => {
          client.destroy();
          resolve(false);
        });
      });
      if (probe) return;
    } catch {
      // Retry until the child has bound the socket or exited.
    }
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  throw new Error(`AppServer socket did not become ready: ${stderr}`);
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
  const payload = buffer.subarray(offset, offset + length);
  return { opcode, payload, rest: buffer.subarray(offset + length) };
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
      if (typeof value.id === "number" && pending.has(value.id)) {
        const { resolve, reject } = pending.get(value.id);
        pending.delete(value.id);
        if (value.error) reject(new Error(JSON.stringify(value.error)));
        else resolve(value.result);
      }
    }
  });

  function call(method, params) {
    const id = nextId++;
    return new Promise((resolve, reject) => {
      pending.set(id, { resolve, reject });
      connection.stream.write(encodeFrame({ method, id, params }));
    });
  }

  function notify(method, params) {
    connection.stream.write(encodeFrame({ method, params }));
  }

  return { call, notify, close: () => connection.stream.end() };
}

let client;
try {
  await waitForSocket();
  client = createClient(await websocketConnect(socket));
  await client.call("initialize", {
    clientInfo: { name: "collab-blackbox", title: "Collab blackbox", version: "1" },
    capabilities: { experimentalApi: true },
  });
  client.notify("initialized", {});

  const first = await client.call("thread/start", {
    cwd: root,
    approvalPolicy: "never",
    sandbox: "danger-full-access",
    sessionStartSource: "startup",
  });
  const second = await client.call("thread/start", {
    cwd: root,
    approvalPolicy: "never",
    sandbox: "danger-full-access",
    sessionStartSource: "startup",
  });
  const firstId = first.thread.id;
  const secondId = second.thread.id;
  if (!firstId || !secondId || firstId === secondId) {
    throw new Error("isolated AppServer did not create two distinct threads");
  }

  const marker = `collab-blackbox-${Date.now()}`;
  const started = await client.call("turn/start", {
    threadId: firstId,
    input: [{ type: "text", text: marker }],
    clientUserMessageId: marker,
  });
  const firstRead = await client.call("thread/read", { threadId: firstId });
  const secondRead = await client.call("thread/read", { threadId: secondId });
  if (firstRead.thread.id !== firstId || secondRead.thread.id !== secondId) {
    throw new Error("thread identity changed after turn/start");
  }
  let itemsProbe;
  try {
    itemsProbe = {
      supported: true,
      result: await client.call("thread/items/list", {
        threadId: firstId,
        limit: 20,
        sortDirection: "desc",
      }),
    };
  } catch (error) {
    const message = String(error);
    if (!message.includes("-32601") && !message.includes("not supported yet")) {
      throw error;
    }
    itemsProbe = { supported: false, diagnostic: message };
  }
  console.log(
    JSON.stringify(
      {
        socket,
        first_thread_id: firstId,
        second_thread_id: secondId,
        turn_start: started,
        first_status: firstRead.thread.status,
        second_status: secondRead.thread.status,
        items_probe: itemsProbe,
        accepted_semantics:
          "turn/start accepted the immediate notification; execution and reply are observed separately",
      },
      null,
      2,
    ),
  );
} finally {
  if (client) client.close();
  const exited = new Promise((resolve) => {
    if (server.exitCode !== null || server.signalCode !== null) resolve();
    else server.once("exit", resolve);
  });
  server.kill("SIGTERM");
  await Promise.race([
    exited,
    new Promise((resolve) => setTimeout(resolve, 1000)),
  ]);
  if (server.exitCode === null && server.signalCode === null) {
    server.kill("SIGKILL");
    await exited;
  }
  await rm(root, { recursive: true, force: true });
  await rm(socket, { force: true });
}
