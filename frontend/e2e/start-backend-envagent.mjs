/**
 * Start the E2E server and a separately compiled envagent, pair them, then keep
 * both children alive for Playwright's webServer lifetime.
 *
 * This is deliberately a process boundary. The Milestone 1 contract says the
 * server's own machine is an ordinary environment, so a green browser suite
 * must cross `sylph.env.v1` instead of reaching a linked adapter in the server.
 *
 * ## Two rules about this process tree, and why a run hangs without them
 *
 * Playwright spawns this launcher itself `detached` with **piped** stdio, and its
 * teardown is `process.kill(-launcherPid, "SIGKILL")` followed by `await`ing the
 * launcher's `close` event (`playwright-core/lib/coreBundle.js`, `launchProcess`).
 * `close` fires only when the process has exited *and* every one of those pipes
 * has reached EOF. Two consequences govern every spawn below:
 *
 * 1. **Nothing here may be `detached`.** A child in its own process group does not
 *    receive that group SIGKILL, and a child spawned `stdio: "inherit"` holds a
 *    duplicated fd of Playwright's stdout/stderr pipe. So a detached survivor keeps
 *    the write end open forever: the launcher exits, `close` never fires, and
 *    teardown blocks after every spec has already passed. Measured directly — with
 *    a detached grandchild, `exit` fires and `close` does not.
 * 2. **Nothing here may rely on a handler running at shutdown.** The signal is
 *    SIGKILL, which cannot be caught. The `SIGTERM` and stdin handlers below serve
 *    `Ctrl-C` and a graceful close; they are not what reaps this tree on a normal
 *    run. Correctness comes from group membership, which needs no cooperation.
 *
 * Both rules are why the binaries are **built first and spawned directly** rather
 * than run through `cargo run`. `cargo run` is a parent of the process we actually
 * want to control, so killing cargo's pid leaves the binary orphaned and holding
 * its port — which is what made a process group look necessary in the first place.
 * Removing the intermediary removes the need for the mechanism that broke teardown:
 * every child is now the real process, `child.kill()` reaches it, and one group
 * SIGKILL reaps everything.
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

// The two fixture binaries, by absolute path. Named once: the build step and every
// spawn have to agree on which files are ours, and a second spelling is a second
// thing to keep true. `CARGO_TARGET_DIR` is honoured because cargo honours it, and
// a launcher that ignored it would spawn a binary this run did not build.
const targetDir = process.env.CARGO_TARGET_DIR ?? path.join(repo, "target");
const serverBin = path.join(targetDir, "debug", "sylph-server");
const agentBin = path.join(targetDir, "debug", "sylph-envagent");

const children = new Set();
const restartMarker = path.join(envHome, "restart-agent");
const restartReceipt = path.join(envHome, "restart-complete");
let agentProcess;
let restarting = false;
let stopping = false;

/**
 * Spawn a long-lived child inside this launcher's own process group.
 *
 * No `detached`, for rule 1 in the file header. `stdin: "ignore"` because this
 * launcher reads its *own* stdin for Playwright's end-of-life signal, and a child
 * inheriting that descriptor would compete for those reads; neither binary wants
 * console input. stdout and stderr stay inherited so a failing spec still shows
 * backend logs through Playwright's reporter.
 */
