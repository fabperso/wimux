// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Fabrice Andy
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { isPermissionGranted, requestPermission, sendNotification } from "@tauri-apps/plugin-notification";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { PaneManager, type LayoutNode } from "./panes";

// --- Icônes SVG (monochrome, héritent de la couleur du texte) --------------
const SVG = {
  terminal: '<path d="M4 6l5 6-5 6"/><path d="M12 18h8"/>',
  folder: '<path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v7a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>',
  branch: '<circle cx="6" cy="6" r="2.2"/><circle cx="6" cy="18" r="2.2"/><circle cx="18" cy="8" r="2.2"/><path d="M6 8.2v7.6"/><path d="M18 10.2c0 4-4 3.4-6 5.6"/>',
  splitCols: '<rect x="3.5" y="4.5" width="7" height="15" rx="1.2"/><rect x="13.5" y="4.5" width="7" height="15" rx="1.2"/>',
  splitRows: '<rect x="4.5" y="3.5" width="15" height="7" rx="1.2"/><rect x="4.5" y="13.5" width="15" height="7" rx="1.2"/>',
  closePane: '<path d="M6 6l12 12"/><path d="M18 6L6 18"/>',
  panel: '<rect x="3.5" y="4.5" width="17" height="15" rx="2"/><path d="M9 4.5v15"/>',
  pin: '<path d="M9 3h6l-1 6 3 3v2h-4v5l-1 2-1-2v-5H6v-2l3-3z"/>',
  bell: '<path d="M6 9a6 6 0 0 1 12 0c0 4 1.5 5.5 2 6H4c0.5-0.5 2-2 2-6"/><path d="M10 20a2 2 0 0 0 4 0"/>',
};

function icon(shape: string, cls = "ic"): HTMLElement {
  const s = document.createElement("span");
  s.className = cls;
  s.innerHTML =
    `<svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" ` +
    `stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">${shape}</svg>`;
  return s;
}

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
  onOpenWeb: () => {
    const url = prompt("URL à ouvrir :", "http://localhost:5173/");
    if (!url) return;
    invoke("open_web_pane", { url, dir: "LeftRight" }).catch((e) =>
      console.error("open_web_pane:", e),
    );
  },
  onWebNavigate: (paneId, url) => {
    invoke("web_navigate", { paneId, url }).catch((e) => console.error("web_navigate:", e));
  },
  onWebBack: (paneId) => {
    invoke("web_back", { paneId }).catch((e) => console.error("web_back:", e));
  },
  onWebForward: (paneId) => {
    invoke("web_forward", { paneId }).catch((e) => console.error("web_forward:", e));
  },
});

let activeSession: string | null = null;
// Volet actif courant (payload[1] de window-layout) : cible des contrôles de
// split/fermeture de la barre d'onglets.
let activePaneId: number | null = null;

// Disposition + flux serveur -> volets.
// --- Repli clavier : ne jamais perdre une frappe -----------------------------
// Le rail et les onglets sont de simples `div` (non focalisables) : cliquer
// dessus fait tomber le focus du document sur `<body>`. Or une bascule de
// session ne focalise le terminal du nouveau volet qu'au RETOUR du serveur
// (aller-retour de ~300 à 450 ms, mesuré). Tout ce qui était tapé dans cet
// intervalle partait dans le vide — avec le bip Windows d'une touche envoyée à
// un élément non éditable. C'est le bug « la 1re touche ne s'affiche pas, je
// dois réappuyer ».
//
// Règle : dans une application terminal, une frappe alors qu'aucun champ n'a le
// focus appartient au terminal actif. Si le volet cible n'existe pas encore
// (bascule en cours), on la met en attente et on la rejoue dès son arrivée.
let switchingSince = 0;
let pendingInput: string[] = [];
/// Vrai tant que `attach_session` n'a pas répondu. Une disposition reçue
/// pendant ce laps est encore celle de l'ANCIENNE session : la prendre pour
/// fin de bascule enverrait la frappe en attente au volet qu'on vient de
/// quitter (observé en test). On n'accepte donc que la disposition d'après.
let attacheEnCours = false;

function basculeEnCours(): boolean {
  return switchingSince !== 0 && Date.now() - switchingSince < 3000;
}

function envoyerAuVolet(paneId: number, texte: string) {
  const bytes = Array.from(new TextEncoder().encode(texte));
  invoke("pane_input", { paneId, bytes }).catch(() => {});
}

