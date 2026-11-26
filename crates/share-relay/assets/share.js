const PALETTE_KEYS = [
  "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
  "brightBlack", "brightRed", "brightGreen", "brightYellow", "brightBlue", "brightMagenta", "brightCyan", "brightWhite",
];
const NOTHING_SHARED = "Nothing is shared with you right now. Tabs appear here as soon as the host shares them.";
const NOT_AVAILABLE = "That tab is not shared with you, or its share ended. Pick a tab on the left.";

const element = (id) => document.getElementById(id);
const terminal = new Terminal({ cols: 80, rows: 24, disableStdin: true, scrollback: 5000 });
terminal.open(element("terminal"));

const tabs = new Map();
let wanted = (location.pathname.match(/^\/s\/([^/]+)/) || [])[1] || null;
let current = null;
let socket = null;
let expiresAt = 0;
let tabsLoaded = false;

function scheme() {
  return location.protocol === "https:" ? "wss:" : "ws:";
}

function setNotice(text) {
  const notice = element("notice");
  notice.textContent = text;
  notice.hidden = !text;
  element("bar").hidden = Boolean(text);
  element("screen").hidden = Boolean(text);
  if (text) element("ended").hidden = true;
}

function modeText(mode) {
  return mode === "input" ? "you can type" : "view only";
}

function renderTabs() {
  const nav = element("tabs");
  nav.replaceChildren();
  let group = null;
  const sorted = [...tabs].sort(([, a], [, b]) => sessionKey(a.tab).localeCompare(sessionKey(b.tab)));
  for (const [id, entry] of sorted) {
    if (sessionKey(entry.tab) !== group) {
      group = sessionKey(entry.tab);
      const heading = document.createElement("div");
      heading.className = "session";
      heading.textContent = `${entry.tab.session_label} on ${entry.tab.host}`;
      nav.append(heading);
    }
    nav.append(tabButton(id, entry));
  }
}

function sessionKey(tab) {
  return `${tab.host}\u0000${tab.session_label}`;
}

function tabButton(id, entry) {
  const button = document.createElement("button");
  button.className = `tab${id === current ? " active" : ""}${entry.ended ? " ended" : ""}`;
  button.title = `${entry.tab.session_label} / ${entry.tab.label} on ${entry.tab.host}`;
  const dot = document.createElement("span");
  dot.className = `dot ${entry.tab.status?.state || ""}`;
  const name = document.createElement("span");
  name.className = "name";
  name.textContent = entry.tab.label || entry.tab.session_label;
  const close = document.createElement("span");
  if (entry.ended) {
    close.className = "close";
    close.textContent = "×";
    close.title = "Remove from the list";
    close.onclick = (event) => { event.stopPropagation(); forget(id); };
  }
  const meta = document.createElement("span");
  meta.className = "meta";
  meta.textContent = entry.ended ? `ended: ${entry.ended}` : statusText(entry.tab);
  button.append(dot, name, close, meta);
  button.onclick = () => select(id);
  return button;
}

function statusText(tab) {
  const state = tab.status?.state;
  const mode = modeText(tab.mode);
  return state && state !== "idle" ? `${mode} · ${state}` : mode;
}

function forget(id) {
  tabs.delete(id);
  if (current === id) {
    closeSocket();
    current = null;
    chooseTab();
  }
  renderTabs();
}

function markEnded(id, reason) {
  const entry = tabs.get(id);
  if (!entry || entry.ended) return;
  entry.ended = reason;
  if (id === current) showEnded(reason);
  renderTabs();
}

function onTabs(list) {
  const live = new Set(list.map((tab) => tab.id));
  for (const [id, entry] of tabs) {
    if (!live.has(id) && !entry.ended) markEnded(id, "no longer shared with you");
  }
  for (const tab of list) {
    const entry = tabs.get(tab.id);
    if (entry && !entry.ended) entry.tab = tab;
    else tabs.set(tab.id, { tab, ended: null });
  }
  tabsLoaded = true;
  if (current && tabs.get(current) && !tabs.get(current).ended) showStatus(tabs.get(current).tab.status);
  if (!current) chooseTab();
  renderTabs();
}

function chooseTab() {
  if (!tabsLoaded) return;
  if (wanted && tabs.has(wanted) && !tabs.get(wanted).ended) return select(wanted);
  const first = [...tabs].find(([, entry]) => !entry.ended);
  if (wanted) {
    wanted = null;
    setNotice(NOT_AVAILABLE);
    return;
  }
  if (first) return select(first[0]);
  setNotice(NOTHING_SHARED);
}

function select(id) {
  wanted = null;
  if (current === id) return;
  current = id;
  history.replaceState(null, "", `/s/${id}`);
  renderTabs();
  const entry = tabs.get(id);
  closeSocket();
  terminal.reset();
  setNotice("");
  showHeader(entry.tab);
  showStatus(entry.tab.status);
  element("viewers").textContent = "";
  if (entry.ended) {
    showEnded(entry.ended);
    return;
  }
  openSocket(id, 1000);
}

