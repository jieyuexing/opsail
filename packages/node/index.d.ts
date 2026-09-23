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
  gateway(request: GatewayRequest, options: GatewayCallOptions): Promise<GatewayResult>;
  read(request: ReadRequest, options?: CallOptions): Promise<ReadResult>;
}

export class OpsailError extends Error {
  readonly code: string;
  readonly stage: "input" | "vault" | "acquire" | "extract" | "protocol" | "process";
  readonly retryable: boolean;
  readonly recovery?: string;
  readonly diagnostic?: string;
  readonly httpStatus?: number;
  readonly providerCode?: string;
  readonly elapsedMs?: number;
}

export type GatewayAuth = { type: "none" } | { type: "bearer"; key: string }
  | { type: "header"; name: string; key: string };
export interface GatewayConnection {
  name: string;
  adapter: "http" | "openai-compatible" | "vercel-ai-gateway";
  baseUrl: string;
  auth: GatewayAuth;
  defaultModel?: string;
  allowHttp?: boolean;
}
export interface GatewayConnectionSummary extends Omit<GatewayConnection, "auth"> {
  authType: "none" | "bearer" | "header";
  authHeader?: string;
  hasKey: boolean;
}
export type EvaluationInput = string | Record<string, unknown> | unknown[];
export type EvaluationQuestion =
  | { type: "boolean"; instructions: EvaluationInput; criteria?: { true?: EvaluationInput | null; false?: EvaluationInput | null } }
  | { type: "choice"; instructions: EvaluationInput; criteria: Record<string, EvaluationInput | null> }
  | { type: "score"; instructions: EvaluationInput; criteria: (EvaluationInput | null)[] };
export type GatewayRequest =
  | { operation: "init" | "list" | "rekey" }
  | { operation: "set"; connection: GatewayConnection }
  | { operation: "remove"; name: string }
  | { operation: "set-default-model"; name: string; model?: string | null }
  | { operation: "request"; connection: string; method: string; path: string; query?: Record<string, string>;
      headers?: Record<string, string>; body?: { type: "json"; value: unknown } | { type: "text"; value: string }; timeoutMs?: number }
  | { operation: "models"; connection: string; timeoutMs?: number }
  | { operation: "chat"; connection: string; model?: string; messages: Record<string, unknown>[];
      parameters?: Record<string, unknown>; timeoutMs?: number }
  | { operation: "evaluate"; connection: string; model?: string; state: EvaluationInput;
      questions: Record<string, EvaluationQuestion>; providerOptions?: unknown; timeoutMs?: number };
export interface GatewayCallOptions extends CallOptions {
  /** Passed only through private child stdin, never argv or a persisted configuration. */
  passphrase: string;
  newPassphrase?: string;
  dataDir?: string;
}
export interface GatewayResult {
  schemaVersion: 1;
  operation: GatewayRequest["operation"];
  connection?: string;
  httpStatus?: number;
  elapsedMs: number;
  /** Full provider JSON/text or a credential-free management result. */
  data: unknown;
}
export function gateway(request: GatewayRequest, options: GatewayCallOptions): Promise<GatewayResult>;

export function read(
  request: ReadRequest,
  options?: CallOptions,
): Promise<ReadResult>;

export function createOpsail(config?: OpsailConfig): OpsailClient;

export function opsailPath(options?: { binaryPath?: string }): string;