/// Ouvre la fenêtre de mise en attente. À appeler dès le CLIC qui va changer de
/// volet — pas seulement quand la bascule part : le rail diffère `switchTo` de
/// 200 ms (désambiguïsation du double-clic de renommage) alors que le focus,
/// lui, tombe sur `<body>` immédiatement.
/// `attendAttache` : vrai pour un changement de SESSION (il faut ignorer les
/// dispositions tant que `attach_session` n'a pas répondu — ce sont encore
/// celles de l'ancienne session) ; faux pour un changement d'ONGLET, dont la
/// prochaine disposition reçue est déjà la bonne.
function armerBascule(attendAttache = true) {
  switchingSince = Date.now();
  attacheEnCours = attendAttache;
}

/// La bascule n'aura finalement pas lieu (double-clic de renommage) : ce qui a
/// été tapé revient au volet actif plutôt que d'être jeté.
function annulerBascule() {
  if (switchingSince === 0) return;
  switchingSince = 0;
  attacheEnCours = false;
  const texte = pendingInput.join("");
  pendingInput = [];
  if (texte !== "" && activePaneId !== null) envoyerAuVolet(activePaneId, texte);
}

/// Rejoue les frappes mises en attente pendant une bascule, sur le volet
/// désormais actif. Appelé à l'arrivée de la disposition.
function flushPendingInput(paneId: number) {
  if (switchingSince === 0 || attacheEnCours) return;
  switchingSince = 0;
  if (pendingInput.length === 0) return;
  const texte = pendingInput.join("");
  pendingInput = [];
  envoyerAuVolet(paneId, texte);
  paneManager.focus(paneId);
}

/// Traduit une touche en octets à envoyer au PTY. `null` = touche non
/// concernée (on la laisse au reste de l'application : Échap, F1, etc.).
function toucheVersTexte(ev: KeyboardEvent): string | null {
  if (ev.ctrlKey || ev.altKey || ev.metaKey) return null;
  if (ev.key.length === 1) return ev.key;
  if (ev.key === "Enter") return "\r";
  if (ev.key === "Backspace") return "\x7f";
  if (ev.key === "Tab") return "\t";
  return null;
}

document.addEventListener(
  "keydown",
  (ev) => {
    // Un terminal, un champ de saisie ou un bouton a le focus : rien à faire.
    if (document.activeElement !== document.body) return;
    const texte = toucheVersTexte(ev);
    if (texte === null) return;
    // Consommer la touche ici évite aussi le bip système (WebView2 signale
    // sinon une frappe non traitée).
    ev.preventDefault();
    if (basculeEnCours()) {
      pendingInput.push(texte);
      return;
    }
    // Bascule armée mais jamais conclue (serveur muet) : on rend ce qui est en
    // attente au volet actif plutôt que de le laisser en suspens.
    if (switchingSince !== 0) annulerBascule();
    // Hors bascule (clic sur une zone inerte de l'interface) : le volet actif
    // existe déjà, on lui rend le focus et on lui livre la frappe.
    if (activePaneId !== null) {
      envoyerAuVolet(activePaneId, texte);
      paneManager.focus(activePaneId);
    }
  },
  true,
);

