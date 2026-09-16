#!/usr/bin/env node
// Captures a real Pi process using deterministic provider bytes, without a remote model.
import { createServer } from "node:http";
import { spawn, execFileSync } from "node:child_process";
import { once } from "node:events";
import { mkdtemp, mkdir, writeFile, readFile, readdir, realpath } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createHash } from "node:crypto";
import { createInterface } from "node:readline";

const root = await mkdtemp(join(tmpdir(), "sylph-pi-loopback-"));
const config = join(root, "config");
const sessions = join(root, "sessions");
await mkdir(config);
await mkdir(sessions);
const version = execFileSync("pi", ["--version"], { encoding: "utf8" }).trim();
const requests = [];
const providerFrames = [];
const rpc = [];
const stdout = [];
const stderr = [];
let child;
let childExit;
let streamError;
const server = createServer(async (request, response) => {
  try {
    if (request.method !== "POST" || request.url !== "/v1/chat/completions") {
      response.writeHead(404).end();
      return;
    }
    const chunks = [];
    let bytes = 0;
    for await (const chunk of request) {
      bytes += chunk.length;
      if (bytes > 1024 * 1024) throw new Error("Provider request exceeds capture bound");
      chunks.push(chunk);
    }
    requests.push(JSON.parse(Buffer.concat(chunks).toString("utf8")));
    response.writeHead(200, { "content-type": "text/event-stream" });
    const emit = (delta, finish = null) => {
      const event = {
        id: "controlled-pi-response", object: "chat.completion.chunk", created: 1,
        model: "trace-marker", choices: [{ index: 0, delta, finish_reason: finish }],
      };
      const frame = `data: ${JSON.stringify(event)}\n\n`;
      providerFrames.push(frame);
      response.write(frame);
    };
    emit({ role: "assistant", content: "" });
    for (const content of ["SYLPH_PI_", "A", "A", "_OK"]) emit({ content });
    emit({}, "stop");
    providerFrames.push("data: [DONE]\n\n");
    response.end("data: [DONE]\n\n");
  } catch (error) {
    streamError = error;
    response.destroy(error);
  }
});
server.listen(0, "127.0.0.1");
await once(server, "listening");
const baseUrl = `http://127.0.0.1:${server.address().port}/v1`;
await writeFile(join(config, "models.json"), JSON.stringify({
  providers: { "sylph-capture": { baseUrl, api: "openai-completions", apiKey: "local-fixture",
    models: [{ id: "trace-marker", reasoning: false, input: ["text"], contextWindow: 8192, maxTokens: 512 }],
  } },
}));
const pending = new Map();
let settle;
const settled = new Promise((resolve) => { settle = resolve; });
const timeout = setTimeout(() => {
  streamError = new Error("Pi capture exceeded 30 seconds");
  child?.kill("SIGTERM");
}, 30_000);
try {
  child = spawn("pi", [
    "--mode", "rpc", "--provider", "sylph-capture", "--model", "trace-marker",
    "--thinking", "off", "--session-dir", sessions,
    "--system-prompt", "Return the controlled marker supplied by the local provider.",
    "--no-tools", "--no-extensions", "--no-skills", "--no-prompt-templates",
    "--no-themes", "--no-context-files", "--no-approve", "--offline",
  ], { cwd: root, env: {
    PATH: process.env.PATH,
    PI_CODING_AGENT_DIR: config, PI_OFFLINE: "1", PI_TELEMETRY: "0",
    NO_COLOR: "1",
  }, stdio: ["pipe", "pipe", "pipe"] });
  const exited = once(child, "exit");
  childExit = exited;
  const failed = exited.then(([code, signal]) => {
    throw streamError ?? new Error(`Pi exited before capture completed (${code}, ${signal})`);
  });
  let capturedBytes = 0;
  const retain = (chunks, chunk) => {
    capturedBytes += chunk.length;
    if (capturedBytes > 8 * 1024 * 1024) {
      streamError = new Error("Pi capture exceeds its byte bound");
      child.kill("SIGTERM");
    } else chunks.push(chunk);
  };
  child.stdout.on("data", (chunk) => retain(stdout, chunk));
  child.stderr.on("data", (chunk) => retain(stderr, chunk));
  createInterface({ input: child.stdout }).on("line", (line) => {
    try {
      const frame = JSON.parse(line);
      rpc.push({ direction: "in", frame });
      if (frame.type === "response") pending.get(frame.id)?.(frame);
      if (frame.type === "agent_settled") settle();
    } catch (error) {
      streamError = error;
      child.kill("SIGTERM");
    }
  });
  const send = (id, type, fields = {}) => {
    const frame = { id, type, ...fields };
    const received = new Promise((resolve) => pending.set(id, resolve));
    rpc.push({ direction: "out", frame });
    child.stdin.write(JSON.stringify(frame) + "\n");
    return Promise.race([received, failed]);
  };
  await send("state-before", "get_state");
  await send("entries-before", "get_entries");
  const admitted = await send("prompt-marker", "prompt", { message: "Reply with exactly SYLPH_PI_AA_OK." });
  if (!admitted.success) throw new Error(`Pi refused capture prompt: ${JSON.stringify(admitted)}`);
  await Promise.race([settled, failed]);
  await send("state-after", "get_state");
  await send("entries-after", "get_entries");
  if (streamError) throw streamError;
  if (requests.length !== 1) throw new Error(`Expected one provider request, received ${requests.length}`);
  const files = (await readdir(sessions)).filter((file) => file.endsWith(".jsonl"));
  if (files.length !== 1) throw new Error("Expected one retained Pi session");
  const native = await readFile(join(sessions, files[0]));
  const entries = native.toString("utf8").trimEnd().split("\n").map((line) => JSON.parse(line));
  const assistant = entries.find((entry) => entry.message?.role === "assistant")?.message;
  if (assistant?.stopReason !== "stop" || assistant.content[0]?.text !== "SYLPH_PI_AA_OK")
    throw new Error("Pi did not persist the controlled successful response");
  const rawStdout = Buffer.concat(stdout);
  const encodedRpc = Buffer.from(rpc.map((row) => JSON.stringify(row)).join("\n") + "\n");
  await writeFile(join(root, "rpc.jsonl"), encodedRpc);
  await writeFile(join(root, "stdout.jsonl"), rawStdout);
  await writeFile(join(root, "stderr.txt"), Buffer.concat(stderr));
  await writeFile(join(root, "native.jsonl"), native);
  await writeFile(join(root, "provider-request.json"), JSON.stringify(requests[0], null, 2) + "\n");
  await writeFile(join(root, "provider-response.sse"), providerFrames.join(""));
  const digest = (bytes) => createHash("sha256").update(bytes).digest("hex");
  const prefixes = [await realpath(root), root].sort((a, b) => b.length - a.length);
  const sanitize = (value) => {
    if (typeof value === "string")
      return prefixes.reduce((text, prefix) => text.replaceAll(prefix, "/workspace/pi-capture"), value);
    if (Array.isArray(value)) return value.map(sanitize);
    if (value && typeof value === "object")
      return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, sanitize(item)]));
    return value;
  };
  const jsonl = (bytes) => Buffer.from(bytes.toString("utf8").trimEnd().split("\n")
    .map((line) => JSON.stringify(sanitize(JSON.parse(line)))).join("\n") + "\n");
  const retainedRpc = jsonl(encodedRpc);
  const retainedNative = jsonl(native);
  const retainedStdout = jsonl(rawStdout);
  await writeFile(join(root, "rpc-success.jsonl"), retainedRpc);
  await writeFile(join(root, "native-success.jsonl"), retainedNative);
  await writeFile(join(root, "stdout-success.jsonl"), retainedStdout);
  await writeFile(join(root, "capture.json"), JSON.stringify({
    vendor: "pi", version, provider: "deterministic loopback OpenAI-compatible SSE fixture",
    expected_text: "SYLPH_PI_AA_OK", remote_model: false, tools_enabled: false,
    source_sha256: { rpc: digest(encodedRpc), stdout: digest(rawStdout), native: digest(native) },
    retained_sha256: { rpc: digest(retainedRpc), stdout: digest(retainedStdout), native: digest(retainedNative) },
    sanitization: "Parse JSON records, replace only temporary capture path prefixes, serialize compact JSONL. Preserve IDs, values, event order and duplicate delta occurrences. RPC direction wrappers are recorder metadata; stdout is captured independently as exact source bytes before sanitization.",
  }, null, 2) + "\n");
  console.log(root);
} finally {
  clearTimeout(timeout);
  child?.kill("SIGTERM");
  if (childExit) await childExit;
  server.closeAllConnections();
  await new Promise((resolve) => server.close(resolve));
}
