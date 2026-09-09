/**
 * Start the E2E server and a separately compiled envagent, pair them, then keep
 * both children alive for Playwright's webServer lifetime.
 *
 * This is deliberately a process boundary. The Milestone 1 contract says the
 * server's own machine is an ordinary environment, so a green browser suite
 * must cross `sylph.env.v1` instead of reaching a linked adapter in the server.
 */
import { spawn } from "node:child_process";
import http from "node:http";
import fs from "node:fs";
import path from "node:path";
import os from "node:os";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const repo = path.resolve(here, "..", "..");
const backendPort = Number(process.env.SYLPH_PORT ?? 4327);
const envPort = Number(process.env.SYLPH_ENV_PORT ?? backendPort + 1);
const backend = `http://127.0.0.1:${backendPort}`;
const envHome = path.join(here, "..", ".e2e-env-home");
const agentHome = path.join(envHome, "home");
// The server's isolated HOME is still the developer home in this process, while
// the fixture envagent uses its own HOME for credentials.  Browse roots describe
// the machine's usable paths, so include both homes explicitly along with the
// temporary roots used by the specs.
const browseRoots = [process.env.HOME ?? "/Users/hsw", "/tmp", os.tmpdir(), os.tmpdir().startsWith("/var/") ? `/private${os.tmpdir()}` : ""].filter(Boolean).join(":");
// Keep Cargo usable after giving the envagent an isolated HOME. Rustup resolves
// its toolchain from HOME when these variables are absent.
const toolchainEnv = {
  HOME: agentHome,
  RUSTUP_HOME: process.env.RUSTUP_HOME ?? path.join(process.env.HOME ?? "", ".rustup"),
  CARGO_HOME: process.env.CARGO_HOME ?? path.join(process.env.HOME ?? "", ".cargo"),
};
// The envagent owns a credential and a durable spool, so its root needs the same
// per-run reset as the server home. Guard the computed path before deleting it;
// this is a fixture directory, never an operator's real environment root.
if (path.basename(envHome) !== ".e2e-env-home") {
  throw new Error(`refusing to wipe unexpected envagent root: ${envHome}`);
}
if (process.env.SYLPH_E2E_KEEP_HOME !== "1") {
  fs.rmSync(envHome, { recursive: true, force: true });
}
fs.mkdirSync(agentHome, { recursive: true });

const children = new Set();
const restartMarker = path.join(envHome, "restart-agent");
const restartReceipt = path.join(envHome, "restart-complete");
let agentProcess;
let restarting = false;
let stopping = false;

function launch(command, args, extraEnv = {}, processGroup = false) {
  const child = spawn(command, args, {
    cwd: repo,
    env: { ...process.env, ...extraEnv },
    stdio: "inherit",
    detached: processGroup,
  });
  child.detached = processGroup;
  children.add(child);
  child.once("exit", (code, signal) => {
    children.delete(child);
    if (!stopping && !restarting && code !== 0) {
      console.error(`[e2e] child ${command} exited (${code ?? signal})`);
      shutdown(code ?? 1);
    }
  });
  return child;
}

function shutdown(code = 0) {
  if (stopping) return;
  stopping = true;
  for (const child of children) {
    if (child.spawnargs && child.detached) {
      try { process.kill(-child.pid, "SIGTERM"); } catch { child.kill("SIGTERM"); }
    } else child.kill("SIGTERM");
  }
  setTimeout(() => {
    for (const child of children) {
      if (child.spawnargs && child.detached) {
        try { process.kill(-child.pid, "SIGKILL"); } catch { child.kill("SIGKILL"); }
      } else child.kill("SIGKILL");
    }
    process.exit(code);
  }, 2_000).unref();
}

process.once("SIGINT", () => shutdown(130));
process.once("SIGTERM", () => shutdown(143));

function request(method, pathname, { cookie, body } = {}) {
  return new Promise((resolve, reject) => {
    const payload = body === undefined ? undefined : JSON.stringify(body);
    const req = http.request(
      `${backend}${pathname}`,
      {
        method,
        headers: {
          ...(payload ? { "content-type": "application/json", "content-length": Buffer.byteLength(payload) } : {}),
          ...(cookie ? { cookie } : {}),
        },
      },
      (res) => {
        const chunks = [];
        res.on("data", (chunk) => chunks.push(chunk));
        res.on("end", () => {
          const text = Buffer.concat(chunks).toString("utf8");
          let value;
          try {
            value = text ? JSON.parse(text) : undefined;
          } catch {
            value = text;
          }
          resolve({ status: res.statusCode ?? 0, headers: res.headers, value });
        });
      },
    );
    req.once("error", reject);
    if (payload) req.write(payload);
    req.end();
  });
}

