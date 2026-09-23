import { OpsailError } from "./errors.js";

const operations = new Set(["init", "list", "set", "remove", "rekey", "request", "models", "chat", "evaluate"]);
const record = (v) => v !== null && typeof v === "object" && !Array.isArray(v);
const status = (v) => v === undefined || (Number.isInteger(v) && v >= 100 && v <= 599);

export function parseGatewayResponse(stdout, exitCode, signalCode) {
  // Malformed output and stderr can contain private stdin echoed by a failed
  // executable. Never attach either, or a JSON parser cause, to gateway errors.
  let response;
  try { response = JSON.parse(stdout.toString("utf8")); }
  catch { throw failure("invalid-response", "Opsail gateway returned no valid JSON response"); }
  if (!record(response) || response.protocolVersion !== 1) {
    throw failure("protocol-mismatch", "Unsupported gateway protocol response");
  }
  if (!record(response.engine) || response.engine.name !== "opsail" || typeof response.engine.version !== "string") {
    throw failure("invalid-response", "Invalid gateway engine envelope");
  }
  if (signalCode !== null || (response.ok === true ? exitCode !== 0 : exitCode !== 1)) {
    throw failure("protocol-mismatch", "Gateway response disagrees with process exit");
  }
  if (response.ok === true) {
    const r = response.result;
    if (!record(r) || r.schemaVersion !== 1 || !operations.has(r.operation) || !Object.hasOwn(r, "data")
        || !Number.isSafeInteger(r.elapsedMs) || r.elapsedMs < 0 || !status(r.httpStatus)
        || (r.connection !== undefined && typeof r.connection !== "string")) {
      throw failure("invalid-response", "Invalid gateway result");
    }
    return r;
  }
  const e = response.error;
  if (response.ok !== false || !record(e) || typeof e.code !== "string" || typeof e.message !== "string"
      || !["input", "vault", "acquire", "protocol"].includes(e.stage) || typeof e.retryable !== "boolean"
      || !status(e.httpStatus) || (e.providerCode !== undefined && typeof e.providerCode !== "string")
      || (e.elapsedMs !== undefined && (!Number.isSafeInteger(e.elapsedMs) || e.elapsedMs < 0))) {
    throw failure("invalid-response", "Invalid gateway error");
  }
  throw new OpsailError(e.message, e);
}

function failure(code, message) { return new OpsailError(message, { code, stage: "protocol" }); }
