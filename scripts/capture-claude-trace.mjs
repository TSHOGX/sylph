#!/usr/bin/env node
// Real Claude framing and persistence with controlled loopback model responses.
import { createServer } from "node:http";
import { spawn, execFileSync } from "node:child_process";
import { once } from "node:events";
import { createInterface } from "node:readline";
import { mkdtemp, mkdir, writeFile, readFile, readdir, realpath, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createHash } from "node:crypto";

const scenario = process.argv[2] ?? "read";
if (!["read", "sidecar", "schedule", "usage"].includes(scenario)) throw new Error("Expected read, sidecar, schedule, or usage scenario");
const root = await mkdtemp(join(tmpdir(), "sylph-claude-loopback-"));
const config = join(root, "config");
await mkdir(config);
await writeFile(join(root, "marker.txt"), "SYLPH_CLAUDE_TOOL_BYTES\n");
const tool = scenario === "usage" ? "" : scenario === "schedule" ? "CronCreate,CronDelete,CronList" : scenario === "read" ? "Read" : "Bash";
const requestCount = scenario === "schedule" ? 6 : 2;
let scheduleId;
let receiveCreation;
const creation = new Promise((resolve) => { receiveCreation = resolve; });
const argumentsValue = scenario === "read"
  ? { file_path: join(root, "marker.txt") }
  : { command: "python3 -c 'import sys; sys.stdout.write(\"SYLPH_CLAUDE_SIDECAR\\n\" * 8192)'", description: "Emit controlled local sidecar bytes" };
