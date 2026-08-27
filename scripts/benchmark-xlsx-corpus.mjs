#!/usr/bin/env node

import { spawn } from "node:child_process";
import { readdir, stat } from "node:fs/promises";
import { extname, join, resolve } from "node:path";
import { performance } from "node:perf_hooks";

function usage() {
  throw new Error(
    "usage: benchmark-xlsx-corpus.mjs [--binary PATH] [--concurrency N] [--max-bytes N] [--max-expanded-bytes N] ROOT",
  );
}

const args = process.argv.slice(2);
let binary = "opsail";
let concurrency = 8;
let maxBytes = 64 * 1024 * 1024;
let maxExpandedBytes = 512 * 1024 * 1024;
for (let index = 0; index < args.length; ) {
  const flag = args[index];
  if (flag === "--binary") {
    binary = args[index + 1] ?? usage();
    args.splice(index, 2);
  } else if (flag === "--concurrency") {
    concurrency = parseBoundedInteger(args[index + 1], 1, 64, flag);
    args.splice(index, 2);
  } else if (flag === "--max-bytes") {
    maxBytes = parseBoundedInteger(args[index + 1], 1, 1024 ** 3, flag);
    args.splice(index, 2);
  } else if (flag === "--max-expanded-bytes") {
    maxExpandedBytes = parseBoundedInteger(
      args[index + 1],
      1,
      4 * 1024 ** 3,
      flag,
    );
    args.splice(index, 2);
  } else {
    index += 1;
  }
}
if (args.length !== 1) usage();

function parseBoundedInteger(value, minimum, maximum, flag) {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < minimum || parsed > maximum) {
    throw new Error(`${flag} must be an integer from ${minimum} to ${maximum}`);
  }
  return parsed;
}

async function findWorkbooks(root) {
  const workbooks = [];
  const directories = [root];
  while (directories.length > 0) {
    const directory = directories.pop();
    const entries = await readdir(directory, { withFileTypes: true });
    entries.sort((left, right) => left.name.localeCompare(right.name));
    for (const entry of entries) {
      const path = join(directory, entry.name);
      if (entry.isDirectory()) {
        directories.push(path);
      } else if (
        entry.isFile() &&
        !entry.name.startsWith("~$") &&
        extname(entry.name).toLowerCase() === ".xlsx"
      ) {
        workbooks.push(path);
      }
    }
  }
  workbooks.sort();
  return workbooks;
}

function runWorkbook(path, { revisionOnly = false, range } = {}) {
  return new Promise((resolveResult) => {
    const command = [
      "read",
      "--format",
      "json",
      "--property",
      "metrics",
      "--max-bytes",
      String(maxBytes),
      "--max-expanded-bytes",
      String(maxExpandedBytes),
      "--max-cells",
      "1",
    ];
    if (revisionOnly) command.push("--revision-only");
    if (range !== undefined) command.push("--range", range);
    command.push(path);
    const started = performance.now();
    const child = spawn(binary, command, { stdio: ["ignore", "pipe", "pipe"] });
    const stdout = [];
    const stderr = [];
    let outputBytes = 0;
    let outputLimited = false;
    const capture = (chunks, chunk) => {
      outputBytes += chunk.length;
      if (outputBytes <= 1024 * 1024) chunks.push(chunk);
      else outputLimited = true;
    };
    child.stdout.on("data", (chunk) => capture(stdout, chunk));
    child.stderr.on("data", (chunk) => capture(stderr, chunk));
    child.on("error", (error) => {
      resolveResult({
        ok: false,
        wallMs: performance.now() - started,
        errorKind: "spawn",
        diagnostic: error.message,
      });
    });
    child.on("close", (code, signal) => {
      const wallMs = performance.now() - started;
      const stdoutText = Buffer.concat(stdout).toString("utf8");
      const stderrText = Buffer.concat(stderr).toString("utf8");
      if (code !== 0 || signal !== null || outputLimited) {
        resolveResult({
          ok: false,
          wallMs,
          errorKind: outputLimited ? "output-limit" : `exit-${code ?? signal}`,
          diagnostic: boundedDiagnostic(stderrText),
        });
        return;
      }
      try {
        const metrics = JSON.parse(stdoutText);
        if (
          metrics.extraction?.method !== "ooxml-sparse" ||
          !Number.isSafeInteger(metrics.extraction.durationMicros) ||
          metrics.extraction.durationMicros < 0 ||
          !Number.isSafeInteger(metrics.statistics?.expandedBytesRead) ||
          !Array.isArray(metrics.sheets)
        ) {
          throw new Error("invalid metrics property");
        }
        resolveResult({
          ok: true,
          wallMs,
          metrics,
        });
      } catch (error) {
        resolveResult({
          ok: false,
          wallMs,
          errorKind: "invalid-output",
          diagnostic: error.message,
        });
      }
    });
  });
}

