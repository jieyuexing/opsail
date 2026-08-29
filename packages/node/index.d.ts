export type ReadSource =
  | {
      kind: "url";
      url: string;
      userAgent?: string;
      acceptLanguage?: string;
    }
  | {
      kind: "html";
      html: string;
      /** URL used to resolve relative links found in the supplied HTML. */
      baseUrl?: string;
      /** Final browser navigation URL recorded as capture provenance. */
      finalUrl?: string;
    }
  | {
      kind: "file";
      path: string;
      baseUrl?: string;
    }
  | {
      kind: "chrome";
      /** HTTP(S) page to navigate to in an Opsail-owned Chrome process. */
      url: string;
      /** Explicit Chrome or Chromium executable path. */
      chromePath?: string;
      /** Browser lifecycle milestone to await after navigation. Defaults to load. */
      waitUntil?: "none" | "dom-content-loaded" | "load" | "network-idle";
      /** User-Agent applied before navigation. */
      userAgent?: string;
      /** Accept-Language value applied before navigation. */
      acceptLanguage?: string;
    }
  | {
      kind: "cdp";
      endpoint: string;
      /** HTTP(S) page to navigate to before capture. */
      url?: string;
      /** Existing Chrome page target to capture or navigate. */
      targetId?: string;
      /** Treat endpoint as a page-scoped WebSocket; incompatible with targetId. */
      directPage?: boolean;
      /** Browser lifecycle milestone to await after navigation. Defaults to load. */
      waitUntil?: "none" | "dom-content-loaded" | "load" | "network-idle";
      /** User-Agent applied before navigation. */
      userAgent?: string;
      /** Accept-Language value applied before navigation. */
      acceptLanguage?: string;
    };

export interface ReadRequest {
  source: ReadSource;
  options?: {
    /** Native acquisition deadline; extraction and bounded cleanup may run afterward. */
    timeoutMs?: number;
    maxBytes?: number;
    // Repeated Sheet!A1:D20 selectors, batched in one XLSX archive pass.
    ranges?: string[];
    // Maximum non-empty XLSX cells returned across all selectors.
    maxCells?: number;
    // Maximum cumulative uncompressed OOXML bytes read from an XLSX package.
    maxExpandedBytes?: number;
    // Include formula expressions alongside cached values. Defaults to true.
    includeFormulas?: boolean;
    // Read the XLSX revision and workbook manifest without worksheet expansion.
    revisionOnly?: boolean;
  };
}

export interface ReadResult {
  schemaVersion: 1;
  content: string;
  contentHtml: string;
  metadata: {
    title: string;
    author?: string;
    description?: string;
    site?: string;
    published?: string;
    modified?: string;
    image?: string;
    favicon?: string;
    language?: string;
    direction?: string;
    canonicalUrl?: string;
    domain?: string;
  };
  source: {
    kind: "url" | "file" | "stdin" | "html" | "chrome" | "cdp" | "memory";
    requested: string;
    resolvedUrl?: string;
    contentType?: string;
    charset: string;
    bytes: number;
  };
  extraction: {
    method: "readability" | "expanded" | "semantic";
    durationMs: number;
  };
  quality: {
    grade: "good" | "fair" | "thin";
    contentCharacters: number;
    wordCount: number;
    extractionRatio: number;
    probablyReadable: boolean;
  };
  warnings: string[];
}

export interface WorkbookReadResult {
  schemaVersion: 1;
  artifactKind: "workbook";
  content: string;
  contentHtml: string;
  metadata: { title: string };
  source: {
    kind: "file";
    requested: string;
    resolvedUrl?: string;
    contentType?: string;
    charset: "binary";
    bytes: number;
  };
  extraction: {
    method: "ooxml-sparse";
    durationMs: number;
    durationMicros: number;
  };
  revision: WorkbookRevision;
  workbook: {
    format: "xlsx";
    dateSystem: "excel1900" | "excel1904";
    sheets: WorkbookSheet[];
    definedNames: WorkbookDefinedName[];
    selections: WorkbookSelection[];
    features: WorkbookFeatureInventory;
    statistics: WorkbookStatistics;
  };
  warnings: string[];
}

export interface WorkbookSheet {
  index: number;
  name: string;
  part: string;
  revision: WorkbookPartRevision;
  state: "visible" | "hidden" | "very-hidden";
  declaredDimension?: string;
  semanticBounds?: string;
  semanticBoundsComplete: boolean;
  selected: boolean;
  mergedRanges: string[];
  /** Worksheet picture inventory. Payloads are published only on intersecting selections. */
  pictures: WorkbookPicture[];
  hiddenRows: number;
  hiddenColumns: number;
  print: WorksheetPrintEvidence;
  features: WorksheetFeatureInventory;
}

export interface WorksheetPrintEvidence {
  printArea?: string;
  printTitles?: string;
  pageSetup?: WorksheetPageSetup;
  printOptions?: WorksheetPrintOptions;
  rowBreaks: number[];
  columnBreaks: number[];
  headerFooter: boolean;
}

export interface WorksheetPageSetup {
  orientation?: string;
  paperSize?: number;
  fitToPage?: boolean;
  fitToWidth?: number;
  fitToHeight?: number;
  scale?: number;
}

export interface WorksheetPrintOptions {
  gridLines?: boolean;
  headings?: boolean;
}

export interface WorkbookRevision {
  id: string;
  compressedBytes: number;
  expandedBytes: number;
  parts: WorkbookPartRevision[];
}

