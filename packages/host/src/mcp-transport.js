/** Shared MCP transport. Capability selection belongs to each server entry. */
import readline from 'node:readline'

const MAX_REQUEST_BYTES = 1024 * 1024
const JSON_RPC_ERROR = {
  parseError: -32700,
  invalidRequest: -32600,
  methodNotFound: -32601,
  invalidParams: -32602,
}

export function projectOpsailTool(definition) {
  return {
    name: definition.name,
    ...(definition._meta ? { _meta: definition._meta } : {}),
    title: definition.title ?? definition.name,
    description: definition.description,
    inputSchema: definition.parameters,
    outputSchema: definition.output.schema,
    annotations: definition.annotations ?? { readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: true },
  }
}

function errorText(error) { return error instanceof Error ? error.message : String(error) }

export function startToolMcpServer({
  input = process.stdin,
  output = process.stdout,
  definitions = [],
  serverName = 'opsail-host',
  serverVersion = '0.1.0',
  instructions = '',
  renderFailure = () => null,
} = {}) {
  const toolsByName = new Map(definitions.map((item) => [item.name, item]))
  const pendingCalls = new Map()

  function send(message) {
    output.write(`${JSON.stringify(message)}\n`)
  }

  function sendResult(id, result) {
    send({ jsonrpc: '2.0', id, result })
  }

  function sendError(id, code, message) {
    send({ jsonrpc: '2.0', id, error: { code, message } })
  }

  async function callTool(id, params) {
    const definition = toolsByName.get(params?.name)
    if (!definition) {
      sendError(
        id,
        JSON_RPC_ERROR.invalidParams,
        `Unknown tool: ${String(params?.name ?? '')}`,
      )
      return
    }
    if (
      params.arguments != null
      && (typeof params.arguments !== 'object' || Array.isArray(params.arguments))
    ) {
      sendError(id, JSON_RPC_ERROR.invalidParams, 'Tool arguments must be an object.')
      return
    }

    const controller = new AbortController()
    pendingCalls.set(id, controller)
    const deadlineMs = Number.isInteger(definition.timeoutMs) ? definition.timeoutMs : null
    const deadline = deadlineMs == null ? null : setTimeout(
      () => controller.abort(new Error(`${definition.name} exceeded ${deadlineMs}ms deadline`)),
      deadlineMs,
    )
    try {
      const args = params.arguments ?? {}
      const result = await definition.execute(args, { signal: controller.signal })
      sendResult(id, {
        content: definition.output.render(args, result),
        structuredContent: result,
        isError: result.exitCode != null && result.exitCode !== 0,
      })
    } catch (error) {
      const failure = renderFailure(definition, params.arguments ?? {}, error)
      sendResult(id, {
        content: [{ type: 'text', text: failure ? JSON.stringify(failure, null, 2) : errorText(error) }],
        ...(failure ? { structuredContent: failure } : {}), isError: true,
      })
    } finally {
      if (deadline) clearTimeout(deadline)
      pendingCalls.delete(id)
    }
  }

  async function handleMessage(message) {
    if (message == null || typeof message !== 'object' || Array.isArray(message)) {
      sendError(null, JSON_RPC_ERROR.invalidRequest, 'Invalid JSON-RPC request.')
      return
    }

    const { id, method, params } = message
    if (method === 'initialize') {
      sendResult(id, {
        protocolVersion: params?.protocolVersion ?? '2025-11-25',
        capabilities: { tools: {} },
        serverInfo: { name: serverName, version: serverVersion },
        instructions,
      })
      return
    }
    if (method === 'notifications/initialized') return
    if (method === 'notifications/cancelled') {
      pendingCalls.get(params?.requestId)?.abort(params?.reason)
      return
    }
    if (method === 'ping') {
      sendResult(id, {})
      return
    }
    if (method === 'tools/list') {
      sendResult(id, { tools: definitions.map(projectOpsailTool) })
      return
    }
    if (method === 'tools/call') {
      await callTool(id, params)
      return
    }
    if (id !== undefined) {
      sendError(id, JSON_RPC_ERROR.methodNotFound, `Method not found: ${String(method)}`)
    }
  }

  const lines = readline.createInterface({ input, crlfDelay: Infinity })
  lines.once('close', () => {
    for (const controller of pendingCalls.values()) controller.abort(new Error('MCP client disconnected'))
  })
  lines.on('line', (line) => {
    if (line.trim().length === 0) return
    if (Buffer.byteLength(line, 'utf8') > MAX_REQUEST_BYTES) {
      sendError(null, JSON_RPC_ERROR.invalidRequest, 'JSON-RPC request is too large.')
      return
    }
    let message
    try {
      message = JSON.parse(line)
    } catch {
      sendError(null, JSON_RPC_ERROR.parseError, 'Invalid JSON.')
      return
    }
    void handleMessage(message)
  })

  return {
    close() {
      for (const controller of pendingCalls.values()) controller.abort('MCP server closed')
      lines.close()
    },
  }
}
