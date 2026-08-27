import assert from "node:assert/strict";
import test from "node:test";

import { parseMachineResponse } from "../src/client.js";
import { OpsailError } from "../src/errors.js";

function encode(value) {
  return Buffer.from(JSON.stringify(value));
}

function machineEnvelope(value) {
  return {
    protocolVersion: 1,
    engine: { name: "opsail", version: "0.1.0" },
    ...value,
  };
}

function validResult() {
  return {
    schemaVersion: 1,
    content: "Readable text.",
    contentHtml: "<p>Readable text.</p>",
    metadata: { title: "Example" },
    source: {
      kind: "memory",
      requested: "<memory>",
      charset: "utf-8",
      bytes: 21,
    },
    extraction: { method: "semantic", durationMs: 1 },
    quality: {
      grade: "thin",
      contentCharacters: 14,
      wordCount: 2,
      extractionRatio: 0.5,
      probablyReadable: true,
    },
    warnings: [],
  };
}

function validWorkbookResult() {
  return {
    schemaVersion: 1,
    artifactKind: "workbook",
    content: "# Workbook: example.xlsx",
    contentHtml: "<article></article>",
    metadata: { title: "example.xlsx" },
    source: {
      kind: "file",
      requested: "example.xlsx",
      contentType:
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
      charset: "binary",
      bytes: 1024,
    },
    extraction: { method: "ooxml-sparse", durationMs: 2, durationMicros: 2000 },
    revision: {
      id: "fnv1a64-0123456789abcdef",
      compressedBytes: 1024,
      expandedBytes: 2048,
      parts: [
        {
          name: "xl/worksheets/sheet1.xml",
          crc32: "0123abcd",
          compressedBytes: 512,
          expandedBytes: 1024,
        },
      ],
    },
    workbook: {
      format: "xlsx",
      dateSystem: "excel1900",
      sheets: [
        {
          index: 0,
          name: "Data",
          part: "xl/worksheets/sheet1.xml",
          revision: {
            name: "xl/worksheets/sheet1.xml",
            crc32: "0123abcd",
            compressedBytes: 512,
            expandedBytes: 1024,
          },
          state: "visible",
          declaredDimension: "A1:XFD99",
          semanticBounds: "A1:A1",
          semanticBoundsComplete: false,
          selected: true,
          mergedRanges: [],
          hiddenRows: 0,
          hiddenColumns: 0,
          features: {
            scanned: true,
            complete: false,
            featureReferencesTruncated: false,
            formulaCells: 0,
            hyperlinks: [],
            tableParts: 0,
            drawingParts: 0,
            commentDrawingParts: 0,
            conditionalFormatRules: 0,
            conditionalFormatRanges: [],
            dataValidationRules: 0,
            dataValidationRanges: [],
            pageSetup: false,
            headerFooter: false,
            outlinedRows: 0,
            outlinedColumns: 0,
            maxRowOutlineLevel: 0,
            maxColumnOutlineLevel: 0,
            sparklines: 0,
            controls: 0,
          },
        },
      ],
      definedNames: [],
      selections: [
        {
          requested: "Data!A1:B2",
          sheet: "Data",
          range: "A1:B2",
          bounds: {
            startRow: 1,
            startColumn: 1,
            endRow: 2,
            endColumn: 2,
          },
          cells: [
            {
              reference: "A1",
              row: 1,
              column: 1,
              valueType: "string",
              value: "value",
              display: "value",
              richText: false,
            },
          ],
          truncated: false,
        },
      ],
      features: {
        inventoryComplete: false,
        cellFormats: 1,
        customNumberFormats: 0,
        richStringItems: 0,
        themeParts: 1,
        drawingParts: 0,
        chartParts: 0,
        imageParts: 0,
        tableParts: 0,
        commentParts: 0,
        controlPropertyParts: 0,
        macroProjectParts: 0,
      },
      statistics: {
        archiveEntries: 4,
        expandedBytesRead: 2048,
        scannedSheets: 1,
        cellElements: 2,
        nonEmptyCells: 1,
        styleOnlyCells: 1,
        returnedCells: 1,
      },
    },
    warnings: [],
  };
}

function successResponse(result = validResult()) {
  return encode(machineEnvelope({ ok: true, result }));
}

function assertInvalidResult(mutate) {
  const result = validResult();
  mutate(result);

  assert.throws(
    () => parseMachineResponse(successResponse(result), 0, null),
    (error) =>
      error instanceof OpsailError && error.code === "invalid-response",
  );
}

test("machine responses require a versioned ReadResult", () => {
  assert.throws(
    () =>
      parseMachineResponse(
        encode(machineEnvelope({ ok: true, result: {} })),
        0,
        null,
      ),
    (error) =>
      error instanceof OpsailError && error.code === "invalid-response",
  );
});

