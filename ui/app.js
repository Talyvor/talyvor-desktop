// The mini-chat. Everything goes through the app's own commands (src-tauri/src/lib.rs); this page holds no token.
const { invoke } = window.__TAURI__.core
const { listen } = window.__TAURI__.event
const $ = (id) => document.getElementById(id)
const thread = []

const mac = navigator.userAgent.includes('Mac')
$('shortcut').textContent = mac ? '⇧⌘Space' : 'Ctrl+Shift+Space'

function money(minor, currency) {
  const n = (Math.abs(minor) / 100).toLocaleString('en-GB', { minimumFractionDigits: 2, maximumFractionDigits: 2 })
  const sign = minor < 0 ? '-' : ''
  const symbol = { GBP: '£', EUR: '€', USD: '$' }[currency]
  return symbol ? `${sign}${symbol}${n}` : `${sign}${n} ${currency}`
}

async function refresh() {
  const signedIn = await invoke('status')
  $('signed-out').hidden = signedIn
  $('signed-in').hidden = !signedIn
  if (signedIn) {
    $('box').focus()
    invoke('approvals').then(showApprovals, () => {})
  } else {
    $('sign-in').focus()
  }
}

function openButton(url, words) {
  const b = document.createElement('button')
  b.type = 'button'
  b.className = 'secondary'
  b.textContent = words
  b.onclick = () => invoke('open_in_talyvor', { url })
  return b
}

function showApprovals(list) {
  $('approvals').hidden = list.length === 0
  $('approvals-title').textContent = list.length === 1 ? '1 approval waiting' : `${list.length} approvals waiting`
  $('approvals-list').replaceChildren(...list.map((a) => {
    const li = document.createElement('li')
    const what = document.createElement('span')
    what.className = 'what'
    what.textContent = a.title
    const amount = document.createElement('span')
    amount.className = 'amount'
    amount.textContent = money(a.amount_minor, a.currency)
    li.append(what, amount, openButton(a.approval_url, 'Open'))
    return li
  }))
}

function say(kind, text) {
  const li = document.createElement('li')
  li.className = kind
  li.textContent = text
  $('messages').querySelector('.hint')?.remove()
  $('messages').append(li)
  li.scrollIntoView({ block: 'end' })
  return li
}

$('sign-in').onclick = async () => {
  $('sign-in').disabled = true
  $('sign-in-note').textContent = 'Finish signing in in your browser.'
  try {
    await invoke('sign_in')
    $('sign-in-note').textContent = ''
    await refresh()
  } catch (e) {
    $('sign-in-note').textContent = String(e)
  } finally {
    $('sign-in').disabled = false
  }
}

$('composer').onsubmit = async (ev) => {
  ev.preventDefault()
  const message = $('box').value.trim()
  if (!message) return
  $('box').value = ''
  say('you', message)
  $('send').disabled = true
  try {
    const r = await invoke('ask', { message, thread: thread.slice(-20) })
    thread.push({ role: 'user', content: message }, { role: 'assistant', content: r.reply })
    const li = say('taly', r.reply)
    if (r.approval_url) {
      // Taly only prepares: the person approves it on the phone or the web, never here.
      const waiting = document.createElement('div')
      waiting.className = 'waiting'
      const words = document.createElement('span')
      words.textContent = 'Waiting for your approval'
      waiting.append(words, openButton(r.approval_url, 'Open approval'))
      li.append(waiting)
    }
  } catch (e) {
    say('problem', String(e))
    if (String(e).startsWith('Sign in')) refresh()
  } finally {
    $('send').disabled = false
    $('box').focus()
  }
}

$('box').onkeydown = (ev) => {
  if (ev.key === 'Enter' && !ev.shiftKey) {
    ev.preventDefault()
    $('composer').requestSubmit()
  }
}

// Push-to-talk: recording only while the button is held; what was said lands in the box, unsent.
let recorder = null
async function startTalking() {
  if (recorder) return
  try {
    const stream = await navigator.mediaDevices.getUserMedia({ audio: true })
    const chunks = []
    recorder = new MediaRecorder(stream)
    recorder.ondataavailable = (e) => chunks.push(e.data)
    recorder.onstop = async () => {
      stream.getTracks().forEach((t) => t.stop())
      const blob = new Blob(chunks, { type: recorder.mimeType })
      recorder = null
      $('mic').setAttribute('aria-pressed', 'false')
      $('mic-note').textContent = 'Writing it down…'
      try {
        const bytes = new Uint8Array(await blob.arrayBuffer())
        let bin = ''
        for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000))
        const text = await invoke('transcribe', { audioBase64: btoa(bin), mime: blob.type })
        $('box').value = ($('box').value + ' ' + text).trim()
        $('mic-note').textContent = ''
        $('box').focus()
      } catch (e) {
        $('mic-note').textContent = String(e)
      }
    }
    recorder.start()
    $('mic').setAttribute('aria-pressed', 'true')
    $('mic-note').textContent = 'Listening…'
  } catch {
    $('mic-note').textContent = 'Talyvor may not use the microphone. Allow it in System Settings.'
  }
}
function stopTalking() {
  if (recorder?.state === 'recording') recorder.stop()
}
$('mic').onpointerdown = startTalking
$('mic').onpointerup = stopTalking
$('mic').onpointerleave = stopTalking
$('mic').onkeydown = (e) => { if ((e.key === ' ' || e.key === 'Enter') && !e.repeat) { e.preventDefault(); startTalking() } }
$('mic').onkeyup = (e) => { if (e.key === ' ' || e.key === 'Enter') stopTalking() }

$('close').onclick = () => invoke('hide')
document.addEventListener('keydown', (e) => { if (e.key === 'Escape') invoke('hide') })

listen('taly-shown', refresh)
listen('approvals', (e) => showApprovals(e.payload))
listen('signed-out', refresh)
refresh()
