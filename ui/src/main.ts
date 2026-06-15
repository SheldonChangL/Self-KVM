import "./styles.css";

/* ------------------------------------------------------------------ *
 * Tauri bridge (degrades gracefully to a self-contained demo in a
 * plain browser, so the UI is fully inspectable without the backend).
 * ------------------------------------------------------------------ */
type Invoke = (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;
type Listen = (event: string, cb: (e: { payload: unknown }) => void) => Promise<unknown>;

let invoke: Invoke | null = null;
let listen: Listen | null = null;

async function initBridge(): Promise<void> {
  if (!(window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__) return;
  try {
    const core = await import("@tauri-apps/api/core");
    const evt = await import("@tauri-apps/api/event");
    invoke = core.invoke as Invoke;
    listen = evt.listen as unknown as Listen;
  } catch {
    /* stay in demo mode */
  }
}

const isLive = () => invoke !== null;

/* ------------------------------------------------------------------ *
 * State
 * ------------------------------------------------------------------ */
type Screen = {
  id: string;
  name: string;
  w: number;
  h: number;
  col: number;
  row: number;
  local: boolean;
};

const COLS = 4;
const ROWS = 3;

let mode: "server" | "client" = "server";
let running = false;
let activeScreen: string | null = null;
let clientCount = 0;

let screens: Screen[] = [
  { id: "s1", name: "studio", w: 1920, h: 1080, col: 1, row: 1, local: true },
  { id: "s2", name: "laptop", w: 1280, h: 800, col: 2, row: 1, local: false },
];

/* ------------------------------------------------------------------ *
 * Tiny DOM helpers
 * ------------------------------------------------------------------ */
const $ = <T extends Element = HTMLElement>(sel: string, root: ParentNode = document): T =>
  root.querySelector(sel) as T;
const $all = <T extends Element = HTMLElement>(sel: string, root: ParentNode = document): T[] =>
  Array.from(root.querySelectorAll(sel)) as T[];

const BRAND_SVG = `
<svg viewBox="0 0 64 64" fill="none" xmlns="http://www.w3.org/2000/svg">
  <rect x="3" y="3" width="58" height="58" rx="12" fill="#0b1426"/>
  <rect x="14" y="13" width="36" height="15" rx="4" stroke="#2ee6d6" stroke-width="2.4" fill="rgba(46,230,214,0.08)"/>
  <rect x="14" y="36" width="36" height="15" rx="4" stroke="#2ee6d6" stroke-width="2.4" fill="rgba(46,230,214,0.08)"/>
  <line x1="32" y1="28" x2="32" y2="36" stroke="#f9b13c" stroke-width="3"/>
  <circle cx="32" cy="32" r="3.4" fill="#f9b13c"/>
</svg>`;

/* ------------------------------------------------------------------ *
 * Shell
 * ------------------------------------------------------------------ */
function mountShell(): void {
  const app = $("#app");
  app.innerHTML = `
    <header class="deck-header enter">
      <div class="brand">
        ${BRAND_SVG}
        <div>
          <div class="wordmark">SELF<b>·</b>KVM</div>
          <div class="tagline">one keyboard · every machine</div>
        </div>
      </div>
      <div class="header-spacer"></div>
      <div class="status-pill" id="pill"><span class="dot"></span><span id="pill-text">Offline</span></div>
    </header>

    <div class="deck-body">
      <div class="col">
        <div class="mode-toggle enter" id="mode-toggle" style="animation-delay:.05s">
          <button data-mode="server" class="active">Server<span class="sub">share this machine's input</span></button>
          <button data-mode="client">Client<span class="sub">receive input from a server</span></button>
        </div>
        <div id="mode-panel" class="col" style="gap:16px;flex:1;min-height:0"></div>
      </div>

      <div class="col">
        <div class="panel enter" style="animation-delay:.12s">
          <div class="panel-title">Telemetry</div>
          <div class="telemetry">
            <div class="metric"><div class="k">Role</div><div class="v cyan" id="m-role">Server</div></div>
            <div class="metric"><div class="k">State</div><div class="v" id="m-state">Idle</div></div>
            <div class="metric"><div class="k">Active screen</div><div class="v amber" id="m-active">—</div></div>
            <div class="metric"><div class="k">Clients</div><div class="v" id="m-clients">0</div></div>
          </div>
        </div>
        <div class="panel fill enter" style="animation-delay:.18s">
          <div class="panel-title">Signal Log <span class="hint">live</span></div>
          <div class="console" id="console"></div>
        </div>
      </div>
    </div>
  `;

  $all<HTMLButtonElement>("#mode-toggle button").forEach((b) =>
    b.addEventListener("click", () => setMode(b.dataset.mode as "server" | "client")),
  );

  renderPanel();
}

function setMode(m: "server" | "client"): void {
  if (running) {
    log("red", "BLOCKED", "stop the current session before switching role");
    return;
  }
  mode = m;
  $all<HTMLButtonElement>("#mode-toggle button").forEach((b) =>
    b.classList.toggle("active", b.dataset.mode === m),
  );
  $("#m-role").textContent = m === "server" ? "Server" : "Client";
  renderPanel();
}

/* ------------------------------------------------------------------ *
 * Mode panels
 * ------------------------------------------------------------------ */
function renderPanel(): void {
  const host = $("#mode-panel");
  if (mode === "server") {
    host.innerHTML = `
      <div class="panel fill">
        <div class="panel-title">Screen Layout
          <span class="hint">drag tiles to arrange · click ◇ to set this machine</span>
        </div>
        <div class="layout-stage" id="stage">
          <div class="grid-cells" id="cells" style="--cols:${COLS};--rows:${ROWS}"></div>
          <svg class="connectors" id="connectors"></svg>
        </div>
        <div class="roster" id="roster"></div>
        <div style="margin-top:12px">
          <button class="btn small ghost" id="add-screen">+ Add screen</button>
        </div>
      </div>
      <div class="panel">
        <div class="panel-title">Network</div>
        <div class="field-row three">
          <label class="field">Bind address
            <input id="bind" value="0.0.0.0" spellcheck="false" />
          </label>
          <label class="field">Port
            <input id="port" value="24800" inputmode="numeric" />
          </label>
          <div class="field" style="justify-content:flex-end">
            <div class="switch" id="tls-server"><span class="track"></span><span>TLS</span></div>
          </div>
        </div>
        <div class="action-row">
          <button class="btn primary" id="go">▶ Start Server</button>
          <button class="btn danger" id="halt" style="display:none">■ Stop</button>
        </div>
        <div class="hint-line">
          The cursor crosses to a neighbour when it runs off a shared edge.
          <b>This machine</b> is the amber tile.
        </div>
      </div>
    `;
    $("#add-screen").addEventListener("click", addScreen);
    $("#go").addEventListener("click", startServer);
    $("#halt").addEventListener("click", stopSession);
    setupSwitch("#tls-server");
    renderLayout();
    renderRoster();
  } else {
    host.innerHTML = `
      <div class="panel fill">
        <div class="panel-title">Connect to Server</div>
        <div class="field-row two">
          <label class="field">Server address
            <input id="srv-addr" value="192.168.1.10:24800" spellcheck="false" />
          </label>
          <label class="field">This screen's name
            <input id="cli-name" value="laptop" spellcheck="false" />
          </label>
        </div>
        <div class="field-row three" style="margin-top:14px">
          <label class="field">Width
            <input id="cli-w" value="1280" inputmode="numeric" />
          </label>
          <label class="field">Height
            <input id="cli-h" value="800" inputmode="numeric" />
          </label>
          <div class="field" style="justify-content:flex-end">
            <div class="switch" id="tls-client"><span class="track"></span><span>TLS</span></div>
          </div>
        </div>
        <div class="hint-line">
          The name must match a screen defined on the server's layout, or the
          server will refuse the connection.
        </div>
        <div class="action-row">
          <button class="btn primary" id="go">▶ Connect</button>
          <button class="btn danger" id="halt" style="display:none">■ Disconnect</button>
        </div>
      </div>
    `;
    $("#go").addEventListener("click", startClient);
    $("#halt").addEventListener("click", stopSession);
    setupSwitch("#tls-client");
  }
  reflectRunning();
}

function setupSwitch(sel: string): void {
  const sw = $(sel);
  sw.addEventListener("click", () => sw.classList.toggle("on"));
}

/* ------------------------------------------------------------------ *
 * Layout editor
 * ------------------------------------------------------------------ */
function renderLayout(): void {
  const cells = $("#cells");
  cells.innerHTML = "";
  for (let r = 0; r < ROWS; r++) {
    for (let c = 0; c < COLS; c++) {
      const cell = document.createElement("div");
      cell.className = "cell";
      cell.dataset.col = String(c);
      cell.dataset.row = String(r);
      cell.addEventListener("dragover", (e) => {
        e.preventDefault();
        cell.classList.add("drop-hot");
      });
      cell.addEventListener("dragleave", () => cell.classList.remove("drop-hot"));
      cell.addEventListener("drop", (e) => {
        e.preventDefault();
        cell.classList.remove("drop-hot");
        const id = e.dataTransfer?.getData("text/plain");
        if (id) moveScreen(id, c, r);
      });
      cells.appendChild(cell);
    }
  }

  for (const s of screens) {
    const cell = $(`.cell[data-col="${s.col}"][data-row="${s.row}"]`);
    if (!cell) continue;
    const tile = document.createElement("div");
    tile.className = "screen-tile" + (s.local ? " local" : "") + (activeScreen === s.name ? " active" : "");
    tile.draggable = true;
    tile.style.inset = "5px";
    tile.innerHTML = `
      <span class="tile-x" title="remove">✕</span>
      <span class="nm">◇ ${s.name}${s.local ? '<span class="badge this">this</span>' : ""}</span>
      <span class="res">${s.w}×${s.h}</span>
    `;
    tile.addEventListener("dragstart", (e) => {
      e.dataTransfer?.setData("text/plain", s.id);
      tile.classList.add("dragging");
    });
    tile.addEventListener("dragend", () => tile.classList.remove("dragging"));
    $(".nm", tile).addEventListener("click", (e) => {
      e.stopPropagation();
      setLocal(s.id);
    });
    $(".tile-x", tile).addEventListener("click", (e) => {
      e.stopPropagation();
      removeScreen(s.id);
    });
    cell.style.position = "relative";
    cell.appendChild(tile);
  }
  requestAnimationFrame(drawConnectors);
}

function drawConnectors(): void {
  const svg = $<SVGSVGElement>("#connectors");
  const stage = $("#stage");
  if (!svg || !stage) return;
  const sr = stage.getBoundingClientRect();
  svg.innerHTML = "";
  const center = (s: Screen): [number, number] | null => {
    const cell = $(`.cell[data-col="${s.col}"][data-row="${s.row}"] .screen-tile`);
    if (!cell) return null;
    const r = cell.getBoundingClientRect();
    return [r.left - sr.left + r.width / 2, r.top - sr.top + r.height / 2];
  };
  for (const a of screens) {
    for (const b of screens) {
      if (a.id >= b.id) continue;
      const adjacent =
        (a.row === b.row && Math.abs(a.col - b.col) === 1) ||
        (a.col === b.col && Math.abs(a.row - b.row) === 1);
      if (!adjacent) continue;
      const pa = center(a);
      const pb = center(b);
      if (!pa || !pb) continue;
      const line = document.createElementNS("http://www.w3.org/2000/svg", "line");
      line.setAttribute("x1", String(pa[0]));
      line.setAttribute("y1", String(pa[1]));
      line.setAttribute("x2", String(pb[0]));
      line.setAttribute("y2", String(pb[1]));
      svg.appendChild(line);
    }
  }
}

function moveScreen(id: string, col: number, row: number): void {
  const dragged = screens.find((s) => s.id === id);
  if (!dragged) return;
  const occupant = screens.find((s) => s.col === col && s.row === row && s.id !== id);
  if (occupant) {
    occupant.col = dragged.col;
    occupant.row = dragged.row;
  }
  dragged.col = col;
  dragged.row = row;
  renderLayout();
  renderRoster();
}

function setLocal(id: string): void {
  screens = screens.map((s) => ({ ...s, local: s.id === id }));
  renderLayout();
  renderRoster();
}

function addScreen(): void {
  const slot = firstFreeCell();
  if (!slot) {
    log("red", "FULL", "no free grid cells — remove a screen first");
    return;
  }
  screens.push({
    id: "s" + Date.now().toString(36),
    name: "screen" + (screens.length + 1),
    w: 1920,
    h: 1080,
    col: slot[0],
    row: slot[1],
    local: false,
  });
  renderLayout();
  renderRoster();
}

function removeScreen(id: string): void {
  const wasLocal = screens.find((s) => s.id === id)?.local;
  screens = screens.filter((s) => s.id !== id);
  if (wasLocal && screens.length) screens[0].local = true;
  renderLayout();
  renderRoster();
}

function firstFreeCell(): [number, number] | null {
  for (let r = 0; r < ROWS; r++)
    for (let c = 0; c < COLS; c++)
      if (!screens.some((s) => s.col === c && s.row === r)) return [c, r];
  return null;
}

function renderRoster(): void {
  const host = $("#roster");
  host.innerHTML = "";
  for (const s of screens) {
    const row = document.createElement("div");
    row.className = "roster-row";
    row.innerHTML = `
      <input value="${s.name}" data-k="name" spellcheck="false" />
      <input value="${s.w}" data-k="w" inputmode="numeric" />
      <input value="${s.h}" data-k="h" inputmode="numeric" />
      <button class="btn small ${s.local ? "" : "ghost"}" data-local>${s.local ? "● this" : "set"}</button>
      <button class="btn small ghost" data-del>✕</button>
    `;
    $<HTMLInputElement>('[data-k="name"]', row).addEventListener("input", (e) => {
      s.name = (e.target as HTMLInputElement).value.trim() || s.name;
      renderLayout();
    });
    $<HTMLInputElement>('[data-k="w"]', row).addEventListener("input", (e) => {
      s.w = parseInt((e.target as HTMLInputElement).value) || s.w;
      renderLayout();
    });
    $<HTMLInputElement>('[data-k="h"]', row).addEventListener("input", (e) => {
      s.h = parseInt((e.target as HTMLInputElement).value) || s.h;
      renderLayout();
    });
    $("[data-local]", row).addEventListener("click", () => setLocal(s.id));
    $("[data-del]", row).addEventListener("click", () => removeScreen(s.id));
    host.appendChild(row);
  }
}

/* ------------------------------------------------------------------ *
 * Actions
 * ------------------------------------------------------------------ */
async function startServer(): Promise<void> {
  const local = screens.find((s) => s.local);
  if (!local) {
    log("red", "ERROR", "mark one screen as 'this machine'");
    return;
  }
  const setup = {
    bind: ($("#bind") as HTMLInputElement).value.trim() || "0.0.0.0",
    port: parseInt(($("#port") as HTMLInputElement).value) || 24800,
    local_screen: local.name,
    tls: $("#tls-server").classList.contains("on"),
    screens: screens.map((s) => ({ name: s.name, w: s.w, h: s.h, col: s.col, row: s.row })),
  };
  log("muted", "INIT", `binding ${setup.bind}:${setup.port} · ${screens.length} screens`);
  if (isLive()) {
    try {
      await invoke!("start_server", { setup });
      enterRunning("server");
    } catch (e) {
      log("red", "ERROR", String(e));
    }
  } else {
    demoServer(setup.bind, setup.port);
  }
}

async function startClient(): Promise<void> {
  const setup = {
    server_addr: ($("#srv-addr") as HTMLInputElement).value.trim(),
    name: ($("#cli-name") as HTMLInputElement).value.trim(),
    width: parseInt(($("#cli-w") as HTMLInputElement).value) || 1280,
    height: parseInt(($("#cli-h") as HTMLInputElement).value) || 800,
    tls: $("#tls-client").classList.contains("on"),
  };
  log("muted", "DIAL", `connecting to ${setup.server_addr} as ${setup.name}`);
  if (isLive()) {
    try {
      await invoke!("start_client", { setup });
      enterRunning("client");
    } catch (e) {
      log("red", "ERROR", String(e));
    }
  } else {
    demoClient(setup.server_addr, setup.name);
  }
}

async function stopSession(): Promise<void> {
  if (isLive()) {
    try {
      await invoke!("stop");
    } catch (e) {
      log("red", "ERROR", String(e));
    }
  }
  running = false;
  activeScreen = null;
  clientCount = 0;
  setStatus("offline");
  $("#m-state").textContent = "Idle";
  $("#m-active").textContent = "—";
  $("#m-clients").textContent = "0";
  log("muted", "HALT", "session stopped");
  renderLayout();
  reflectRunning();
}

function enterRunning(role: "server" | "client"): void {
  running = true;
  setStatus(role === "server" ? "live" : "linked");
  $("#m-state").textContent = role === "server" ? "Listening" : "Linking";
  reflectRunning();
}

function reflectRunning(): void {
  const go = $("#go");
  const halt = $("#halt");
  if (go) go.style.display = running ? "none" : "";
  if (halt) halt.style.display = running ? "" : "none";
}

/* ------------------------------------------------------------------ *
 * Status + console
 * ------------------------------------------------------------------ */
function setStatus(kind: "offline" | "live" | "linked"): void {
  const pill = $("#pill");
  const text = $("#pill-text");
  pill.className = "status-pill" + (kind === "offline" ? "" : " " + kind);
  text.textContent = kind === "offline" ? "Offline" : kind === "live" ? "Server Live" : "Client Linked";
}

function log(tag: "" | "amber" | "red" | "muted", label: string, msg: string): void {
  const con = $("#console");
  if (!con) return;
  const now = new Date();
  const ts = `${pad(now.getHours())}:${pad(now.getMinutes())}:${pad(now.getSeconds())}`;
  const line = document.createElement("div");
  line.className = "log-line";
  line.innerHTML = `<span class="t">${ts}</span><span class="tag ${tag}">${label}</span><span class="msg"></span>`;
  $(".msg", line).textContent = msg;
  con.appendChild(line);
  con.scrollTop = con.scrollHeight;
  while (con.childElementCount > 200) con.removeChild(con.firstChild as Node);
}

const pad = (n: number) => String(n).padStart(2, "0");

function handleStatus(payload: { kind: string; detail: string }): void {
  switch (payload.kind) {
    case "listening":
      log("", "LISTEN", `server up on ${payload.detail}`);
      break;
    case "client_connected":
      clientCount += 1;
      $("#m-clients").textContent = String(clientCount);
      log("amber", "LINK", `client "${payload.detail}" connected`);
      break;
    case "client_disconnected":
      clientCount = Math.max(0, clientCount - 1);
      $("#m-clients").textContent = String(clientCount);
      log("muted", "DROP", `client "${payload.detail}" left`);
      break;
    case "active_screen":
      activeScreen = payload.detail;
      $("#m-active").textContent = payload.detail;
      log("", "FOCUS", `control → ${payload.detail}`);
      renderLayout();
      break;
    case "grab":
      log("muted", "GRAB", `local input ${payload.detail === "true" ? "captured" : "released"}`);
      break;
    case "connecting":
      log("muted", "DIAL", "connecting…");
      break;
    case "connected":
      $("#m-state").textContent = "Linked";
      log("", "READY", "handshake complete");
      break;
    case "entered":
      log("amber", "FOCUS", "cursor entered this screen");
      break;
    case "left":
      log("muted", "BLUR", "cursor left this screen");
      break;
    case "disconnected":
      running = false;
      setStatus("offline");
      $("#m-state").textContent = "Idle";
      reflectRunning();
      log("red", "DOWN", payload.detail || "disconnected");
      break;
    case "stopped":
      break;
  }
}

/* ------------------------------------------------------------------ *
 * Demo mode (browser, no backend)
 * ------------------------------------------------------------------ */
function demoServer(bind: string, port: number): void {
  enterRunning("server");
  handleStatus({ kind: "listening", detail: `${bind}:${port}` });
  const other = screens.find((s) => !s.local);
  setTimeout(() => other && handleStatus({ kind: "client_connected", detail: other.name }), 700);
  setTimeout(() => other && handleStatus({ kind: "grab", detail: "true" }), 1500);
  setTimeout(() => other && handleStatus({ kind: "active_screen", detail: other.name }), 1600);
  setTimeout(() => handleStatus({ kind: "grab", detail: "false" }), 3200);
  const local = screens.find((s) => s.local);
  setTimeout(() => local && handleStatus({ kind: "active_screen", detail: local.name }), 3300);
}

function demoClient(addr: string, name: string): void {
  enterRunning("client");
  handleStatus({ kind: "connecting", detail: addr });
  setTimeout(() => handleStatus({ kind: "connected", detail: "" }), 600);
  setTimeout(() => handleStatus({ kind: "entered", detail: name }), 1600);
  setTimeout(() => handleStatus({ kind: "left", detail: name }), 3200);
}

/* ------------------------------------------------------------------ *
 * Boot
 * ------------------------------------------------------------------ */
async function boot(): Promise<void> {
  await initBridge();
  mountShell();

  if (isLive() && listen) {
    await listen("kvm://status", (e) => handleStatus(e.payload as { kind: string; detail: string }));
    await listen("kvm://log", (e) => log("muted", "LOG", String(e.payload)));
    try {
      const st = (await invoke!("get_state")) as { running: boolean; mode: string | null };
      if (st.running && st.mode) enterRunning(st.mode as "server" | "client");
    } catch {
      /* ignore */
    }
    log("muted", "BOOT", "control deck online — backend linked");
  } else {
    log("muted", "BOOT", "control deck online — demo mode (no backend)");
  }

  window.addEventListener("resize", () => mode === "server" && drawConnectors());
}

boot();