function launch(command, args, extraEnv = {}) {
  const child = spawn(command, args, {
    cwd: repo,
    env: { ...process.env, ...extraEnv },
    stdio: ["ignore", "inherit", "inherit"],
  });
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

/**
 * Spawn a child that is *supposed* to exit, and resolve with its code.
 *
 * Separate from `launch` because the supervisor above treats any exit as a fault.
 * A one-shot command reporting failure should surface as a thrown error from its
 * own call site, naming what failed, rather than as a second shutdown racing the
 * first.
 */
function runToCompletion(command, args, extraEnv = {}) {
  const child = spawn(command, args, {
    cwd: repo,
    env: { ...process.env, ...extraEnv },
    stdio: ["ignore", "inherit", "inherit"],
  });
  children.add(child);
  return new Promise((resolve) => {
    child.once("exit", (code, signal) => {
      children.delete(child);
      resolve(code ?? (signal ? 1 : 0));
    });
  });
}

function shutdown(code = 0) {
  if (stopping) return;
  stopping = true;
  // Plain per-child signals: every child is the real process now, not a `cargo run`
  // standing in front of it, so there is no tree below them to chase.
  for (const child of children) child.kill("SIGTERM");
  setTimeout(() => {
    for (const child of children) child.kill("SIGKILL");
    process.exit(code);
  }, 2_000).unref();
}

process.once("SIGINT", () => shutdown(130));
process.once("SIGTERM", () => shutdown(143));
// Playwright closes the webServer stdin after the test process exits. Listen
// for that lifecycle signal as well as SIGTERM so the launcher cannot keep the
// npm command alive when a shell wrapper absorbs the signal.
process.stdin.once("end", () => shutdown(0));
process.stdin.resume();

/**
 * Compile both fixture binaries, in one cargo invocation, before anything starts.
 *
 * One invocation rather than two because both would contend for the same target
 * directory lock, and because `sylph-envagent/e2e` resolves once here — two passes
 * with different feature sets rebuild the adapters crate back and forth.
 *
 * A build failure has to surface *as* a build failure. Compiled lazily by a
 * `cargo run` the health poll is already waiting on, it instead presents as
 * "backend did not become healthy within three minutes", three minutes later.
 */
async function buildFixtureBinaries() {
  console.log("[e2e] building sylph-server and the e2e envagent");
  const started = Date.now();
  const code = await runToCompletion(
    "cargo",
    ["build", "--bin", "sylph-server", "--bin", "sylph-envagent", "--features", "sylph-envagent/e2e"],
    toolchainEnv,
  );
  if (code !== 0) throw new Error(`cargo build failed with ${code}`);
  for (const binary of [serverBin, agentBin]) {
    if (!fs.existsSync(binary)) throw new Error(`cargo build reported success but ${binary} is missing`);
  }
  console.log(`[e2e] built both binaries in ${Math.round((Date.now() - started) / 1000)}s`);
}

/*
 * There is deliberately no pre-flight sweep of leftover fixture processes here,
 * and the two reasons it was measured out are worth keeping.
 *
 * It looks necessary because `AGENTS.md` documents clearing these ports by hand
 * before diagnosing a startup failure. But a leaked envagent does not hold either
 * port: it *dials* the server, so what `lsof -i tcp:4328` reports for it is a client
 * socket, and a fresh server binds 4328 alongside it — verified against a real
 * orphan. Sweeping by port would therefore also match any foreign process merely
 * connected to the backend and fail a run over something harmless.
 *
 * Sweeping by executable cannot be made safe either. A developer's own envagent runs
 * the same `target/debug/sylph-envagent`, and the one fact that separates it from a
 * fixture — `SYLPH_ENV_HOME` — is unreadable on macOS, where `ps -E` returns nothing
 * for another process. So the sweep would sometimes kill real work to tidy a leftover
 * that costs a rejected hello and nothing else.
 *
 * What removes the need is upstream: no child is detached, so Playwright's group
 * SIGKILL reaps this whole tree and a normal run leaks nothing to sweep.
 */

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
  const result = await runToCompletion(
    agentBin,
    ["pair", "--server", backend, "--code", offer.value.code],
    agentEnvironment(),
  );
  if (result !== 0) throw new Error(`envagent pair exited with ${result}`);
}

function agentEnvironment() {
  return {
    ...toolchainEnv,
    // The envagent's credential/spool isolation is controlled by SYLPH_ENV_HOME.
    // HOME must remain the fixture machine's real home because PathPolicy always
    // treats HOME as the first browse root; pointing it at .e2e-env-home/home made
    // DirectoryPicker open an empty synthetic directory and hid /tmp and the test
    // worktrees behind later roots.
    HOME: process.env.HOME ?? "/Users/hsw",
    SYLPH_ENV_HOME: envHome,
    SYLPH_ENV_SERVER: `127.0.0.1:${envPort}`,
    SYLPH_BROWSE_ROOTS: browseRoots,
  };
}

function startAgent() {
  return launch(agentBin, ["run"], agentEnvironment());
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
  await buildFixtureBinaries();
  launch(serverBin, [], {
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
      // Signals the envagent itself, which is the process whose reconnect the spec
      // asserts on. Through `cargo run` this had to be a group kill, and a group is
      // exactly what teardown cannot tolerate here.
      old.kill("SIGTERM");
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