const requests = [];
const providerFrames = [];
let streamError;
const server = createServer(async (request, response) => {
  try {
    const chunks = [];
    let length = 0;
    for await (const chunk of request) {
      length += chunk.length;
      if (length > 2 * 1024 * 1024) throw new Error("Provider request exceeds capture bound");
      chunks.push(chunk);
    }
    if (request.method !== "POST" || new URL(request.url, "http://localhost").pathname !== "/v1/messages") {
      response.writeHead(404).end();
      return;
    }
    if (request.headers["x-api-key"] !== "local-fixture") throw new Error("Provider authentication differs from the isolated fixture key");
    const body = JSON.parse(Buffer.concat(chunks));
    requests.push(body);
    const ordinal = requests.length;
    if (ordinal > requestCount) throw new Error("Unexpected additional model request");
    if (scenario === "schedule" && ordinal === 2) { await creation; if (!scheduleId) throw new Error("Schedule creation did not return an identity"); }
    const invoke = scenario !== "usage" && ordinal < requestCount;
    const activeTool = scenario !== "schedule" ? tool : ordinal === 1 ? "CronCreate" : ordinal === 2 || ordinal === 4 ? "CronList" : "CronDelete";
    const activeArguments = scenario !== "schedule" ? argumentsValue : ordinal === 1
      ? { cron: "0 0 1 1 *", prompt: "Controlled inert fixture; no external actions.", recurring: false, durable: false }
      : ordinal === 3 ? { id: scheduleId } : ordinal === 5 ? { id: "missing-fixture-task" } : {};
    const callId = scenario === "schedule" ? `toolu_schedule_${ordinal}` : "toolu_controlled";
    response.writeHead(200, { "content-type": "text/event-stream" });
    const emit = (type, fields) => {
      const frame = `event: ${type}\ndata: ${JSON.stringify({ type, ...fields })}\n\n`;
      providerFrames.push(frame);
      response.write(frame);
    };
    emit("message_start", { message: {
      id: `msg_controlled_${ordinal}`, type: "message", role: "assistant", model: body.model,
      content: [], stop_reason: null, stop_sequence: null, usage: { input_tokens: scenario === "usage" ? ordinal * 10 : 1, output_tokens: 0 },
    } });
    emit("content_block_start", { index: 0, content_block: invoke
      ? { type: "tool_use", id: callId, name: activeTool, input: {} }
      : { type: "text", text: "" } });
    emit("content_block_delta", { index: 0, delta: invoke
      ? { type: "input_json_delta", partial_json: JSON.stringify(activeArguments) }
      : { type: "text_delta", text: "SYLPH_CLAUDE_TOOL_OK" } });
    emit("content_block_stop", { index: 0 });
    emit("message_delta", { delta: { stop_reason: invoke ? "tool_use" : "end_turn", stop_sequence: null }, usage: { output_tokens: scenario === "usage" ? ordinal : 1 } });
    emit("message_stop", {});
    response.end();
  } catch (error) {
    streamError = error;
    response.destroy(error);
  }
});
server.listen(0, "127.0.0.1");
await once(server, "listening");
const env = {
  PATH: process.env.PATH, CLAUDE_CONFIG_DIR: config,
  ANTHROPIC_BASE_URL: `http://127.0.0.1:${server.address().port}`,
  ANTHROPIC_API_KEY: "local-fixture", CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: "1",
  DISABLE_AUTOUPDATER: "1", NO_COLOR: "1",
};
let child;
let timeout;
try {
  const version = execFileSync("claude", ["--version"], { env, encoding: "utf8" }).trim();
  child = spawn("claude", [
    ...(scenario === "schedule" ? [] : ["--bare"]), "--safe-mode", "--disable-slash-commands", "--print", "--verbose",
    "--output-format", "stream-json", "--include-partial-messages", "--model", "claude-sonnet-4-6",
    "--tools", tool, "--allowedTools", tool, "--setting-sources", "", "--strict-mcp-config",
    "--mcp-config", '{"mcpServers":{}}', "--system-prompt", "Use the controlled local tool and return the marker.",
    ...(scenario === "usage" ? ["--input-format", "stream-json"] : ["Run the controlled local fixture tool and return the marker."]),
  ], { cwd: root, env, stdio: [scenario === "usage" ? "pipe" : "ignore", "pipe", "pipe"] });
  const output = [];
  const errors = [];
  let bytes = 0;
  const retain = (list, chunk) => {
    bytes += chunk.length;
    if (bytes > 8 * 1024 * 1024) {
      streamError = new Error("Claude capture exceeds its byte bound");
      child.kill("SIGTERM");
    } else list.push(chunk);
  };
  child.stdout.on("data", (chunk) => retain(output, chunk));
  if (scenario === "schedule") createInterface({ input: child.stdout }).on("line", (line) => {
    try {
      const frame = JSON.parse(line);
      if (frame.type === "user" && frame.message.content?.some((part) => part.tool_use_id === "toolu_schedule_1")) {
        scheduleId = frame.tool_use_result?.id;
        receiveCreation();
      }
    } catch (error) { streamError = error; child.kill("SIGTERM"); }
  });
  if (scenario === "usage") {
    let completed = 0;
    const send = (index) => child.stdin.write(JSON.stringify({ type: "user", message: { role: "user", content: `Accounting fixture ${index}` } }) + "\n");
    createInterface({ input: child.stdout }).on("line", (line) => {
      try { if (JSON.parse(line).type === "result") { completed += 1; if (completed === 1) send(2); else child.stdin.end(); } }
      catch (error) { streamError = error; child.kill("SIGTERM"); }
    });
    send(1);
  }
  child.stderr.on("data", (chunk) => retain(errors, chunk));
  timeout = setTimeout(() => {
    streamError = new Error("Claude capture exceeded 30 seconds");
    child.kill("SIGTERM");
  }, 30_000);
  const [code, signal] = await once(child, "exit");
  const stdout = Buffer.concat(output);
  await writeFile(join(root, "stdout-original.jsonl"), stdout);
  await writeFile(join(root, "stderr.txt"), Buffer.concat(errors));
  if (streamError) throw streamError;
  if (code !== 0 || requests.length !== requestCount) throw new Error(`Unexpected exit ${code}/${signal} or request count ${requests.length}; capture is in ${root}`);
  const frames = stdout.toString("utf8").trimEnd().split("\n").map((line) => JSON.parse(line));
  const init = frames.find((frame) => frame.type === "system" && frame.subtype === "init");
  if (JSON.stringify([...(init?.tools ?? [])].sort()) !== JSON.stringify(tool ? tool.split(",").sort() : []) || init.skills.length || init.plugins.length || init.mcp_servers.length)
    throw new Error("Capture did not isolate the requested built-in tool");
  const result = frames.find((frame) => Object.hasOwn(frame, "tool_use_result"));
  if (scenario !== "usage" && (!result || result.message.content.some((part) => part.is_error))) throw new Error("Expected a successful structured tool result");
  if (!frames.some((frame) => frame.type === "result" && frame.result === "SYLPH_CLAUDE_TOOL_OK" && !frame.is_error))
    throw new Error("Claude did not finish with the controlled marker");
  const relativeFiles = await readdir(join(config, "projects"), { recursive: true });
  const nativeFiles = relativeFiles.filter((file) => file.endsWith(`/${init.session_id}.jsonl`));
  if (nativeFiles.length !== 1) throw new Error("Expected one native transcript for the observed session");
  const native = await readFile(join(config, "projects", nativeFiles[0]));
  const nativeRows = native.toString("utf8").trimEnd().split("\n").map((line) => JSON.parse(line));
  const nativeResult = result && nativeRows.find((row) => row.uuid === result.uuid);
  if (result && JSON.stringify(nativeResult?.toolUseResult) !== JSON.stringify(result.tool_use_result))
    throw new Error("Live and native records did not preserve the observed structured result");
  const substitutions = [[await realpath(root), "/workspace/claude-capture"], [root, "/workspace/claude-capture"]];
  let sidecar;
  if (scenario === "read") {
    if (result.tool_use_result.file?.content !== "SYLPH_CLAUDE_TOOL_BYTES\n") throw new Error("Wrong read bytes");
  } else if (scenario === "sidecar") {
    const path = result.tool_use_result.persistedOutputPath;
    if (typeof path !== "string" || (await stat(path)).size > 8 * 1024 * 1024) throw new Error("Missing or oversized sidecar");
    sidecar = await readFile(path);
    if (!sidecar.equals(Buffer.from("SYLPH_CLAUDE_SIDECAR\n".repeat(8192)))) throw new Error("Sidecar content differs from controlled output");
    substitutions.push([path, "/workspace/claude-capture/sidecar.txt"]);
  }
  if (scenario === "schedule") {
    const results = frames.filter((frame) => Object.hasOwn(frame, "tool_use_result"));
    if (results.length !== 5 || results[0].tool_use_result.durable !== false
        || results[1].tool_use_result.jobs?.[0]?.id !== scheduleId
        || results[2].tool_use_result.id !== scheduleId
        || results[3].tool_use_result.jobs?.length !== 0
        || !results[4].message.content.some((part) => part.is_error))
      throw new Error("Schedule registration/list/deletion/failure cycle did not match observed results");
  }
  if (scenario === "usage") {
    const results = frames.filter((frame) => frame.type === "result");
    if (results.length !== 2 || results[0].usage.input_tokens !== 10 || results[1].usage.input_tokens !== 20
        || Object.values(results[0].modelUsage)[0].inputTokens !== 10 || Object.values(results[1].modelUsage)[0].inputTokens !== 30)
      throw new Error("The accounting capture did not observe query-local usage and cumulative model usage");
  }
  substitutions.sort((a, b) => b[0].length - a[0].length);
  const sanitize = (value) => {
    if (typeof value === "string") return substitutions.reduce((text, [from, to]) => text.replaceAll(from, to), value);
    if (Array.isArray(value)) return value.map(sanitize);
    if (value && typeof value === "object") return Object.fromEntries(Object.entries(value).map(([key, child]) => [key, sanitize(child)]));
    return value;
  };
  const jsonl = (rows) => Buffer.from(rows.map((row) => JSON.stringify(sanitize(row))).join("\n") + "\n");
  const retainedStdout = jsonl(frames);
  const retainedNative = jsonl(nativeRows);
  await writeFile(join(root, "stdout.jsonl"), retainedStdout);
  await writeFile(join(root, "native.jsonl"), retainedNative);
  if (sidecar) await writeFile(join(root, "sidecar.txt"), sidecar);
  await writeFile(join(root, "provider-requests.json"), JSON.stringify(requests, null, 2) + "\n");
  await writeFile(join(root, "provider-response.sse"), providerFrames.join(""));
  const digest = (bytes) => createHash("sha256").update(bytes).digest("hex");
  await writeFile(join(root, "capture.json"), JSON.stringify({
    vendor: "claude", version, scenario, session_id: init.session_id, result_uuid: result?.uuid ?? null,
    method: `scripts/capture-claude-trace.mjs ${scenario}; real CLI with a deterministic loopback provider and ${tool || "no tools"} enabled. Isolated configuration, safe mode, disabled skills and an explicitly checked local API key isolate the capture. Bare mode is also used except for schedules, which it disables. No remote model or external messages.`,
    limits: "Provider tool calls, response text and usage are synthetic; CLI frames, native records, tool execution and sidecar bytes are observed. This capture proves those emitted shapes, not remote-model behavior.",
    sanitization: "Replace only temporary capture roots and the exact produced sidecar path, then compact JSON reserialization. All keys, IDs, values and event order remain. Sidecar bytes are unchanged.",
    source_sha256: { stdout: digest(stdout), native: digest(native), ...(sidecar ? { sidecar: digest(sidecar) } : {}) },
    retained_sha256: { stdout: digest(retainedStdout), native: digest(retainedNative), ...(sidecar ? { sidecar: digest(sidecar) } : {}) },
    records: { stdout: frames.length, native: nativeRows.length },
  }, null, 2) + "\n");
  console.log(root);
} finally {
  clearTimeout(timeout);
  if (child && child.exitCode === null && child.signalCode === null) {
    child.kill("SIGTERM");
    await once(child, "exit");
  }
  server.closeAllConnections();
  await new Promise((resolve) => server.close(resolve));
}
