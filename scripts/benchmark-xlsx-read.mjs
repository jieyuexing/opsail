#!/usr/bin/env node

import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { performance } from "node:perf_hooks";

function usage() {
  throw new Error(
    "usage: benchmark-xlsx-read.mjs [--binary PATH] [--runs N] WORKBOOK RANGE [RANGE ...]",
  );
}

const args = process.argv.slice(2);
let binary = "opsail";
let runs = 7;
for (let index = 0; index < args.length; ) {
  if (args[index] === "--binary") {
    binary = args[index + 1] ?? usage();
    args.splice(index, 2);
  } else if (args[index] === "--runs") {
    runs = Number(args[index + 1]);
    if (!Number.isSafeInteger(runs) || runs < 1 || runs > 100) usage();
    args.splice(index, 2);
  } else {
    index += 1;
  }
}
if (args.length < 2) usage();
const [workbook, ...ranges] = args;

function invoke(selectedRanges) {
  const command = [
    "read",
    workbook,
    "--format",
    "json",
    "--max-cells",
    "100000",
  ];
  for (const range of selectedRanges) command.push("--range", range);
  const started = performance.now();
  const result = spawnSync(binary, command, {
    encoding: "utf8",
    maxBuffer: 128 * 1024 * 1024,
  });
  const elapsedMs = performance.now() - started;
  if (result.status !== 0 || result.signal !== null) {
    throw new Error(
      `opsail failed with status ${result.status ?? "signal"}: ${result.stderr.trim()}`,
    );
  }
  const artifact = JSON.parse(result.stdout);
  if (artifact.artifactKind !== "workbook") {
    throw new Error("opsail did not return a workbook artifact");
  }
  return {
    elapsedMs,
    outputBytes: Buffer.byteLength(result.stdout),
    artifact,
  };
}

function selectionProjection(artifact) {
  return artifact.workbook.selections.map((selection) => ({
    requested: selection.requested,
    sheet: selection.sheet,
    range: selection.range,
    truncated: selection.truncated,
    cells: selection.cells.map((cell) => ({
      reference: cell.reference,
      valueType: cell.valueType,
      value: cell.value,
      display: cell.display,
      formula: cell.formula,
      styleIndex: cell.styleIndex,
      numberFormat: cell.numberFormat,
    })),
  }));
}

function digest(value) {
  return createHash("sha256").update(JSON.stringify(value)).digest("hex");
}

function median(values) {
  const sorted = [...values].sort((left, right) => left - right);
  const middle = Math.floor(sorted.length / 2);
  return sorted.length % 2 === 1
    ? sorted[middle]
    : (sorted[middle - 1] + sorted[middle]) / 2;
}

function summarize(samples, projection) {
  return {
    runs,
    medianMs: Number(median(samples.map((sample) => sample.elapsedMs)).toFixed(3)),
    minMs: Number(Math.min(...samples.map((sample) => sample.elapsedMs)).toFixed(3)),
    maxMs: Number(Math.max(...samples.map((sample) => sample.elapsedMs)).toFixed(3)),
    medianOutputBytes: median(samples.map((sample) => sample.outputBytes)),
    semanticDigest: digest(projection(samples[0])),
  };
}

invoke(ranges);
const batchedSamples = Array.from({ length: runs }, () => invoke(ranges));

function invokeIsolated() {
  const started = performance.now();
  const results = ranges.map((range) => invoke([range]));
  return {
    elapsedMs: performance.now() - started,
    outputBytes: results.reduce((sum, result) => sum + result.outputBytes, 0),
    artifacts: results.map((result) => result.artifact),
  };
}

invokeIsolated();
const isolatedSamples = Array.from({ length: runs }, invokeIsolated);

invoke([]);
const previewSamples = Array.from({ length: runs }, () => invoke([]));

const batched = summarize(batchedSamples, (sample) =>
  selectionProjection(sample.artifact),
);
const isolated = summarize(isolatedSamples, (sample) =>
  sample.artifacts.flatMap(selectionProjection),
);
const preview = summarize(previewSamples, (sample) =>
  selectionProjection(sample.artifact),
);

if (batched.semanticDigest !== isolated.semanticDigest) {
  throw new Error("batched and isolated range projections differ");
}

const batchedArtifact = batchedSamples[0].artifact;
process.stdout.write(
  `${JSON.stringify(
    {
      schemaVersion: 1,
      workbook,
      rangeCount: ranges.length,
      uniqueSelectedSheets:
        batchedArtifact.workbook.statistics.scannedSheets,
      returnedCells:
        batchedArtifact.workbook.statistics.returnedCells,
      expandedBytesRead:
        batchedArtifact.workbook.statistics.expandedBytesRead,
      styleOnlyCells:
        batchedArtifact.workbook.statistics.styleOnlyCells,
      batched,
      isolated,
      defaultPreview: preview,
      speedupVsIsolated: Number(
        (isolated.medianMs / batched.medianMs).toFixed(2),
      ),
      semanticMatch: true,
    },
    null,
    2,
  )}\n`,
);
