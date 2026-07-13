import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

const term = new Terminal({ fontFamily: "Cascadia Mono, Consolas, monospace", fontSize: 14 });
const fit = new FitAddon();
term.loadAddon(fit);
term.open(document.getElementById("terminal")!);
fit.fit();
window.addEventListener("resize", () => fit.fit());

let activeSession: string | null = null;
let activePane = 0;

// Sortie serveur -> terminal.
listen<[number, number[]]>("pane-snapshot", (e) => { activePane = e.payload[0]; term.write(new Uint8Array(e.payload[1])); });
listen<[number, number[]]>("pane-output", (e) => { activePane = e.payload[0]; term.write(new Uint8Array(e.payload[1])); });
listen<string>("pane-error", (e) => { term.write(`\r\n[erreur serveur: ${e.payload}]\r\n`); });

// Frappe -> serveur.
term.onData((data) => {
  const bytes = Array.from(new TextEncoder().encode(data));
  invoke("pane_input", { paneId: activePane, bytes }).catch(() => {});
});

type SessionDto = { name: string; attached: boolean };

async function switchTo(name: string) {
  if (name === activeSession) return;
  activeSession = name;
  term.clear();
  await invoke("attach_session", { session: name }).catch((e) => term.write(`\r\n[${e}]\r\n`));
  renderRail(lastSessions);
}

let lastSessions: SessionDto[] = [];

function renderRail(sessions: SessionDto[]) {
  lastSessions = sessions;
  const container = document.getElementById("sessions")!;
  container.innerHTML = "";
  for (const s of sessions) {
    const el = document.createElement("div");
    el.className = "session" + (s.name === activeSession ? " active" : "");
    const name = document.createElement("span");
    name.className = "name";
    name.textContent = s.name;
    let clickTimer: number | null = null;
    name.ondblclick = (ev) => {
      ev.stopPropagation();
      if (clickTimer !== null) { clearTimeout(clickTimer); clickTimer = null; }
      startRename(el, s.name);
    };
    const close = document.createElement("span");
    close.className = "close";
    close.textContent = "×";
    close.onclick = async (ev) => { ev.stopPropagation(); await invoke("kill_session", { name: s.name }).catch(() => {}); await refresh(); };
    el.onclick = () => {
      if (clickTimer !== null) return; // 2e clic d'un double-clic : ignore, laisse ondblclick gerer
      clickTimer = window.setTimeout(() => { clickTimer = null; switchTo(s.name); }, 200);
    };
    el.append(name, close);
    container.append(el);
  }
}

function startRename(el: HTMLElement, oldName: string) {
  const input = document.createElement("input");
  input.className = "name-edit";
  input.value = oldName;
  el.replaceChildren(input);
  input.focus();
  input.select();
  let committed = false;
  const commit = async () => {
    if (committed) return;
    committed = true;
    const to = input.value.trim();
    if (to && to !== oldName) {
      await invoke("rename_session", { from: oldName, to }).catch(() => {});
      if (activeSession === oldName) activeSession = to;
    }
    await refresh();
  };
  input.onkeydown = (ev) => {
    if (ev.key === "Enter") commit();
    else if (ev.key === "Escape") { committed = true; refresh(); }
  };
  input.onblur = () => commit();
}

async function refresh() {
  try {
    const sessions = await invoke<SessionDto[]>("list_sessions");
    renderRail(sessions); // peuple le rail + lastSessions d'abord
    // Auto-sélection : si aucune session active mais il en existe, prendre la première.
    if (!activeSession && sessions.length > 0) { await switchTo(sessions[0].name); }
  } catch { /* serveur absent : on garde l'affichage precedent (rail non modifie) */ }
}

document.getElementById("new-session")!.onclick = async () => {
  const name = await invoke<string>("create_session", { name: null }).catch(() => null);
  await refresh();
  if (name) await switchTo(name);
};

// Sondage périodique (une session créée/fermée ailleurs apparaît/disparaît).
refresh();
setInterval(refresh, 1000);
