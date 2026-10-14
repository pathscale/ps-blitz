import { mkdtemp, mkdir, rm } from "node:fs/promises";
import { lookup } from "node:dns/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

const repository = resolve(import.meta.dir, "..");
const assets = join(repository, "apps/chuzz/src/resource_policy/qa");
const qa = process.env.PS_QA_BIN || join(dirname(repository), "ps-observability/target/release/ps-qa");
const host = process.env.QA_HOST_BIN || join(repository, "target/release/chuzz-headless");
const publicHost = process.env.QA_RESOURCE_PUBLIC_HOST;
if (!publicHost || !/^[a-z0-9.-]+$/i.test(publicHost) ||
    /(?:^|\.)(?:localhost|local)\.?$/i.test(publicHost) || /^\d+(?:\.\d+){3}$/.test(publicHost)) {
  throw new Error("QA_RESOURCE_PUBLIC_HOST must be a public-looking DNS name mapped to 127.0.0.1 by the witness environment");
}
// This validates fixture plumbing, not policy. Chuzz performs its own DNS
// resolution and receives the actual loopback address before connecting.
const addresses = await lookup(publicHost, { all: true });
if (!addresses.length || addresses.some(address => address.address !== "127.0.0.1")) {
  throw new Error("the witness public name must resolve exclusively to the local fixture's 127.0.0.1");
}
const work = await mkdtemp(join(tmpdir(), "chuzz-resource-policy-"));
const children: Bun.Subprocess[] = [];
const template = await Bun.file(join(assets, "index.html")).text();
const canary = join(work, "resource-canary.js");
await Bun.write(canary, 'globalThis.__resourceFileCanary = "RESOURCE_FILE_CANARY";\n');
const file = pathToFileURL(canary).href;
let own = "";
let loopback = "";
let resolved = "";

function html(mode: string): string {
  const fixture = JSON.stringify({ mode, file, own, loopback, resolved }).replaceAll("<", "\\u003c");
  return template.replace("__RESOURCE_FIXTURE__", fixture);
}

function response(request: Request, foreign: boolean): Response {
  const url = new URL(request.url);
  const origin = request.headers.get("origin") || "*";
  const headers = new Headers({
    "access-control-allow-origin": origin,
    "access-control-allow-credentials": "true",
    "cache-control": "no-store",
  });
  if (url.pathname === "/seed") {
    headers.set("set-cookie", "resource_seed=RESOURCE_COOKIE_CANARY; Path=/; HttpOnly");
    headers.set("location", own + "/index.html?mode=credentials");
    return new Response(null, { status: 302, headers });
  }
  if (url.pathname === "/echo") {
    if (foreign) headers.set("set-cookie", "resource_foreign=FOREIGN_CANARY; Path=/; HttpOnly");
    headers.set("content-type", "application/json");
    return new Response(JSON.stringify({ cookie: request.headers.get("cookie") || "" }), { headers });
  }
  if (url.pathname === "/probe") return new Response("PRIVATE_PEER_REACHED", { headers });
  if (url.pathname === "/tiny") return new Response("ok", { headers });
  if (url.pathname === "/chunk") return new Response("x".repeat(6000), { headers });
  if (url.pathname === "/big") return new Response("x".repeat(65536), { headers });
  headers.set("content-type", "text/html; charset=utf-8");
  return new Response(html(url.searchParams.get("mode") || "file"), { headers });
}

const pageServer = Bun.serve({ hostname: "127.0.0.1", port: 0, fetch: request => response(request, false) });
const targetServer = Bun.serve({ hostname: "127.0.0.1", port: 0, fetch: request => response(request, true) });
own = `http://127.0.0.1:${pageServer.port}`;
loopback = `http://127.0.0.1:${targetServer.port}`;
resolved = `http://${publicHost}:${targetServer.port}`;

function cells(line: string): string[] {
  const values: string[] = [];
  let start = 0, quoted = false, escaped = false;
  for (let index = 0; index < line.length; index++) {
    const character = line[index];
    if (escaped) { escaped = false; continue; }
    if (quoted && character === "\\") { escaped = true; continue; }
    if (character === '"') { quoted = !quoted; continue; }
    if (character === "," && !quoted) { values.push(line.slice(start, index).trim()); start = index + 1; }
  }
  if (quoted || escaped) throw new Error("malformed ps-qa TOON cell");
  values.push(line.slice(start).trim());
  return values.map(value => value.startsWith('"') ? String(JSON.parse(value)) : value);
}

