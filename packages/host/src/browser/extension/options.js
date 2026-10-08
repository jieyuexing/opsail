const $ = id => document.getElementById(id)
let state
async function refresh() {
  state = await chrome.runtime.sendMessage({action: 'status'})
  $('connection').textContent = state.connectionState || '未连接'
  $('profile').textContent = state.profileId || '未初始化'
  $('state').textContent = state.paused ? '已暂停' : '等待 CLI 请求'
  $('pause').textContent = state.paused ? '恢复读取' : '暂停读取'
  $('sources').replaceChildren()
  for (const [provider, binding] of Object.entries(state.bindings || {})) {
    const row = document.createElement('div'); row.className = 'source'
    const title = document.createElement('span'); title.textContent = provider === 'feishu' ? '飞书' : 'Teams'
    const button = document.createElement('button'); button.textContent = binding ? '授予站点权限' : '等待 CLI 配置'; button.disabled = !binding
    button.addEventListener('click', async () => {
      const granted = await chrome.permissions.request({origins: binding.allowedOrigins.map(origin => origin + '/*')})
      $('notice').textContent = granted ? '权限已授予，请重试原 CLI 命令。' : '未授予权限。'
    })
    row.append(title, button); $('sources').append(row)
  }
}
$('pause').addEventListener('click', async () => { await chrome.runtime.sendMessage({action: 'pause', paused: !state.paused}); refresh() })
$('reconnect').addEventListener('click', async () => { await chrome.runtime.sendMessage({action: 'reconnect'}); refresh() })
refresh().catch(() => { $('notice').textContent = '无法读取扩展状态，请重新加载扩展。' })