async function waitForHealth() {
  for (let attempt = 0; attempt < 900; attempt += 1) {
    try {
      const response = await request("GET", "/api/v1/health");
      if (response.status === 200) return;
    } catch {
      // The cargo process is still compiling or binding its listener.
    }
    await new Promise((resolve) => setTimeout(resolve, 200));
  }
  throw new Error("backend did not become healthy within three minutes");
}

async function pair() {
  const token = await request("GET", "/api/v1/auth/token");
  if (token.status !== 200) throw new Error(`auth bootstrap failed (${token.status})`);
  const setCookie = token.headers["set-cookie"]?.[0];
  if (!setCookie) throw new Error("auth bootstrap did not set a cookie");
  const cookie = setCookie.split(";", 1)[0];
  const offer = await request("POST", "/api/v1/environments/pairings", { cookie, body: {} });
  if (offer.status !== 201 || !offer.value?.code) {
    throw new Error(`pairing offer failed (${offer.status}): ${JSON.stringify(offer.value)}`);
  }
  const pairChild = launch(
    "cargo",
    ["run", "--quiet", "-p", "sylph-envagent", "--features", "e2e", "--", "pair", "--server", backend, "--code", offer.value.code],
    {
      ...toolchainEnv,
      SYLPH_ENV_HOME: envHome,
      SYLPH_ENV_SERVER: `127.0.0.1:${envPort}`,
      SYLPH_BROWSE_ROOTS: browseRoots,
    },
  );
  const result = await new Promise((resolve) => pairChild.once("exit", (code) => resolve(code ?? 1)));
  if (result !== 0) throw new Error(`envagent pair exited with ${result}`);
}

function agentEnvironment() {
  return {
    ...toolchainEnv,
    SYLPH_ENV_HOME: envHome,
    SYLPH_ENV_SERVER: `127.0.0.1:${envPort}`,
    SYLPH_BROWSE_ROOTS: browseRoots,
  };
}

function startAgent() {
  return launch(
    "cargo",
    ["run", "--quiet", "-p", "sylph-envagent", "--features", "e2e", "--", "run"],
    agentEnvironment(),
    true,
  );
}

async function waitForConnectedEnvironment() {
  for (let attempt = 0; attempt < 300; attempt += 1) {
    const response = await request("GET", "/api/v1/auth/token");
    const cookie = response.headers["set-cookie"]?.[0]?.split(";", 1)[0];
    const environments = await request("GET", "/api/v1/environments", { cookie });
    const rows = environments.value?.environments ?? [];
    if (rows.some((row) => row.connected && row.agents?.some((agent) => agent.name === "e2e" && agent.available))) return;
    await new Promise((resolve) => setTimeout(resolve, 200));
  }
  throw new Error("paired envagent did not report a connected e2e agent");
}

try {
  launch("cargo", ["run", "--quiet", "-p", "sylph-server", "--features", "e2e", "--bin", "sylph-server"], {
    SYLPH_ENV_LISTEN: `127.0.0.1:${envPort}`,
    SYLPH_BROWSE_ROOTS: browseRoots,
  });
  await waitForHealth();
  await pair();
  agentProcess = startAgent();
  await waitForConnectedEnvironment();
  console.log("[e2e] paired envagent is connected and reports the e2e adapter");
  // A browser fault-injection spec can request a real process restart by creating
  // this marker. Polling keeps the control local and avoids adding a product API.
  setInterval(async () => {
    if (stopping || restarting || !fs.existsSync(restartMarker)) return;
    fs.rmSync(restartMarker, { force: true });
    restarting = true;
    const old = agentProcess;
    if (old) {
      try {
        process.kill(-old.pid, "SIGTERM");
      } catch {
        old.kill("SIGTERM");
      }
      await new Promise((resolve) => old.once("exit", resolve));
    }
    agentProcess = startAgent();
    restarting = false;
    try {
      await waitForConnectedEnvironment();
      fs.writeFileSync(restartReceipt, `${Date.now()}\n`);
      console.log("[e2e] envagent reconnected after requested restart");
    } catch (error) {
      console.error(`[e2e] envagent restart failed: ${error instanceof Error ? error.message : error}`);
      shutdown(1);
    }
  }, 100);
  await new Promise(() => {});
} catch (error) {
  console.error(`[e2e] stack startup failed: ${error instanceof Error ? error.message : error}`);
  shutdown(1);
}