function boundedDiagnostic(value) {
  return value
    .replaceAll(/\x1b\[[0-9;]*m/g, "")
    .replaceAll(/[\r\n]+/g, " ")
    .trim()
    .slice(0, 500);
}

function percentile(values, fraction) {
  if (values.length === 0) return null;
  const sorted = [...values].sort((left, right) => left - right);
  return sorted[Math.ceil(fraction * sorted.length) - 1];
}

const corpusRoot = resolve(args[0]);
const workbooks = await findWorkbooks(corpusRoot);
if (workbooks.length === 0) throw new Error("no XLSX workbooks found");
const sizes = await Promise.all(workbooks.map(async (path) => (await stat(path)).size));
const benchmarkStarted = performance.now();
let nextIndex = 0;
const results = new Array(workbooks.length);
async function worker() {
  while (nextIndex < workbooks.length) {
    const index = nextIndex;
    nextIndex += 1;
    const cold = await runWorkbook(workbooks[index]);
    if (!cold.ok) {
      results[index] = { ...cold, stage: "cold" };
      continue;
    }
    const sheet =
      cold.metrics.sheets.find(({ state }) => state === "visible") ??
      cold.metrics.sheets[0];
    if (sheet === undefined || typeof sheet.name !== "string") {
      results[index] = {
        ok: false,
        wallMs: cold.wallMs,
        errorKind: "no-worksheet",
        diagnostic: "workbook metrics contain no worksheet",
        stage: "plan",
      };
      continue;
    }
    const revision = await runWorkbook(workbooks[index], { revisionOnly: true });
    if (!revision.ok) {
      results[index] = { ...revision, stage: "revision" };
      continue;
    }
    const range = `${quoteSheetName(sheet.name)}!A1:P24`;
    const targeted = await runWorkbook(workbooks[index], { range });
    if (!targeted.ok) {
      results[index] = { ...targeted, stage: "targeted" };
      continue;
    }
    const coldBytes = cold.metrics.statistics.expandedBytesRead;
    const incrementalBytes =
      revision.metrics.statistics.expandedBytesRead +
      targeted.metrics.statistics.expandedBytesRead;
    const coldMicros = cold.metrics.extraction.durationMicros;
    const incrementalMicros =
      revision.metrics.extraction.durationMicros +
      targeted.metrics.extraction.durationMicros;
    const byteSaving = coldBytes === 0 ? 0 : 1 - incrementalBytes / coldBytes;
    const timeSaving = coldMicros === 0 ? 0 : 1 - incrementalMicros / coldMicros;
    results[index] = {
      ok: true,
      wallMs: cold.wallMs + revision.wallMs + targeted.wallMs,
      cold,
      revision,
      targeted,
      byteSaving,
      timeSaving,
      efficient: byteSaving >= 0.8 && timeSaving >= 0.8,
    };
  }
}

function quoteSheetName(name) {
  if (/^[A-Za-z0-9_]+$/.test(name)) return name;
  return `'${name.replaceAll("'", "''")}'`;
}
await Promise.all(Array.from({ length: Math.min(concurrency, workbooks.length) }, worker));
const wallMs = performance.now() - benchmarkStarted;
const successes = results.filter((result) => result.ok);
const failures = results
  .map((result, index) => ({ result, index }))
  .filter(({ result }) => !result.ok);
const failureKinds = {};
for (const { result } of failures) {
  failureKinds[result.errorKind] = (failureKinds[result.errorKind] ?? 0) + 1;
}
const totalBytes = sizes.reduce((sum, value) => sum + value, 0);
const successRatePercent = (successes.length / workbooks.length) * 100;
const thresholdPercent = 80;
const efficient = successes.filter((result) => result.efficient);
const efficiencyRatePercent = (efficient.length / successes.length) * 100;
const output = {
  schemaVersion: 1,
  benchmark: "opsail-xlsx-corpus",
  corpusRoot,
  binary,
  concurrency,
  acceptance: {
    compatibility: {
      metric: "successful cold, revision, and targeted reads / eligible non-temporary XLSX files",
      actualPercent: Number(successRatePercent.toFixed(3)),
    },
    highEfficiency: {
      metric: "valid workbooks with both expanded-byte and extraction-time savings >= 80%",
      thresholdPercent,
      actualPercent: Number(efficiencyRatePercent.toFixed(3)),
      passed: efficiencyRatePercent >= thresholdPercent,
    },
  },
  corpus: {
    workbooks: workbooks.length,
    successful: successes.length,
    failed: failures.length,
    compressedBytes: totalBytes,
    largestWorkbookBytes: Math.max(...sizes),
  },
  performance: {
    wallMs: Number(wallMs.toFixed(3)),
    throughputMiBPerSecond: Number(
      (totalBytes / (1024 * 1024) / (wallMs / 1000)).toFixed(3),
    ),
    processWallMs: {
      p50: Number(percentile(successes.map(({ wallMs }) => wallMs), 0.5)?.toFixed(3)),
      p95: Number(percentile(successes.map(({ wallMs }) => wallMs), 0.95)?.toFixed(3)),
      p99: Number(percentile(successes.map(({ wallMs }) => wallMs), 0.99)?.toFixed(3)),
      max: Number(Math.max(...successes.map(({ wallMs }) => wallMs)).toFixed(3)),
    },
    expandedByteSavingPercent: {
      p50: Number((percentile(successes.map(({ byteSaving }) => byteSaving), 0.5) * 100).toFixed(3)),
      p95: Number((percentile(successes.map(({ byteSaving }) => byteSaving), 0.95) * 100).toFixed(3)),
    },
    extractionTimeSavingPercent: {
      p50: Number((percentile(successes.map(({ timeSaving }) => timeSaving), 0.5) * 100).toFixed(3)),
      p95: Number((percentile(successes.map(({ timeSaving }) => timeSaving), 0.95) * 100).toFixed(3)),
    },
  },
  failures: {
    byKind: failureKinds,
    samples: failures.slice(0, 20).map(({ result, index }) => ({
      path: workbooks[index].slice(corpusRoot.length + 1),
      kind: result.errorKind,
      stage: result.stage,
      diagnostic: result.diagnostic,
    })),
  },
};
process.stdout.write(`${JSON.stringify(output, null, 2)}\n`);
