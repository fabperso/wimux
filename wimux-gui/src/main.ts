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

// --- W2 : barre d'onglets (fenêtres de la session GUI-attachée) -------------
type WindowInfo = { name: string | null };

const tabsEl = document.getElementById("tabs")!;
// Dernière fenêtre active connue : une bascule (changement d'`active`) impose un
// `reset()` du PaneManager avant le prochain window-layout (pane_id globaux, les
// snapshots frais repeignent le contenu).
let lastActiveWindow = -1;
let lastWindows: WindowInfo[] = [];

listen<[WindowInfo[], number]>("window-list", (e) => {
  const [windows, active] = e.payload;
  if (active !== lastActiveWindow) {
    paneManager.reset();
  }
  lastActiveWindow = active;
  lastWindows = windows;
  renderTabs(windows, active);
});

function renderTabs(windows: WindowInfo[], active: number) {
  tabsEl.innerHTML = "";
  windows.forEach((win, i) => {
    const tab = document.createElement("div");
    tab.className = "tab" + (i === active ? " active" : "");
    const label = document.createElement("span");
    label.className = "tab-label";
    label.textContent = win.name ?? String(i + 1);
    tab.appendChild(label);
    let clickTimer: number | null = null;
    label.ondblclick = (ev) => {
      ev.stopPropagation();
      if (clickTimer !== null) { clearTimeout(clickTimer); clickTimer = null; }
      startTabRename(tab, i, win.name ?? "");
    };
    // Le `×` est masqué s'il ne reste qu'une fenêtre (fermeture interdite).
    if (windows.length > 1) {
      const close = document.createElement("span");
      close.className = "tab-close";
      close.textContent = "×";
      close.onclick = (ev) => {
        ev.stopPropagation();
        invoke("close_window", { index: i }).catch(() => {});
      };
      tab.appendChild(close);
    }
    tab.onclick = () => {
      if (clickTimer !== null) return; // 2e clic d'un double-clic : laisse ondblclick
      clickTimer = window.setTimeout(() => {
        clickTimer = null;
        invoke("select_window", { index: i }).catch(() => {});
      }, 200);
    };
    tabsEl.appendChild(tab);
  });
  const add = document.createElement("button");
  add.className = "tab-add";
  add.textContent = "+";
  add.title = "Nouvel onglet";
  add.onclick = () => { invoke("new_window", {}).catch(() => {}); };
  tabsEl.appendChild(add);
}

function startTabRename(tab: HTMLElement, index: number, oldName: string) {
  const input = document.createElement("input");
  input.className = "tab-edit";
  input.value = oldName;
  tab.replaceChildren(input);
  input.focus();
  input.select();
  let committed = false;
  const commit = () => {
    if (committed) return;
    committed = true;
    const name = input.value.trim();
    // Nom vide => le serveur remet le nom à None (affiche la position).
    invoke("rename_window", { index, name }).catch(() => {});
  };
  input.onkeydown = (ev) => {
    if (ev.key === "Enter") commit();
    else if (ev.key === "Escape") {
      committed = true;
      renderTabs(lastWindows, lastActiveWindow);
    }
  };
  input.onblur = () => commit();
}

type SessionDto = {
  name: string;
  attached: boolean;
  activity: boolean;
  bell: boolean;
  agent: boolean;
  agent_status: string | null;
  group: string | null;
};

type AgentTemplateDto = { name: string };

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
  lastActiveWindow = -1; // force le rendu des onglets + reset au 1er window-list de la nouvelle session
  await invoke("attach_session", { session: name }).catch((e) =>
    console.error("attach:", e),
  );
  renderRail(lastSessions);
}

let lastSessions: SessionDto[] = [];
let renaming = false; // suspend le sondage tant qu'une edition de nom est en cours

function agentStatusGlyph(status: string | null): string {
  switch (status) {
    case "Working": return "⚙";
    case "Idle": return "○";
    case "Attention": return "❗";
    case "Done": return "✓";
    case "Error": return "✗";
    default: return "○";
  }
}

function agentStatusClass(status: string | null): string {
  switch (status) {
    case "Working": return "working";
    case "Idle": return "idle";
    case "Attention": return "attention";
    case "Done": return "done";
    case "Error": return "error";
    default: return "idle";
  }
}

function renderSession(s: SessionDto): HTMLElement {
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
  if (s.agent) {
    // Session agent : glyphe de statut (remplace les pastilles G4).
    const glyph = document.createElement("span");
    glyph.className = "agent-glyph " + agentStatusClass(s.agent_status);
    glyph.textContent = agentStatusGlyph(s.agent_status);
    glyph.title = s.agent_status ?? "agent";
    el.append(name, glyph, close);
  } else if (!isActive && (s.bell || s.activity)) {
    // Cloche prioritaire sur l'activité ; rien pour la session active.
    const dot = document.createElement("span");
    dot.className = "dot " + (s.bell ? "bell" : "activity");
    dot.textContent = s.bell ? "🔔" : "";
    el.append(name, dot, close);
  } else {
    el.append(name, close);
  }
  return el;
}

function renderBatchHeader(group: string, members: SessionDto[]): HTMLElement {
  const el = document.createElement("div");
  el.className = "batch-header";
  const title = document.createElement("span");
  title.className = "batch-name";
  title.textContent = group;
  // Agrégat des statuts : ⚙ Working, ✓ Done, ✗ Error.
  let working = 0, done = 0, error = 0;
  for (const m of members) {
    if (m.agent_status === "Working") working++;
    else if (m.agent_status === "Done") done++;
    else if (m.agent_status === "Error") error++;
  }
  const agg = document.createElement("span");
  agg.className = "batch-agg";
  agg.textContent = `⚙${working} ✓${done} ✗${error}`;
  const close = document.createElement("span");
  close.className = "batch-close";
  close.textContent = "×";
  close.title = "Fermer le lot";
  close.onclick = async (ev) => {
    ev.stopPropagation();
    for (const m of members) {
      await invoke("kill_session", { name: m.name }).catch(() => {});
    }
    await refresh();
  };
  el.append(title, agg, close);
  return el;
}