function report(stdout: string): Record<string, string>[] {
  const tables = [...stdout.matchAll(/^checks\[(\d+)\]\{([^}]+)\}:\r?$/gm)];
  if (tables.length !== 1) throw new Error("expected exactly one ps-qa checks table");
  const table = tables[0];
  const count = Number(table[1]);
  const columns = table[2].split(",").map(value => value.trim());
  if (!["id", "verdict", "error"].every(column => columns.includes(column))) throw new Error("missing ps-qa columns");
  const lines = stdout.slice(table.index! + table[0].length).split(/\r?\n/).slice(1, count + 1);
  if (lines.length !== count) throw new Error("truncated ps-qa report");
  return lines.map(line => {
    if (!line.startsWith("  ")) throw new Error("invalid ps-qa row");
    const row = cells(line.trim());
    if (row.length !== columns.length) throw new Error("invalid ps-qa row width");
    return Object.fromEntries(columns.map((column, index) => [column, row[index]]));
  });
}

async function bounded<T>(operation: Promise<T>, name: string, ms: number): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([operation, new Promise<never>((_, reject) => {
      timer = setTimeout(() => reject(new Error(`${name} exceeded ${ms}ms`)), ms);
    })]);
  } finally { clearTimeout(timer); }
}

const modes = ["file", "private-address", "credentials", "request-cap", "byte-cap"];
const mutants = new Map(modes.map(mode => [mode, process.env[`QA_RESOURCE_DEFECT_${mode.toUpperCase().replaceAll("-", "_")}_BIN`]]));
if ([...mutants.values()].some(binary => !binary)) {
  pageServer.stop(true); targetServer.stop(true);
  await rm(work, { recursive: true, force: true });
  throw new Error("supply all five prebuilt QA_RESOURCE_DEFECT_<RULE>_BIN executables; see defects.json");
}

async function drive(mode: string, binary: string, defect: boolean): Promise<void> {
  const profile = await mkdtemp(join(work, `${mode}-${defect ? "defect" : "normal"}-`));
  const control = join(profile, "control");
  await mkdir(control, { mode: 0o700 });
  let page: string;
  if (mode === "file-allowed") {
    const local = join(profile, "local.html");
    await Bun.write(local, html(mode));
    page = pathToFileURL(local).href;
  } else if (mode === "credentials") {
    page = loopback + "/seed";
  } else {
    const origin = ["file", "private-address"].includes(mode) ? `http://${publicHost}:${pageServer.port}` : own;
    page = origin + "/index.html?mode=" + mode;
  }
  const child = Bun.spawn([qa, "qa-hosted", "--host", binary, "--page", page,
    "--app", join(assets, "ps-qa.ron"), "--checks", join(assets, "checks", mode + ".ron")], {
    stdin: "ignore", stdout: "pipe", stderr: "pipe",
    env: { ...process.env, PS_QA_HEADLESS: "true", CHUZZ_PROFILE_DIR: profile, CHUZZ_CONTROL_DIR: control,
      CHUZZ_RESOURCE_MAX_REQUESTS: "12", CHUZZ_RESOURCE_WARN_REQUESTS: "6",
      CHUZZ_RESOURCE_MAX_BYTES: "16384", CHUZZ_RESOURCE_WARN_BYTES: "4096" },
  });
  children.push(child);
  const [code, stdout, stderr] = await bounded(Promise.all([
    child.exited, new Response(child.stdout).text(), new Response(child.stderr).text()
  ]), mode, 90000);
  process.stdout.write(stdout); process.stderr.write(stderr);
  const rows = report(stdout);
  const expected = ["resource-policy-ready", `resource-policy-${mode}`];
  if (rows.length !== expected.length || new Set(rows.map(row => row.id)).size !== expected.length ||
      expected.some(id => !rows.some(row => row.id === id))) throw new Error(`${mode}: wrong checks`);
  for (const row of rows) {
    const verdict = defect && row.id === `resource-policy-${mode}` ? "fail" : "pass";
    if (row.verdict !== verdict) throw new Error(`${mode}: ${row.id} expected ${verdict}: ${row.error}`);
  }
  if (code !== (defect ? 1 : 0)) throw new Error(`${mode}: unexpected ps-qa exit ${code}`);
  if (!defect && mode === "request-cap" && !(stdout + stderr).includes("resource-request-warning")) {
    throw new Error("request warning threshold was not observed");
  }
  if (!defect && mode === "byte-cap" && !(stdout + stderr).includes("resource-byte-warning")) {
    throw new Error("byte warning threshold was not observed");
  }
  console.log(defect ? `PASS: planted ${mode} defect failed by name resource-policy-${mode}` : `PASS: ${mode}`);
}

try {
  await drive("file-allowed", host, false);
  for (const mode of modes) {
    await drive(mode, host, false);
    await drive(mode, mutants.get(mode)!, true);
  }
} finally {
  for (const child of children) {
    if (child.exitCode === null) child.kill("SIGTERM");
  }
  await Promise.all(children.map(child => bounded(child.exited, "shutdown", 5000).catch(async () => {
    if (child.exitCode === null) child.kill("SIGKILL");
    await child.exited;
  })));
  pageServer.stop(true); targetServer.stop(true);
  await rm(work, { recursive: true, force: true });
}
