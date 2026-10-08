/** Standalone package paths. Embedders may supply explicit process-local defaults. */
import { homedir } from 'node:os'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const packageDir = dirname(dirname(fileURLToPath(import.meta.url)))
export const OPSAIL_PACKAGE_DIR = resolve(process.env.OPSAIL_RUNTIME_PACKAGE_DIR || packageDir)

export function resolveOpsailLayout(options = {}) {
  const root = resolve(options.root || process.env.OPSAIL_RUNTIME_PACKAGE_DIR || packageDir)
  const sourceDir = resolve(options.sourceDir || process.env.OPSAIL_SOURCE_DIR || join(packageDir, '../..'))
  const pinPath = resolve(options.pinPath || process.env.OPSAIL_PIN_PATH || join(root, 'pin.json'))
  const dataHome = process.env.XDG_DATA_HOME || join(homedir(), '.local/share')
  const retainedChatDataDir = resolve(options.dataRoot || process.env.OPSAIL_CHAT_DATA_ROOT || join(dataHome, 'opsail-host/retained-chat'))
  const chatBindingFile = resolve(options.bindingFile || process.env.OPSAIL_CHAT_BINDING_FILE || join(retainedChatDataDir, 'bindings.json'))
  return Object.freeze({ root, sourceDir, pinPath, retainedChatDataDir, chatBindingFile })
}