function renderRail(sessions: SessionDto[]) {
  lastSessions = sessions;
  const container = document.getElementById("sessions")!;
  container.innerHTML = "";
  // Regrouper par `group` en préservant l'ordre d'apparition ; les sessions
  // sans group sont rendues comme avant, après les lots.
  const groups = new Map<string, SessionDto[]>();
  const ungrouped: SessionDto[] = [];
  for (const s of sessions) {
    if (s.group) {
      let arr = groups.get(s.group);
      if (!arr) { arr = []; groups.set(s.group, arr); }
      arr.push(s);
    } else {
      ungrouped.push(s);
    }
  }
  for (const [group, members] of groups) {
    container.append(renderBatchHeader(group, members));
    for (const s of members) container.append(renderSession(s));
  }
  for (const s of ungrouped) container.append(renderSession(s));
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

const agentModal = document.getElementById("agent-modal")!;
const agentTemplateSel = document.getElementById("agent-template") as HTMLSelectElement;
const agentPrompt = document.getElementById("agent-prompt") as HTMLTextAreaElement;
const agentCwd = document.getElementById("agent-cwd") as HTMLInputElement;
const agentName = document.getElementById("agent-name") as HTMLInputElement;
const agentError = document.getElementById("agent-error")!;

async function openAgentModal() {
  agentError.textContent = "";
  agentPrompt.value = "";
  agentCwd.value = "";
  agentName.value = "";
  agentTemplateSel.innerHTML = "";
  try {
    const templates = await invoke<AgentTemplateDto[]>("list_agent_templates");
    for (const t of templates) {
      const opt = document.createElement("option");
      opt.value = t.name;
      opt.textContent = t.name;
      agentTemplateSel.append(opt);
    }
  } catch (e) {
    agentError.textContent = "Impossible de charger les modèles : " + e;
  }
  agentModal.classList.remove("hidden");
}

function closeAgentModal() {
  agentModal.classList.add("hidden");
}

document.getElementById("new-agent")!.onclick = openAgentModal;
document.getElementById("agent-cancel")!.onclick = closeAgentModal;
document.getElementById("agent-launch")!.onclick = async () => {
  const template = agentTemplateSel.value;
  if (!template) {
    agentError.textContent = "Choisissez un modèle.";
    return;
  }
  const prompt = agentPrompt.value;
  const cwd = agentCwd.value.trim();
  const name = agentName.value.trim();
  try {
    const created = await invoke<string>("create_agent", {
      template,
      prompt,
      cwd: cwd || null,
      name: name || null,
    });
    closeAgentModal();
    await refresh();
    await switchTo(created);
  } catch (e) {
    agentError.textContent = "Échec : " + e;
  }
};

const batchModal = document.getElementById("batch-modal")!;
const batchRepo = document.getElementById("batch-repo") as HTMLInputElement;
const batchTemplateSel = document.getElementById("batch-template") as HTMLSelectElement;
const batchPrompt = document.getElementById("batch-prompt") as HTMLTextAreaElement;
const batchCount = document.getElementById("batch-count") as HTMLInputElement;
const batchError = document.getElementById("batch-error")!;

async function openBatchModal() {
  batchError.textContent = "";
  batchRepo.value = "";
  batchPrompt.value = "";
  batchCount.value = "2";
  batchTemplateSel.innerHTML = "";
  try {
    const templates = await invoke<AgentTemplateDto[]>("list_agent_templates");
    for (const t of templates) {
      const opt = document.createElement("option");
      opt.value = t.name;
      opt.textContent = t.name;
      batchTemplateSel.append(opt);
    }
  } catch (e) {
    batchError.textContent = "Impossible de charger les modèles : " + e;
  }
  batchModal.classList.remove("hidden");
}

function closeBatchModal() {
  batchModal.classList.add("hidden");
}

document.getElementById("new-batch")!.onclick = openBatchModal;
document.getElementById("batch-cancel")!.onclick = closeBatchModal;
document.getElementById("batch-launch")!.onclick = async () => {
  const template = batchTemplateSel.value;
  if (!template) {
    batchError.textContent = "Choisissez un modèle.";
    return;
  }
  const baseRepo = batchRepo.value.trim();
  if (!baseRepo) {
    batchError.textContent = "Indiquez le repo de base.";
    return;
  }
  const count = parseInt(batchCount.value, 10);
  if (!Number.isFinite(count) || count < 1) {
    batchError.textContent = "Le nombre d'agents doit être ≥ 1.";
    return;
  }
  const prompt = batchPrompt.value;
  try {
    const group = await invoke<string>("create_batch", {
      template,
      prompt,
      baseRepo,
      count,
    });
    closeBatchModal();
    await refresh();
    console.log("lot créé:", group);
  } catch (e) {
    batchError.textContent = "Échec : " + e;
  }
};

document.getElementById("new-session")!.onclick = async () => {
  const name = await invoke<string>("create_session", { name: null }).catch(() => null);
  await refresh();
  if (name) await switchTo(name);
};

// Sondage périodique (une session créée/fermée ailleurs apparaît/disparaît).
refresh();
setInterval(refresh, 1000);
