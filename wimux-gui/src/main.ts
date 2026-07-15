import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { PaneManager, type LayoutNode } from "./panes";

const mount = document.getElementById("terminal")!;
const paneManager = new PaneManager(mount, {
  onInput: (paneId, bytes) => {
    invoke("pane_input", { paneId, bytes }).catch(() => {});
  },
  onResize: (paneId, cols, rows) => {
    invoke("pane_resize", { paneId, cols, rows }).catch(() => {});
  },
  onFocus: (paneId) => {
    invoke("focus_pane", { paneId }).catch(() => {});
  },
  onSplit: (paneId, dir) => {
    invoke("split_pane", { paneId, dir }).catch(() => {});
  },
  onClose: (paneId) => {
    invoke("close_pane", { paneId }).catch(() => {});
  },
  onRatio: (nodeId, ratio) => {
    invoke("set_split_ratio", { nodeId, ratio }).catch(() => {});
  },
});

let activeSession: string | null = null;

// Disposition + flux serveur -> volets.
listen<[LayoutNode, number]>("window-layout", (e) => {
  paneManager.renderLayout(e.payload[0], e.payload[1]);
});
listen<[number, number[]]>("pane-snapshot", (e) => {
  paneManager.write(e.payload[0], new Uint8Array(e.payload[1]));
});
listen<[number, number[]]>("pane-output", (e) => {
  paneManager.write(e.payload[0], new Uint8Array(e.payload[1]));
});
listen<string>("pane-error", (e) => {
  console.error("erreur serveur:", e.payload);
});

type SessionDto = { name: string; attached: boolean; activity: boolean; bell: boolean };

async function switchTo(name: string) {
  if (name === activeSession) return;
  activeSession = name;
  // Effacement optimiste : la session qu'on regarde n'a plus d'indicateur, sans
  // attendre le prochain sondage.
  for (const s of lastSessions) {
    if (s.name === name) {
      s.activity = false;
      s.bell = false;
    }
  }
  paneManager.reset();
  await invoke("attach_session", { session: name }).catch((e) =>
    console.error("attach:", e),
  );
  renderRail(lastSessions);
}

let lastSessions: SessionDto[] = [];
let renaming = false; // suspend le sondage tant qu'une edition de nom est en cours

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
    const isActive = s.name === activeSession;
    if (!isActive && (s.bell || s.activity)) {
      // Cloche prioritaire sur l'activité ; rien pour la session active.
      const dot = document.createElement("span");
      dot.className = "dot " + (s.bell ? "bell" : "activity");
      dot.textContent = s.bell ? "🔔" : "";
      el.append(name, dot, close);
    } else {
      el.append(name, close);
    }
    container.append(el);
  }
}

function startRename(el: HTMLElement, oldName: string) {
  renaming = true;
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
    renaming = false;
    const to = input.value.trim();
    if (to && to !== oldName) {
      await invoke("rename_session", { from: oldName, to }).catch(() => {});
      if (activeSession === oldName) activeSession = to;
    }
    await refresh();
  };
  input.onkeydown = (ev) => {
    if (ev.key === "Enter") commit();
    else if (ev.key === "Escape") { committed = true; renaming = false; refresh(); }
  };
  input.onblur = () => commit();
}

async function refresh() {
  if (renaming) return; // ne pas reconstruire le rail pendant un renommage
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
