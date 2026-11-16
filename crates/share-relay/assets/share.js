const PALETTE_KEYS = [
  "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
  "brightBlack", "brightRed", "brightGreen", "brightYellow", "brightBlue", "brightMagenta", "brightCyan", "brightWhite",
];

const element = (id) => document.getElementById(id);
const terminal = new Terminal({ cols: 80, rows: 24, disableStdin: true, scrollback: 5000 });
terminal.open(element("terminal"));

let expiresAt = 0;
let ended = false;

function socketUrl() {
  const scheme = location.protocol === "https:" ? "wss:" : "ws:";
  return `${scheme}//${location.host}${location.pathname.replace(/\/$/, "")}/ws`;
}

function showHello(share, email) {
  element("label").textContent = `${share.label} on ${share.host}`;
  const mode = element("mode");
  mode.textContent = share.mode === "input" ? "you can type" : "view only";
  mode.className = `pill ${share.mode}`;
  terminal.options.disableStdin = share.mode !== "input";
  expiresAt = share.expires_at_unix;
  document.title = `${share.label} (${email})`;
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

function showEnded(reason) {
  ended = true;
  terminal.options.disableStdin = true;
  const banner = element("ended");
  banner.textContent = `Share ended: ${reason}`;
  banner.hidden = false;
}

function tickExpiry() {
  if (!expiresAt || ended) return;
  const left = Math.max(0, expiresAt - Math.floor(Date.now() / 1000));
  const minutes = Math.floor(left / 60);
  const seconds = String(left % 60).padStart(2, "0");
  element("expiry").textContent = `ends in ${minutes}:${seconds}`;
}

function handle(message) {
  switch (message.type) {
    case "hello": showHello(message.share, message.email); break;
    case "screen": terminal.reset(); terminal.write(message.vt); break;
    case "resize": terminal.resize(message.cols, message.rows); break;
    case "theme": applyTheme(message.theme); break;
    case "status": showStatus(message.status); break;
    case "viewers": element("viewers").textContent = `watching: ${message.emails.join(", ")}`; break;
    case "ended": showEnded(message.reason); break;
  }
}

const socket = new WebSocket(socketUrl());
socket.binaryType = "arraybuffer";
socket.onmessage = (event) => {
  if (typeof event.data === "string") handle(JSON.parse(event.data));
  else terminal.write(new Uint8Array(event.data));
};
socket.onclose = () => { if (!ended) showEnded("connection closed"); };
terminal.onData((data) => {
  if (!terminal.options.disableStdin && socket.readyState === WebSocket.OPEN) {
    socket.send(JSON.stringify({ type: "input", data }));
  }
});

setInterval(tickExpiry, 1000);
tickExpiry();