listen<[LayoutNode, number]>("window-layout", (e) => {
  activePaneId = e.payload[1];
  paneManager.renderLayout(e.payload[0], e.payload[1]);
  flushPendingInput(e.payload[1]);
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
// Fix 2 : la connexion GUI persistante vient de mourir côté pont Tauri
// (redémarrage du daemon — geste routinier sur ce projet). Le pont a déjà
// remis sa connexion à `None` (voir `attach_session`, wimux-gui/src-tauri/
// src/lib.rs) ; il ne reste qu'à redemander un attachement complet pour que
// le prochain appel Tauri reconnecte au lieu de rester inerte pour toujours.
listen("server-disconnected", () => {
  console.warn("connexion serveur perdue, tentative de reconnexion...");
  reattachActive();
});

// --- W2 : barre d'onglets (fenêtres de la session GUI-attachée) -------------
type WindowInfo = { name: string | null; cwd: string | null };

// Dernier segment d'un chemin (basename), façon CMUX pour nommer un onglet.
function baseName(p: string | null): string | null {
  if (!p) return null;
  const parts = p.replace(/[\\/]+$/, "").split(/[\\/]/);
  const last = parts[parts.length - 1];
  return last || null;
}

const tabsEl = document.getElementById("tabs")!;
// Dernière fenêtre active connue : une bascule (changement d'`active`) impose un
// `reset()` du PaneManager avant le prochain window-layout (pane_id globaux, les
// snapshots frais repeignent le contenu).
let lastActiveWindow = -1;
let lastWindows: WindowInfo[] = [];
let editingTab = false; // suspend le rafraîchissement des onglets pendant un renommage

listen<[WindowInfo[], number]>("window-list", (e) => {
  if (editingTab) return; // ne pas écraser l'input de renommage d'onglet en cours
  const [windows, active] = e.payload;
  if (active !== lastActiveWindow) {
    paneManager.reset();
  }
  lastActiveWindow = active;
  lastWindows = windows;
  renderTabs(windows, active);
});

// Repli du panneau des workspaces (bouton en haut du rail).
const railCollapseBtn = document.getElementById("rail-collapse")!;
railCollapseBtn.appendChild(icon(SVG.panel));
railCollapseBtn.onclick = () => {
  document.getElementById("app")!.classList.toggle("rail-collapsed");
};

// Contrôles de fenêtre (barre de titre custom, fenêtre sans décoration).
const appWindow = getCurrentWindow();
document.getElementById("win-min")!.onclick = () => appWindow.minimize();
document.getElementById("win-max")!.onclick = () => appWindow.toggleMaximize();
document.getElementById("win-close")!.onclick = () => appWindow.close();

function renderTabs(windows: WindowInfo[], active: number) {
  tabsEl.innerHTML = "";
  windows.forEach((win, i) => {
    const tab = document.createElement("div");
    tab.className = "tab" + (i === active ? " active" : "");
    tab.appendChild(icon(SVG.terminal, "tab-ic"));
    const label = document.createElement("span");
    label.className = "tab-label";
    label.textContent = win.name ?? baseName(win.cwd) ?? String(i + 1);
    tab.appendChild(label);
    let clickTimer: number | null = null;
    label.ondblclick = (ev) => {
      ev.stopPropagation();
      if (clickTimer !== null) { clearTimeout(clickTimer); clickTimer = null; }
      annulerBascule(); // la bascule armée au 1er clic n'aura pas lieu
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
      // Changer d'onglet reconstruit les volets : même trou de focus que le
      // rail, on met donc les frappes en attente dès le clic.
      if (i !== active) armerBascule(false);
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

  // Contrôles de volet TOUJOURS visibles (à droite), agissant sur le volet actif.
  const controls = document.createElement("div");
  controls.className = "tab-controls";
  controls.append(
    ctrlButton(SVG.splitCols, "Découper gauche/droite", () => {
      if (activePaneId !== null) invoke("split_pane", { paneId: activePaneId, dir: "LeftRight" }).catch(() => {});
    }),
    ctrlButton(SVG.splitRows, "Découper haut/bas", () => {
      if (activePaneId !== null) invoke("split_pane", { paneId: activePaneId, dir: "TopBottom" }).catch(() => {});
    }),
    ctrlButton(SVG.closePane, "Fermer le volet actif", () => {
      if (activePaneId !== null) invoke("close_pane", { paneId: activePaneId }).catch(() => {});
    }),
  );
  tabsEl.appendChild(controls);

  updateWsHeader();
}

function ctrlButton(shape: string, title: string, onClick: () => void): HTMLElement {
  const b = document.createElement("button");
  b.className = "ctrl-btn";
  b.title = title;
  b.appendChild(icon(shape));
  b.onclick = (ev) => { ev.stopPropagation(); onClick(); };
  return b;
}

function updateWsHeader() {
  const h = document.getElementById("ws-breadcrumb")!;
  h.replaceChildren();
  if (!activeSession) return;
  h.append(icon(SVG.folder, "ws-ic"));
  const dto = lastSessions.find((s) => s.name === activeSession);
  const n = document.createElement("span");
  n.textContent = dto ? sessionTitle(dto) : activeSession;
  h.appendChild(n);
}

function startTabRename(tab: HTMLElement, index: number, oldName: string) {
  editingTab = true;
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
    editingTab = false;
    const name = input.value.trim();
    // Nom vide => le serveur remet le nom à None (affiche le répertoire ou la position).
    invoke("rename_window", { index, name }).catch(() => {});
  };
  input.onkeydown = (ev) => {
    if (ev.key === "Enter") commit();
    else if (ev.key === "Escape") {
      committed = true;
      editingTab = false;
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
  cwd: string | null;
  branch: string | null;
  color: string | null;
  pinned: boolean;
  layout_rev: number;
};

type AgentTemplateDto = { name: string };

async function switchTo(name: string) {
  if (name === activeSession) return;
  // Les volets de la nouvelle session n'existeront qu'au retour du serveur :
  // les frappes de cet intervalle sont mises en attente (cf. repli clavier).
  // `pendingInput` n'est PAS vidé : les frappes déjà mises en attente depuis le
  // clic (cf. `armerBascule`) appartiennent à la session qu'on rejoint.
  armerBascule();
  activeSession = name;
  lastLayoutRev = -1; // nouvelle session active : on resondera son layout_rev au prochain refresh (A1.5)
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
  attacheEnCours = false; // les dispositions suivantes sont celles de `name`
  renderRail(lastSessions);
  updateWsHeader();
}

// Réattache la session active (réutilisé quand un volet a été créé/fermé hors GUI,
// ex. `wimux agent spawn` : le serveur ne pousse pas WindowLayout à cette
// connexion, on redemande donc un attachement complet). (A1.5)
async function reattachActive() {
  if (!activeSession) return;
  paneManager.reset();
  lastActiveWindow = -1;
  await invoke("attach_session", { session: activeSession }).catch((e) =>
    console.error("reattach:", e),
  );
}

let lastSessions: SessionDto[] = [];
let lastLayoutRev = -1; // révision de layout vue pour la session active (A1.5)
let renaming = false; // suspend le sondage tant qu'une edition de nom est en cours
let draggedName: string | null = null; // session en cours de glisser-déposer (réordonnancement)

// Réordonne les sessions du rail (glisser-déposer) : place `dragged` avant/après
// `target`, met à jour l'affichage de façon optimiste, puis persiste côté serveur.
function reorderSessions(dragged: string, target: string, before: boolean) {
  const order = lastSessions.map((s) => s.name).filter((n) => n !== dragged);
  const ti = order.indexOf(target);
  if (ti < 0) return;
  order.splice(before ? ti : ti + 1, 0, dragged);
  const byName = new Map(lastSessions.map((s) => [s.name, s] as const));
  const reordered = order.map((n) => byName.get(n)).filter((s): s is SessionDto => !!s);
  renderRail(reordered); // optimiste
  invoke("reorder_sessions", { names: order }).catch(() => {});
}

// Déplace une session dans le rail (menu contextuel : monter/descendre/en haut).
function moveSession(name: string, target: "up" | "down" | "top") {
  const names = lastSessions.map((s) => s.name);
  const i = names.indexOf(name);
  if (i < 0) return;
  names.splice(i, 1);
  const j = target === "top" ? 0 : target === "up" ? Math.max(0, i - 1) : Math.min(names.length, i + 1);
  names.splice(j, 0, name);
  const byName = new Map(lastSessions.map((s) => [s.name, s] as const));
  const reordered = names.map((n) => byName.get(n)).filter((s): s is SessionDto => !!s);
  renderRail(reordered);
  invoke("reorder_sessions", { names }).catch(() => {});
}

// Palette de couleurs de workspace (nom -> hex), façon CMUX.
const WORKSPACE_COLORS: Array<[string, string]> = [
  ["Rouge", "#e2504a"], ["Cramoisi", "#d0355b"], ["Orange", "#e8833a"], ["Ambre", "#e0a92e"],
  ["Olive", "#a0a52a"], ["Vert", "#4fa64f"], ["Sarcelle", "#2bb0a0"], ["Aqua", "#33b5c9"],
  ["Bleu", "#3d7ee0"], ["Marine", "#3a5bbf"], ["Indigo", "#6a5acd"], ["Violet", "#9b59b6"],
  ["Magenta", "#c0399b"], ["Rose", "#d6538a"], ["Brun", "#a0703a"], ["Anthracite", "#7a7a7a"],
];

// Ferme une liste de workspaces puis rafraîchit.
async function closeSessions(names: string[]) {
  for (const n of names) await invoke("kill_session", { name: n }).catch(() => {});
  await refresh();
}

// Applique une couleur de façon optimiste (avant la confirmation serveur).
function optimisticColor(name: string, color: string | null) {
  const s = lastSessions.find((x) => x.name === name);
  if (s) s.color = color;
  renderRail(lastSessions);
}

// --- Menu contextuel (clic droit) sur un workspace -------------------------
let contextMenuEl: HTMLElement | null = null;

function closeContextMenu() {
  contextMenuEl?.remove();
  contextMenuEl = null;
  document.querySelectorAll(".context-submenu").forEach((n) => n.remove());
}

function menuRow(menu: HTMLElement, label: string, run: () => void, opts: { danger?: boolean; disabled?: boolean } = {}) {
  const row = document.createElement("div");
  row.className = "context-item" + (opts.danger ? " danger" : "") + (opts.disabled ? " disabled" : "");
  row.textContent = label;
  if (!opts.disabled) row.onclick = () => { closeContextMenu(); run(); };
  menu.appendChild(row);
}

function menuSep(menu: HTMLElement) {
  const sep = document.createElement("div");
  sep.className = "context-sep";
  menu.appendChild(sep);
}

// Sous-menu couleur (palette + « Défaut »), ouvert au survol de la ligne « Couleur ».
function attachColorSubmenu(menu: HTMLElement, s: SessionDto) {
  const row = document.createElement("div");
  row.className = "context-item has-submenu";
  row.textContent = "Couleur du workspace";
  const arrow = document.createElement("span");
  arrow.className = "submenu-arrow";
  arrow.textContent = "›";
  row.appendChild(arrow);
  let sub: HTMLElement | null = null;
  const openSub = () => {
    if (sub) return;
    sub = document.createElement("div");
    sub.className = "context-menu context-submenu";
    const def = document.createElement("div");
    def.className = "context-item";
    def.textContent = "Défaut (aucune)";
    def.onclick = () => { closeContextMenu(); optimisticColor(s.name, null); invoke("set_session_color", { name: s.name, color: null }).catch(() => {}); };
    sub.appendChild(def);
    const custom = document.createElement("div");
    custom.className = "context-item";
    custom.textContent = "Couleur personnalisée…";
    custom.onclick = (ev) => {
      ev.stopPropagation();
      const input = document.createElement("input");
      input.type = "color";
      input.value = s.color ?? "#3d7ee0";
      input.style.cssText = "position:fixed;left:-9999px;";
      document.body.appendChild(input);
      input.onchange = () => {
        const hex = input.value;
        input.remove();
        closeContextMenu();
        optimisticColor(s.name, hex);
        invoke("set_session_color", { name: s.name, color: hex }).catch(() => {});
      };
      input.click();
    };
    sub.appendChild(custom);
    for (const [label, hex] of WORKSPACE_COLORS) {
      const r = document.createElement("div");
      r.className = "context-item color-item";
      const dot = document.createElement("span");
      dot.className = "color-dot";
      dot.style.background = hex;
      r.append(dot, document.createTextNode(label));
      r.onclick = () => { closeContextMenu(); optimisticColor(s.name, hex); invoke("set_session_color", { name: s.name, color: hex }).catch(() => {}); };
      sub!.appendChild(r);
    }
    document.body.appendChild(sub);
    const rr = row.getBoundingClientRect();
    let x = rr.right - 2;
    if (x + sub.offsetWidth > window.innerWidth) x = rr.left - sub.offsetWidth + 2;
    let y = rr.top;
    if (y + sub.offsetHeight > window.innerHeight) y = window.innerHeight - sub.offsetHeight - 4;
    sub.style.left = Math.max(4, x) + "px";
    sub.style.top = Math.max(4, y) + "px";
  };
  row.addEventListener("mouseenter", openSub);
  menu.appendChild(row);
}

function showSessionMenu(ev: MouseEvent, s: SessionDto, el: HTMLElement) {
  closeContextMenu();
  const menu = document.createElement("div");
  menu.className = "context-menu";
  const i = lastSessions.findIndex((x) => x.name === s.name);
  const last = lastSessions.length - 1;

  menuRow(menu, s.pinned ? "Désépingler" : "Épingler le workspace", async () => {
    await invoke("set_session_pinned", { name: s.name, pinned: !s.pinned }).catch(() => {});
    await refresh();
  });
  menuRow(menu, "Renommer…", () => {
    // Récupérer l'élément vivant (le rail a pu être re-rendu depuis l'ouverture
    // du menu ; un `el` détaché laisserait `renaming` coincé).
    const live = document.querySelector<HTMLElement>(`.session[data-name="${CSS.escape(s.name)}"]`) ?? el;
    startRename(live, s.name);
  });
  menuRow(menu, "Réinitialiser le nom", () => {
    // Renomme vers le premier nom numérique libre → réaffiche le répertoire (CMUX).
    const used = new Set(lastSessions.map((x) => x.name));
    let n = 0;
    while (used.has(String(n))) n++;
    const to = String(n);
    invoke("rename_session", { from: s.name, to }).then(() => {
      if (activeSession === s.name) activeSession = to;
      refresh();
    }).catch(() => {});
  }, { disabled: /^\d+$/.test(s.name) });
  attachColorSubmenu(menu, s);
  menuSep(menu);
  menuRow(menu, "Monter", () => moveSession(s.name, "up"), { disabled: i <= 0 });
  menuRow(menu, "Descendre", () => moveSession(s.name, "down"), { disabled: i < 0 || i >= last });
  menuRow(menu, "Placer en haut", () => moveSession(s.name, "top"), { disabled: i <= 0 });
  menuSep(menu);
  menuRow(menu, "Marquer comme lu", () => { invoke("mark_session_read", { name: s.name }).then(() => refresh()).catch(() => {}); }, { disabled: !(s.activity || s.bell) });
  menuRow(menu, "Marquer comme non lu", () => { invoke("mark_session_unread", { name: s.name }).then(() => refresh()).catch(() => {}); }, { disabled: s.activity || s.bell || s.name === activeSession });
  menuSep(menu);
  menuRow(menu, "Fermer le workspace", async () => { await invoke("kill_session", { name: s.name }).catch(() => {}); await refresh(); }, { danger: true });
  menuRow(menu, "Fermer les autres", () => closeSessions(lastSessions.filter((x) => x.name !== s.name).map((x) => x.name)), { danger: true, disabled: lastSessions.length <= 1 });
  menuRow(menu, "Fermer ceux en dessous", () => closeSessions(lastSessions.slice(i + 1).map((x) => x.name)), { danger: true, disabled: i < 0 || i >= last });
  menuRow(menu, "Fermer ceux au-dessus", () => closeSessions(lastSessions.slice(0, i).map((x) => x.name)), { danger: true, disabled: i <= 0 });

  document.body.appendChild(menu);
  const x = Math.min(ev.clientX, window.innerWidth - menu.offsetWidth - 4);
  const y = Math.min(ev.clientY, window.innerHeight - menu.offsetHeight - 4);
  menu.style.left = Math.max(4, x) + "px";
  menu.style.top = Math.max(4, y) + "px";
  contextMenuEl = menu;
}

// Fermer le menu sur clic ailleurs (hors menu ET sous-menu), Échap, ou molette.
document.addEventListener("mousedown", (ev) => {
  if (!contextMenuEl) return;
  const t = ev.target as Node;
  if (contextMenuEl.contains(t)) return;
  if ([...document.querySelectorAll(".context-submenu")].some((n) => n.contains(t))) return;
  closeContextMenu();
});
document.addEventListener("keydown", (ev) => { if (ev.key === "Escape") closeContextMenu(); });
window.addEventListener("wheel", () => closeContextMenu(), { passive: true });

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

// Titre affiché d'une session : si elle porte un nom auto-généré (numérique),
// afficher le nom du répertoire courant (façon CMUX) ; sinon le nom explicite.
// `s.name` reste l'identité (switch/kill/rename).
function sessionTitle(s: SessionDto): string {
  if (s.cwd && /^\d+$/.test(s.name)) return baseName(s.cwd) ?? s.name;
  return s.name;
}

function abbreviateCwd(cwd: string): string {
  // Remplace un préfixe de profil utilisateur par `~` (heuristique).
  let p = cwd.replace(/^[A-Za-z]:\\Users\\[^\\]+/i, "~");
  const MAX = 30;
  if (p.length > MAX) p = "…" + p.slice(p.length - (MAX - 1));
  return p;
}

function renderSession(s: SessionDto): HTMLElement {
  const el = document.createElement("div");
  el.className = "session" + (s.name === activeSession ? " active" : "");
  el.dataset.name = s.name;
  // Couleur d'accent du workspace (bordure gauche) si définie (W5).
  if (s.color) el.style.borderLeftColor = s.color;
  const main = document.createElement("div");
  main.className = "session-main";
  const nameRow = document.createElement("div");
  nameRow.className = "name-row";
  if (s.pinned) nameRow.appendChild(icon(SVG.pin, "pin-ic"));
  const name = document.createElement("span");
  name.className = "name";
  name.textContent = sessionTitle(s);
  nameRow.appendChild(name);
  main.appendChild(nameRow);
  // 2e ligne : cwd abrégé + branche. Masquée si cwd inconnu (cmd.exe, agent…).
  if (s.cwd) {
    const meta = document.createElement("div");
    meta.className = "session-meta";
    meta.appendChild(icon(SVG.folder, "meta-ic"));
    const cwd = document.createElement("span");
    cwd.className = "meta-cwd";
    cwd.textContent = abbreviateCwd(s.cwd);
    cwd.title = s.cwd;
    meta.appendChild(cwd);
    if (s.branch) {
      const br = document.createElement("span");
      br.className = "meta-branch";
      br.appendChild(icon(SVG.branch, "meta-ic"));
      const bn = document.createElement("span");
      bn.textContent = s.branch;
      br.appendChild(bn);
      br.title = s.branch;
      meta.appendChild(br);
    }
    main.appendChild(meta);
  }
  let clickTimer: number | null = null;
  name.ondblclick = (ev) => {
    ev.stopPropagation();
    if (clickTimer !== null) { clearTimeout(clickTimer); clickTimer = null; }
    annulerBascule(); // la bascule armée au 1er clic n'aura pas lieu
    startRename(el, s.name);
  };
  const close = document.createElement("span");
  close.className = "close";
  close.textContent = "×";
  close.onclick = async (ev) => { ev.stopPropagation(); await invoke("kill_session", { name: s.name }).catch(() => {}); await refresh(); };
  el.onclick = () => {
    if (clickTimer !== null) return; // 2e clic d'un double-clic : ignore, laisse ondblclick gerer
    // Armé DÈS le clic : le focus tombe sur `<body>` maintenant, alors que la
    // bascule ne partira que dans 200 ms (et le nouveau volet, que bien après).
    if (s.name !== activeSession) armerBascule();
    clickTimer = window.setTimeout(() => { clickTimer = null; switchTo(s.name); }, 200);
  };
  // Glisser-déposer : réordonner les workspaces dans le rail.
  el.draggable = true;
  el.ondragstart = (ev) => {
    draggedName = s.name;
    el.classList.add("dragging");
    if (ev.dataTransfer) ev.dataTransfer.effectAllowed = "move";
  };
  el.ondragend = () => {
    draggedName = null;
    document
      .querySelectorAll(".session.dragging, .session.drop-before, .session.drop-after")
      .forEach((n) => n.classList.remove("dragging", "drop-before", "drop-after"));
  };
  el.ondragover = (ev) => {
    if (draggedName === null || draggedName === s.name) return;
    ev.preventDefault();
    if (ev.dataTransfer) ev.dataTransfer.dropEffect = "move";
    const r = el.getBoundingClientRect();
    const before = ev.clientY < r.top + r.height / 2;
    el.classList.toggle("drop-before", before);
    el.classList.toggle("drop-after", !before);
  };
  el.ondragleave = () => el.classList.remove("drop-before", "drop-after");
  el.ondrop = (ev) => {
    if (draggedName === null || draggedName === s.name) return;
    ev.preventDefault();
    const dragged = draggedName;
    // Le drop termine le glisser : remettre draggedName à null MAINTENANT.
    // reorderSessions va appeler renderRail qui retire l'élément traîné du DOM,
    // ce qui empêche ondragend de se déclencher (sinon draggedName resterait
    // coincé et bloquerait le sondage refresh()).
    draggedName = null;
    const r = el.getBoundingClientRect();
    const before = ev.clientY < r.top + r.height / 2;
    reorderSessions(dragged, s.name, before);
  };
  el.oncontextmenu = (ev) => {
    ev.preventDefault();
    showSessionMenu(ev, s, el);
  };
  const isActive = s.name === activeSession;
  if (s.agent) {
    // Session agent : glyphe de statut (remplace les pastilles G4).
    const glyph = document.createElement("span");
    glyph.className = "agent-glyph " + agentStatusClass(s.agent_status);
    glyph.textContent = agentStatusGlyph(s.agent_status);
    glyph.title = s.agent_status ?? "agent";
    el.append(main, glyph, close);
  } else if (!isActive && (s.bell || s.activity)) {
    // Cloche prioritaire sur l'activité ; rien pour la session active.
    const dot = document.createElement("span");
    dot.className = "dot " + (s.bell ? "bell" : "activity");
    dot.textContent = s.bell ? "🔔" : "";
    el.append(main, dot, close);
  } else {
    el.append(main, close);
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

// --- Notifications (cloche/BEL + toasts OS) --------------------------------
type Notif = { session: string; title: string | null; body: string; time: number; read: boolean };
let notifications: Notif[] = [];
const prevBell = new Map<string, boolean>();
let osNotifAllowed = false;
let notifPanelOpen = false;

async function initNotifications() {
  try {
    osNotifAllowed = await isPermissionGranted();
    if (!osNotifAllowed) osNotifAllowed = (await requestPermission()) === "granted";
  } catch { osNotifAllowed = false; }
}

function sessionDisplayName(name: string): string {
  const s = lastSessions.find((x) => x.name === name);
  return s ? sessionTitle(s) : name;
}

function unreadCount(): number {
  return notifications.reduce((n, x) => n + (x.read ? 0 : 1), 0);
}

function updateNotifBadge() {
  const bell = document.getElementById("notif-bell");
  if (!bell) return;
  let badge = bell.querySelector<HTMLElement>(".notif-badge");
  const n = unreadCount();
  if (n > 0) {
    if (!badge) { badge = document.createElement("span"); badge.className = "notif-badge"; bell.appendChild(badge); }
    badge.textContent = n > 9 ? "9+" : String(n);
  } else badge?.remove();
}

function addNotification(session: string, title: string | null, body: string) {
  notifications.unshift({ session, title, body, time: Date.now(), read: notifPanelOpen });
  if (notifications.length > 50) notifications.length = 50;
  updateNotifBadge();
  if (notifPanelOpen) renderNotifPanel();
  if (osNotifAllowed) {
    try { sendNotification({ title: title ?? `wimux — ${sessionDisplayName(session)}`, body }); } catch { /* ignore */ }
  }
}

// Détecte les nouvelles cloches (BEL) sur les sessions en arrière-plan (front, W6).
function detectBellNotifications(sessions: SessionDto[]) {
  const live = new Set<string>();
  for (const s of sessions) {
    live.add(s.name);
    const was = prevBell.get(s.name) ?? false;
    if (s.bell && !was && s.name !== activeSession) addNotification(s.name, null, "Cloche (BEL)");
    prevBell.set(s.name, s.bell);
  }
  for (const k of [...prevBell.keys()]) if (!live.has(k)) prevBell.delete(k);
}

function closeNotifPanel() {
  notifPanelOpen = false;
  document.getElementById("notif-panel")?.remove();
}

function renderNotifPanel() {
  document.getElementById("notif-panel")?.remove();
  const panel = document.createElement("div");
  panel.id = "notif-panel";
  const head = document.createElement("div");
  head.className = "notif-head";
  const title = document.createElement("span");
  title.textContent = "Notifications";
  const clear = document.createElement("span");
  clear.className = "notif-clear";
  clear.textContent = "Tout effacer";
  clear.onclick = (ev) => { ev.stopPropagation(); notifications = []; updateNotifBadge(); renderNotifPanel(); };
  head.append(title, clear);
  panel.appendChild(head);
  if (notifications.length === 0) {
    const empty = document.createElement("div");
    empty.className = "notif-empty";
    empty.textContent = "Aucune notification";
    panel.appendChild(empty);
  } else {
    for (const n of notifications) {
      const row = document.createElement("div");
      row.className = "notif-item";
      const t = document.createElement("div");
      t.className = "notif-item-title";
      t.textContent = (n.title ? n.title + " — " : "") + sessionDisplayName(n.session);
      const b = document.createElement("div");
      b.className = "notif-item-body";
      b.textContent = n.body;
      row.append(t, b);
      row.onclick = () => { closeNotifPanel(); switchTo(n.session); };
      panel.appendChild(row);
    }
  }
  document.body.appendChild(panel);
  const bell = document.getElementById("notif-bell")!;
  const r = bell.getBoundingClientRect();
  panel.style.left = Math.max(4, r.left) + "px";
  panel.style.top = r.bottom + 4 + "px";
}

function toggleNotifPanel() {
  if (notifPanelOpen) { closeNotifPanel(); return; }
  notifPanelOpen = true;
  notifications.forEach((n) => (n.read = true));
  updateNotifBadge();
  renderNotifPanel();
}

const notifBellBtn = document.getElementById("notif-bell")!;
notifBellBtn.appendChild(icon(SVG.bell));
notifBellBtn.onclick = (ev) => { ev.stopPropagation(); toggleNotifPanel(); };
document.addEventListener("mousedown", (ev) => {
  if (!notifPanelOpen) return;
  const t = ev.target as Node;
  if (document.getElementById("notif-panel")?.contains(t) || notifBellBtn.contains(t)) return;
  closeNotifPanel();
});
initNotifications();

async function refresh() {
  if (renaming || draggedName !== null) return; // pas de rebuild pendant renommage/glisser
  try {
    const sessions = await invoke<SessionDto[]>("list_sessions");
    renderRail(sessions); // peuple le rail + lastSessions d'abord
    detectBellNotifications(sessions); // notifie sur cloche d'une session en arrière-plan
    // La session active a disparu (fermée) : nettoyer la vue (volets + onglets + en-tête).
    if (activeSession && !sessions.some((s) => s.name === activeSession)) {
      activeSession = null;
      paneManager.reset();
      tabsEl.innerHTML = "";
      lastActiveWindow = -1;
      lastWindows = [];
      updateWsHeader();
    }
    // A1.5 : un volet a-t-il été créé/fermé hors GUI ? (layout_rev de la session
    // active a changé) → réattachement pour refléter la nouvelle topologie.
    if (activeSession) {
      const active = sessions.find((s) => s.name === activeSession);
      if (active) {
        if (lastLayoutRev === -1) {
          lastLayoutRev = active.layout_rev;
        } else if (active.layout_rev !== lastLayoutRev) {
          lastLayoutRev = active.layout_rev;
          await reattachActive();
        }
      }
    }
    // Auto-sélection : si aucune session active mais il en existe, prendre la première.
    if (!activeSession && sessions.length > 0) { await switchTo(sessions[0].name); }
    // Rafraîchit aussi les onglets (leurs cwd/libellés) : le serveur répond par un
    // window-list sur la connexion persistante. Suspendu pendant une édition d'onglet.
    if (activeSession && !editingTab) invoke("list_windows").catch(() => {});
    // Notifications OSC 9/777 en attente côté serveur (drainées) (W6).
    invoke<Array<{ session: string; title: string | null; body: string }>>("take_notifications")
      .then((ns) => ns.forEach((n) => addNotification(n.session, n.title, n.body)))
      .catch(() => {});
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