test("machine responses accept a validated sparse workbook artifact", () => {
  const result = validWorkbookResult();
  assert.deepEqual(parseMachineResponse(successResponse(result), 0, null), result);
});

test("machine workbook validation rejects inconsistent structured fields", () => {
  for (const mutate of [
    (result) => {
      result.artifactKind = "document";
    },
    (result) => {
      result.extraction.method = "semantic";
    },
    (result) => {
      result.workbook.sheets[0].state = "secret";
    },
    (result) => {
      result.workbook.selections[0].bounds.endRow = 0;
    },
    (result) => {
      result.workbook.selections[0].cells[0].valueType = "formula";
    },
    (result) => {
      result.workbook.statistics.returnedCells = -1;
    },
    (result) => {
      result.revision.id = "unstable";
    },
    (result) => {
      result.workbook.sheets[0].features.complete = "yes";
    },
    (result) => {
      result.workbook.selections[0].cells[0].richText = 1;
    },
  ]) {
    const result = validWorkbookResult();
    mutate(result);
    assert.throws(
      () => parseMachineResponse(successResponse(result), 0, null),
      (error) =>
        error instanceof OpsailError && error.code === "invalid-response",
    );
  }
});

test("machine responses require the Opsail engine identity", () => {
  for (const engine of [
    undefined,
    null,
    { name: "other", version: "0.1.0" },
    { name: "opsail", version: "" },
    { name: "opsail", version: 1 },
  ]) {
    const response = machineEnvelope({ ok: true, result: validResult() });
    response.engine = engine;
    assert.throws(
      () => parseMachineResponse(encode(response), 0, null),
      (error) =>
        error instanceof OpsailError && error.code === "invalid-response",
    );
  }
});

test("machine ReadResult validation covers required strings and objects", () => {
  for (const mutate of [
    (result) => {
      result.content = null;
    },
    (result) => {
      result.contentHtml = 1;
    },
    (result) => {
      result.metadata = [];
    },
    (result) => {
      result.metadata.title = false;
    },
    (result) => {
      result.source = null;
    },
    (result) => {
      result.source.requested = 1;
    },
    (result) => {
      result.source.charset = undefined;
    },
    (result) => {
      result.extraction = [];
    },
    (result) => {
      result.quality = null;
    },
    (result) => {
      result.warnings = ["useful", 1];
    },
  ]) {
    assertInvalidResult(mutate);
  }
});

test("machine ReadResult validation enforces documented enums", () => {
  for (const mutate of [
    (result) => {
      result.source.kind = "browser";
    },
    (result) => {
      result.extraction.method = "custom";
    },
    (result) => {
      result.quality.grade = "excellent";
    },
  ]) {
    assertInvalidResult(mutate);
  }
});

test("machine ReadResult validation accepts captured browser provenance", () => {
  for (const kind of ["html", "chrome", "cdp"]) {
    const result = validResult();
    result.source.kind = kind;
    assert.deepEqual(parseMachineResponse(successResponse(result), 0, null), result);
  }
});

test("machine ReadResult validation enforces non-negative safe counts", () => {
  const setters = [
    (result, value) => {
      result.source.bytes = value;
    },
    (result, value) => {
      result.extraction.durationMs = value;
    },
    (result, value) => {
      result.quality.contentCharacters = value;
    },
    (result, value) => {
      result.quality.wordCount = value;
    },
  ];

  for (const set of setters) {
    for (const value of [-1, 1.5, Number.MAX_SAFE_INTEGER + 1]) {
      assertInvalidResult((result) => set(result, value));
    }
  }
});

test("machine ReadResult validation requires a bounded extraction ratio and boolean readability", () => {
  for (const value of [null, "0.5", false, -0.01, 1.01]) {
    assertInvalidResult((result) => {
      result.quality.extractionRatio = value;
    });
  }
  assertInvalidResult((result) => {
    result.quality.probablyReadable = 1;
  });

  const envelope = JSON.stringify(
    machineEnvelope({
      ok: true,
      result: validResult(),
    }),
  ).replace('"extractionRatio":0.5', '"extractionRatio":1e400');
  assert.throws(
    () => parseMachineResponse(Buffer.from(envelope), 0, null),
    (error) =>
      error instanceof OpsailError && error.code === "invalid-response",
  );
});