export interface WorkbookPartRevision {
  name: string;
  crc32: string;
  compressedBytes: number;
  expandedBytes: number;
}

export interface WorkbookFeatureInventory {
  inventoryComplete: boolean;
  cellFormats: number;
  customNumberFormats: number;
  richStringItems: number;
  themeParts: number;
  drawingParts: number;
  chartParts: number;
  imageParts: number;
  tableParts: number;
  commentParts: number;
  controlPropertyParts: number;
  macroProjectParts: number;
}

export interface WorkbookHyperlink {
  reference: string;
  target?: string;
  location?: string;
  display?: string;
  external: boolean;
}

export interface WorksheetFeatureInventory {
  scanned: boolean;
  cellDataComplete: boolean;
  tailFeaturesComplete: boolean;
  complete: boolean;
  featureReferencesTruncated: boolean;
  formulaCells: number;
  hyperlinks: WorkbookHyperlink[];
  autoFilter?: string;
  tableParts: number;
  drawingParts: number;
  commentDrawingParts: number;
  conditionalFormatRules: number;
  conditionalFormatRanges: string[];
  dataValidationRules: number;
  dataValidationRanges: string[];
  pageSetup: boolean;
  headerFooter: boolean;
  outlinedRows: number;
  outlinedColumns: number;
  maxRowOutlineLevel: number;
  maxColumnOutlineLevel: number;
  sparklines: number;
  controls: number;
}

export interface WorkbookDefinedName {
  name: string;
  localSheetIndex?: number;
  reference: string;
  validReference: boolean;
}

export interface WorkbookSelection {
  requested: string;
  sheet: string;
  range: string;
  bounds: {
    startRow: number;
    startColumn: number;
    endRow: number;
    endColumn: number;
  };
  usedBounds?: string;
  mergedRanges: string[];
  /** Pictures whose DrawingML anchors intersect the requested rectangle. */
  images: WorkbookPicture[];
  /** Independent of cell `truncated`; true when image payload/reference caps were hit. */
  imagesTruncated: boolean;
  overflow?: WorkbookSelectionOverflow;
  cells: WorkbookCell[];
  truncated: boolean;
}

export interface WorkbookPicture {
  sheet: string;
  fromCell: string;
  toCell?: string;
  /** Zero-based OOXML drawing marker index. */
  fromRowIndex: number;
  /** Zero-based OOXML drawing marker index. */
  fromColumnIndex: number;
  toRowIndex?: number;
  toColumnIndex?: number;
  mediaPart: string;
  contentType: string;
  byteSize: number;
  sha256: string;
  /** Bounded image payload; absent from sheet inventory and when limits are hit. */
  dataUri?: string;
  payloadTruncated: boolean;
}

export interface WorkbookSelectionOverflow {
  left?: WorkbookColumnOverflow;
  right?: WorkbookColumnOverflow;
  above?: WorkbookRowOverflow;
  below?: WorkbookRowOverflow;
}

export interface WorkbookColumnOverflow {
  minColumn: number;
  maxColumn: number;
  cellCount: number;
}

export interface WorkbookRowOverflow {
  minRow: number;
  maxRow: number;
  cellCount: number;
}

export interface WorkbookCell {
  reference: string;
  row: number;
  column: number;
  valueType: "string" | "number" | "boolean" | "error" | "date" | "blank";
  value: string;
  display: string;
  formula?: string;
  formulaKind?: "normal" | "shared" | "array" | "data-table" | "other";
  formulaReference?: string;
  sharedFormulaIndex?: number;
  richText: boolean;
  fontStrike?: boolean;
  fontColor?: WorkbookFontColor;
  richTextRuns?: WorkbookRichTextRun[];
  merge?: WorkbookMergeMembership;
  styleIndex?: number;
  numberFormat?: string;
}

export interface WorkbookMergeMembership {
  range: string;
  anchor: string;
  role: "anchor" | "covered";
}

export interface WorkbookFontColor {
  rgb?: string;
  theme?: number;
  indexed?: number;
  auto?: boolean;
  tint?: number;
  resolvedRgb?: string;
}

export interface WorkbookRichTextRun {
  text: string;
  strike?: boolean;
  fontColor?: WorkbookFontColor;
}

export interface WorkbookStatistics {
  archiveEntries: number;
  expandedBytesRead: number;
  scannedSheets: number;
  cellElements: number;
  nonEmptyCells: number;
  styleOnlyCells: number;
  returnedCells: number;
}

export type ReadArtifact = ReadResult | WorkbookReadResult;
export interface CallOptions {
  signal?: AbortSignal;
}

export interface OpsailConfig {
  binaryPath?: string;
  /** Explicit whole-process deadline; otherwise includes a cleanup margin after timeoutMs. */
  hardTimeoutMs?: number;
  maxOutputBytes?: number;
}

export interface OpsailClient {
  read(request: ReadRequest, options?: CallOptions): Promise<ReadArtifact>;
}

export class OpsailError extends Error {
  readonly code: string;
  readonly stage: "input" | "acquire" | "extract" | "protocol" | "process";
  readonly retryable: boolean;
  readonly recovery?: string;
  readonly diagnostic?: string;
}

export function read(
  request: ReadRequest,
  options?: CallOptions,
): Promise<ReadArtifact>;

export function createOpsail(config?: OpsailConfig): OpsailClient;

export function opsailPath(options?: { binaryPath?: string }): string;
