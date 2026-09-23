import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import http from "node:http";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { createOpsail, gateway, OpsailError } from "../src/index.js";
import { parseGatewayResponse } from "../src/gateway.js";

const binaryPath = fileURLToPath(new URL(`../../../target/debug/opsail${process.platform === "win32" ? ".exe" : ""}`, import.meta.url));
const passphrase = "test-vault-passphrase";
const key = "test-only-provider-key";

test("gateway persists across processes and CLI and Node share the protocol", { timeout: 240_000 }, async (t) => {
  const dataDir = mkdtempSync(path.join(os.tmpdir(), "opsail-gateway-"));
  t.after(() => rmSync(dataDir, { recursive: true, force: true }));
  const client = createOpsail({ binaryPath, hardTimeoutMs: 120_000 });
  assert.equal(typeof gateway, "function");
  await client.gateway({ operation: "init" }, { dataDir, passphrase });
  const server = http.createServer((req, res) => {
    assert.equal(req.headers.authorization, `Bearer ${key}`);
    res.setHeader("content-type", "application/json");
    res.end(JSON.stringify({ data: [{ id: "local-fixture" }] }));
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  t.after(() => { server.closeAllConnections(); server.close(); });
  const baseUrl = `http://127.0.0.1:${server.address().port}/v1`;
  const saved = await client.gateway({ operation: "set", connection: {
    name: "local", adapter: "openai-compatible", baseUrl, auth: { type: "bearer", key }, defaultModel: "local-fixture",
  } }, { dataDir, passphrase });
  assert.equal(saved.data.hasKey, true);
  assert(!JSON.stringify(saved).includes(key));
  const bytes = readFileSync(path.join(dataDir, "vault.age"));
  assert(!bytes.includes(key)); assert(!bytes.includes(passphrase)); assert(!bytes.includes(baseUrl));
  const request = { operation: "models", connection: "local" };
  const node = await client.gateway(request, { dataDir, passphrase });
  const native = await machine({ protocolVersion: 1, request, dataDir, passphrase });
  assert.equal(native.exitCode, 0);
  assert.deepEqual(native.response.result.data, node.data);
  assert.equal(node.httpStatus, 200);
  assert.equal(native.stderr, "");
  await assert.rejects(client.gateway({ operation: "list" }, { dataDir, passphrase: "wrong" }), (e) => {
    assert(e instanceof OpsailError); assert.equal(e.code, "vault-unlock-failed");
    assert(!JSON.stringify(e).includes(passphrase)); return true;
  });
  const replacement = "replacement-test-passphrase";
  await client.gateway({ operation: "rekey" }, { dataDir, passphrase, newPassphrase: replacement });
  const reopened = await createOpsail({ binaryPath, hardTimeoutMs: 120_000 }).gateway({ operation: "list" }, { dataDir, passphrase: replacement });
  assert.equal(reopened.data[0].name, "local");
});

test("malformed and oversized machine input never echo secrets", async () => {
  for (const input of ["not-json-private-secret", JSON.stringify({ protocolVersion: 2, request: { operation: "list" }, passphrase }), "x".repeat(1024*1024+1)]) {
    const native = await machine(input);
    assert.equal(native.exitCode, 1); assert.equal(native.response.ok, false);
    assert(!JSON.stringify(native).includes(passphrase)); assert(!native.stderr.includes("private-secret"));
  }
});

test("gateway protocol failures retain status and reject inconsistent exits", () => {
  const response = { protocolVersion: 1, engine: { name: "opsail", version: "test" }, ok: false,
    error: { code: "http-error", stage: "acquire", message: "provider failed", retryable: false, httpStatus: 403, providerCode: "customer_verification_required", elapsedMs: 5 } };
  assert.throws(() => parseGatewayResponse(Buffer.from(JSON.stringify(response)), 1, null), (e) => {
    assert.equal(e.httpStatus, 403); assert.equal(e.providerCode, "customer_verification_required"); return true;
  });
  assert.throws(() => parseGatewayResponse(Buffer.from(JSON.stringify(response)), 0, null), { code: "protocol-mismatch" });
  assert.throws(() => parseGatewayResponse(Buffer.from(`malformed ${key}`), 1, null), (e) => {
    assert(!String(e).includes(key)); assert.equal(e.cause, undefined); return true;
  });
});

test("gateway validates and aborts before spawning", async () => {
  const client = createOpsail({ binaryPath: "/missing/opsail" });
  await assert.rejects(client.gateway({ operation: "list" }, {}), { code: "invalid-request" });
  const controller = new AbortController(); controller.abort();
  await assert.rejects(client.gateway({ operation: "list" }, { passphrase, signal: controller.signal }), { code: "aborted" });
});

test("gateway sends credentials only on stdin and suppresses private process diagnostics", { skip: process.platform === "win32" }, async (t) => {
  const dir = mkdtempSync(path.join(os.tmpdir(), "opsail-gateway-process-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const binary = path.join(dir, "fake");
  writeFileSync(binary, `#!${process.execPath}\nlet s='';process.stdin.on('data', c=>s+=c);process.stdin.on('end',()=>{if(JSON.stringify(process.argv).includes('test-vault')) process.exit(4);process.stderr.write(s);process.stdout.write('invalid '+s);});`, { mode: 0o700 });
  const client = createOpsail({ binaryPath: binary });
  await assert.rejects(client.gateway({ operation: "list" }, { passphrase }), (e) => {
    assert.equal(e.code, "invalid-response"); assert.equal(e.diagnostic, undefined); assert.equal(e.cause, undefined);
    assert(!String(e).includes(passphrase)); return true;
  });
  writeFileSync(binary, `#!${process.execPath}\nprocess.stdin.resume();process.stdin.on('end',()=>{process.stderr.write('test-vault-passphrase');setInterval(()=>{},1000);});`, { mode: 0o700 });
  const controller = new AbortController();
  const pending = client.gateway({ operation: "list" }, { passphrase, signal: controller.signal });
  setTimeout(() => controller.abort(), 100);
  await assert.rejects(pending, { code: "aborted" });
  await assert.rejects(createOpsail({ binaryPath: binary, hardTimeoutMs: 100 }).gateway({ operation: "list" }, { passphrase }), (e) => {
    assert.equal(e.code, "process-timeout"); assert.equal(e.diagnostic, undefined); return true;
  });
  writeFileSync(binary, `#!${process.execPath}\nprocess.stdin.resume();process.stdin.on('end',()=>process.stdout.write('x'.repeat(5000)));`, { mode: 0o700 });
  await assert.rejects(createOpsail({ binaryPath: binary, maxOutputBytes: 100 }).gateway({ operation: "list" }, { passphrase }), { code: "output-limit-exceeded" });
});

function machine(payload) {
  return new Promise((resolve, reject) => {
    const child = spawn(binaryPath, ["gateway", "--machine"], { stdio: ["pipe", "pipe", "pipe"] });
    let stdout = "", stderr = "";
    child.stdout.on("data", c => stdout += c); child.stderr.on("data", c => stderr += c);
    child.stdin.on("error", () => {}); child.on("error", reject);
    child.on("close", exitCode => { try { resolve({ exitCode, response: JSON.parse(stdout), stderr }); } catch { reject(new Error("Invalid machine response")); } });
    child.stdin.end(typeof payload === "string" ? payload : JSON.stringify(payload));
  });
}