test("machine ReadResult validation checks optional metadata and source strings", () => {
  const metadataFields = [
    "author",
    "description",
    "site",
    "published",
    "modified",
    "image",
    "favicon",
    "language",
    "direction",
    "canonicalUrl",
    "domain",
  ];
  for (const field of metadataFields) {
    assertInvalidResult((result) => {
      result.metadata[field] = 1;
    });
  }
  for (const field of ["resolvedUrl", "contentType"]) {
    assertInvalidResult((result) => {
      result.source[field] = false;
    });
  }

  const result = validResult();
  for (const field of metadataFields) result.metadata[field] = field;
  result.source.resolvedUrl = "https://example.test/final";
  result.source.contentType = "text/html";
  assert.deepEqual(parseMachineResponse(successResponse(result), 0, null), result);
});

test("machine success responses require exit code zero without a signal", () => {
  const result = validResult();
  assert.deepEqual(
    parseMachineResponse(
      encode(machineEnvelope({ ok: true, result })),
      0,
      null,
    ),
    result,
  );

  assert.throws(
    () =>
      parseMachineResponse(
        encode(machineEnvelope({ ok: true, result })),
        null,
        "SIGTERM",
      ),
    (error) =>
      error instanceof OpsailError && error.code === "protocol-mismatch",
  );
});

test("machine failure responses require the reserved exit code", () => {
  const response = encode(machineEnvelope({
    ok: false,
    error: {
      code: "not-html",
      stage: "input",
      message: "source does not appear to be HTML",
      retryable: false,
    },
  }));

  assert.throws(
    () => parseMachineResponse(response, 1, null),
    (error) => error instanceof OpsailError && error.code === "not-html",
  );
  for (const [exitCode, signalCode] of [
    [2, null],
    [null, "SIGTERM"],
  ]) {
    assert.throws(
      () => parseMachineResponse(response, exitCode, signalCode),
      (error) =>
        error instanceof OpsailError && error.code === "protocol-mismatch",
    );
  }
});

test("machine failures require a native error stage", () => {
  for (const stage of ["unknown-stage", "protocol", "process"]) {
    const response = encode(
      machineEnvelope({
        ok: false,
        error: {
          code: "not-html",
          stage,
          message: "source does not appear to be HTML",
          retryable: false,
        },
      }),
    );

    assert.throws(
      () => parseMachineResponse(response, 1, null),
      (error) =>
        error instanceof OpsailError && error.code === "invalid-response",
    );
  }
});

test("machine failures only accept the native recovery value", () => {
  const failure = {
    code: "verification-required",
    stage: "acquire",
    message: "source requires browser verification",
    retryable: false,
    recovery: "rendered-html",
  };
  assert.throws(
    () =>
      parseMachineResponse(
        encode(machineEnvelope({ ok: false, error: failure })),
        1,
        null,
      ),
    (error) =>
      error instanceof OpsailError && error.recovery === "rendered-html",
  );

  assert.throws(
    () =>
      parseMachineResponse(
        encode(
          machineEnvelope({
            ok: false,
            error: { ...failure, recovery: "open-browser" },
          }),
        ),
        1,
        null,
      ),
    (error) =>
      error instanceof OpsailError && error.code === "invalid-response",
  );
});

test("process and protocol errors expose sanitized bounded native diagnostics", () => {
  assert.throws(
    () =>
      parseMachineResponse(
        Buffer.alloc(0),
        2,
        null,
        Buffer.from(
          "\u001b[31mfirst\u001b[0m\u009b32m line\u009b0m\u202e\0\r\nsecond line\u2067",
        ),
      ),
    (error) => {
      assert(error instanceof OpsailError);
      assert.equal(error.code, "process-failed");
      assert.equal(error.diagnostic, "first line\nsecond line");
      return true;
    },
  );

  assert.throws(
    () =>
      parseMachineResponse(
        Buffer.from("not-json"),
        2,
        null,
        Buffer.from("x".repeat(10_000)),
      ),
    (error) => {
      assert(error instanceof OpsailError);
      assert.equal(error.code, "invalid-response");
      assert(Buffer.byteLength(error.diagnostic, "utf8") <= 4_096);
      assert.match(error.diagnostic, /…$/u);
      return true;
    },
  );
});

test("valid structured failures only expose whitelisted fields", () => {
  const response = encode(machineEnvelope({
    ok: false,
    error: {
      code: "not-html",
      stage: "input",
      message: "source does not appear to be HTML",
      retryable: false,
      diagnostic: "\u001b[31minjected diagnostic",
      cause: { secret: "injected cause" },
    },
  }));

  assert.throws(
    () =>
      parseMachineResponse(
        response,
        1,
        null,
        Buffer.from("possibly sensitive native detail"),
      ),
    (error) => {
      assert(error instanceof OpsailError);
      assert.equal(error.message, "source does not appear to be HTML");
      assert.equal(error.diagnostic, undefined);
      assert.equal(error.cause, undefined);
      return true;
    },
  );
});