function closeSocket() {
  if (!socket) return;
  socket.onclose = null;
  socket.close();
  socket = null;
}

function openSocket(id, retryDelay) {
  const opened = new WebSocket(`${scheme()}//${location.host}/s/${id}/ws`);
  opened.binaryType = "arraybuffer";
  opened.onopen = () => { retryDelay = 1000; };
  opened.onmessage = (event) => {
    if (typeof event.data === "string") handle(id, JSON.parse(event.data));
    else terminal.write(new Uint8Array(event.data));
  };
  opened.onclose = () => {
    if (socket !== opened) return;
    socket = null;
    const entry = tabs.get(id);
    if (current !== id || !entry || entry.ended) return;
    showReconnecting(true);
    setTimeout(() => {
      const latest = tabs.get(id);
      if (current === id && latest && !latest.ended && !socket) openSocket(id, Math.min(retryDelay * 2, 15000));
    }, retryDelay);
  };
  socket = opened;
}

function showHeader(tab) {
  element("label").textContent = `${tab.session_label} / ${tab.label} on ${tab.host}`;
  const mode = element("mode");
  mode.textContent = modeText(tab.mode);
  mode.className = `pill ${tab.mode}`;
  terminal.options.disableStdin = tab.mode !== "input";
  expiresAt = tab.expires_at_unix;
  document.title = `${tab.label} (shared)`;
  tickExpiry();
}

function applyTheme(theme) {
  const next = {};
  if (theme.foreground) next.foreground = theme.foreground;
  if (theme.background) next.background = theme.background;
  if (theme.cursor) next.cursor = theme.cursor;
  theme.palette.slice(0, 16).forEach((color, index) => { next[PALETTE_KEYS[index]] = color; });
  terminal.options.theme = next;
  if (theme.background) element("terminal").style.background = theme.background;
}

function showStatus(status) {
  const badge = element("status");
  badge.hidden = !status;
  if (!status) return;
  const state = status.state || "idle";
  const parts = [status.kind ? `${state}: ${status.kind}` : state, status.title, status.msg].filter(Boolean);
  badge.textContent = parts.join(" | ");
  badge.className = `pill ${state}`;
}

function showReconnecting(waiting) {
  const entry = tabs.get(current);
  if (!entry || entry.ended) return;
  const banner = element("ended");
  banner.textContent = "Reconnecting to the shared terminal";
  banner.hidden = !waiting;
}

function showEnded(reason) {
  terminal.options.disableStdin = true;
  const banner = element("ended");
  banner.textContent = `Share ended: ${reason}`;
  banner.hidden = false;
}

function tickExpiry() {
  const entry = tabs.get(current);
  if (!expiresAt || !entry || entry.ended) {
    element("expiry").textContent = "";
    return;
  }
  const left = Math.max(0, expiresAt - Math.floor(Date.now() / 1000));
  const hours = Math.floor(left / 3600);
  const minutes = String(Math.floor(left / 60) % 60).padStart(hours ? 2 : 1, "0");
  const seconds = String(left % 60).padStart(2, "0");
  element("expiry").textContent = `ends in ${hours ? `${hours}:` : ""}${minutes}:${seconds}`;
}

function handle(id, message) {
  if (id !== current) return;
  switch (message.type) {
    case "hello": showReconnecting(false); break;
    case "reconnecting": showReconnecting(true); break;
    case "screen": terminal.reset(); terminal.write(message.vt); break;
    case "resize": terminal.resize(message.cols, message.rows); break;
    case "theme": applyTheme(message.theme); break;
    case "status": showReconnecting(false); showStatus(message.status); break;
    case "viewers": element("viewers").textContent = `watching: ${message.emails.join(", ")}`; break;
    case "ended": markEnded(id, message.reason); break;
  }
}

let lobbyDelay = 1000;

function connectLobby() {
  const lobby = new WebSocket(`${scheme()}//${location.host}/events`);
  lobby.onopen = () => { lobbyDelay = 1000; };
  lobby.onmessage = (event) => {
    const message = JSON.parse(event.data);
    if (message.type === "tabs") onTabs(message.tabs);
  };
  lobby.onclose = async () => {
    try {
      const page = await fetch("/", { cache: "no-store" });
      if ((await page.text()).includes('name="password"')) {
        location.reload();
        return;
      }
    } catch (error) {}
    setTimeout(connectLobby, lobbyDelay);
    lobbyDelay = Math.min(lobbyDelay * 2, 15000);
  };
}

connectLobby();
terminal.onData((data) => {
  if (!terminal.options.disableStdin && socket && socket.readyState === WebSocket.OPEN) {
    socket.send(JSON.stringify({ type: "input", data }));
  }
});
setInterval(tickExpiry, 1000);
