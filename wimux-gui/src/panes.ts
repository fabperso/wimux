import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";

/// Arbre de disposition, miroir du `LayoutNode` serveur (serde externally-tagged).
export type LayoutNode =
  | { Leaf: { pane_id: number } }
  | {
      Split: {
        node_id: number;
        dir: "LeftRight" | "TopBottom";
        ratio: number;
        a: LayoutNode;
        b: LayoutNode;
      };
    };

export interface PaneCallbacks {
  onInput: (paneId: number, bytes: number[]) => void;
  onResize: (paneId: number, cols: number, rows: number) => void;
  onFocus: (paneId: number) => void;
  onSplit: (paneId: number, dir: "LeftRight" | "TopBottom") => void;
  onClose: (paneId: number) => void;
  onRatio: (nodeId: number, ratio: number) => void;
}

interface PaneView {
  term: Terminal;
  fit: FitAddon;
  el: HTMLElement;
  observer: ResizeObserver;
}

export class PaneManager {
  private views = new Map<number, PaneView>();
  private mount: HTMLElement;
  private cb: PaneCallbacks;
  private ratioTimer: number | null = null;
  private pendingRatio: { nodeId: number; ratio: number } | null = null;
  // Focus clavier : mémorise le dernier volet actif pour ne poser le focus
  // que lors d'un vrai changement (pas à chaque redraw / drag de ratio).
  private lastActive: number | null = null;
  // Anti-spam pane_resize : dernière taille (cols,rows) déjà notifiée au serveur par volet.
  private lastSizes = new Map<number, { cols: number; rows: number }>();
  // Anti-rebuild : signature structurelle (sans ratio) + ensemble des pane_id
  // du dernier rendu, pour détecter une topologie inchangée (ex. FocusPane).
  private lastSignature: string | null = null;
  private lastPaneIds: Set<number> | null = null;
  // Élements .split-child (a/b) de chaque noeud de split, indexés par node_id,
  // pour pouvoir mettre à jour flexGrow sans reconstruire le DOM.
  private splitChildren = new Map<number, { a: HTMLElement; b: HTMLElement }>();

  constructor(mount: HTMLElement, cb: PaneCallbacks) {
    this.mount = mount;
    this.cb = cb;
  }

  private emitResize(paneId: number, cols: number, rows: number) {
    const prev = this.lastSizes.get(paneId);
    if (prev && prev.cols === cols && prev.rows === rows) return;
    this.lastSizes.set(paneId, { cols, rows });
    this.cb.onResize(paneId, cols, rows);
  }

  private emitRatio(nodeId: number, ratio: number) {
    this.pendingRatio = { nodeId, ratio };
    if (this.ratioTimer !== null) return;
    this.ratioTimer = window.setTimeout(() => {
      this.ratioTimer = null;
      if (this.pendingRatio) {
        this.cb.onRatio(this.pendingRatio.nodeId, this.pendingRatio.ratio);
        this.pendingRatio = null;
      }
    }, 50);
  }

  write(paneId: number, data: Uint8Array) {
    const v = this.views.get(paneId);
    if (!v) return;
    v.term.write(data);
    // Anneau de notification (W6) : BEL (0x07) reçu -> le volet clignote.
    if (data.includes(0x07)) {
      v.el.classList.add("pane-ring");
      window.setTimeout(() => v.el.classList.remove("pane-ring"), 1500);
    }
  }

  reset() {
    for (const v of this.views.values()) {
      v.observer.disconnect();
      v.term.dispose();
    }
    this.views.clear();
    this.mount.replaceChildren();
    this.lastSizes.clear();
    this.lastSignature = null;
    this.lastPaneIds = null;
    this.lastActive = null;
    this.splitChildren.clear();
  }

  renderLayout(tree: LayoutNode, active: number) {
    const wanted = new Set<number>();
    this.collectIds(tree, wanted);
    // Disposer les volets disparus.
    for (const [id, v] of this.views) {
      if (!wanted.has(id)) {
        v.observer.disconnect();
        v.term.dispose();
        this.views.delete(id);
        this.lastSizes.delete(id);
      }
    }

    const signature = this.computeSignature(tree);
    const idsUnchanged =
      this.lastPaneIds !== null &&
      this.lastPaneIds.size === wanted.size &&
      [...wanted].every((id) => this.lastPaneIds!.has(id));
    const structureUnchanged = idsUnchanged && this.lastSignature === signature;

    if (structureUnchanged) {
      // Topologie (et ensemble de pane_id) identique à celle du dernier rendu :
      // pas de replaceChildren, on se contente de refléter un ratio éventuel
      // via flexGrow sur les .split-child déjà en place.
      this.updateRatios(tree);
    } else {
      // Reconstruire les conteneurs .split ; seules les feuilles .pane
      // existantes (récupérées via ensureView) sont réutilisées.
      this.splitChildren.clear();
      const root = this.buildNode(tree);
      this.mount.replaceChildren(root);
      // Réajuster les tailles après reparentage.
      for (const [id, v] of this.views) {
        try {
          v.fit.fit();
          this.emitResize(id, v.term.cols, v.term.rows);
        } catch {
          /* conteneur non mesurable (détaché) : ignoré */
        }
      }
    }
    this.lastSignature = signature;
    this.lastPaneIds = wanted;

    // Marquer le volet actif.
    for (const [id, v] of this.views) {
      v.el.classList.toggle("active", id === active);
    }
    // Focus clavier uniquement si le volet actif a changé depuis le dernier
    // rendu (évite de voler le focus pendant un drag de ratio ou un simple
    // redraw sans changement de volet actif).
    if (active !== this.lastActive) {
      this.views.get(active)?.term.focus();
      this.lastActive = active;
    }
  }

  private collectIds(tree: LayoutNode, into: Set<number>) {
    if ("Leaf" in tree) into.add(tree.Leaf.pane_id);
    else {
      this.collectIds(tree.Split.a, into);
      this.collectIds(tree.Split.b, into);
    }
  }

  private computeSignature(tree: LayoutNode): string {
    if ("Leaf" in tree) return `L${tree.Leaf.pane_id}`;
    const s = tree.Split;
    return `S${s.node_id}:${s.dir}:(${this.computeSignature(s.a)},${this.computeSignature(s.b)})`;
  }

  private updateRatios(tree: LayoutNode) {
    if ("Leaf" in tree) return;
    const s = tree.Split;
    const children = this.splitChildren.get(s.node_id);
    if (children) {
      children.a.style.flexGrow = String(s.ratio);
      children.b.style.flexGrow = String(1 - s.ratio);
    }
    this.updateRatios(s.a);
    this.updateRatios(s.b);
  }

  private ensureView(paneId: number): PaneView {
    const existing = this.views.get(paneId);
    if (existing) return existing;
    const el = document.createElement("div");
    el.className = "pane";
    el.dataset.paneId = String(paneId);
    const term = new Terminal({
      fontFamily: "Cascadia Mono, Consolas, monospace",
      fontSize: 14,
      // Fond = couleur de la sidebar (façon CMUX), pas noir pur.
      theme: { background: "#252526" },
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(el);
    // Anneau de notification (W6) : le volet qui reçoit une cloche (BEL) clignote.
    term.onBell(() => {
      el.classList.add("pane-ring");
      window.setTimeout(() => el.classList.remove("pane-ring"), 1500);
    });
    const bar = document.createElement("div");
    bar.className = "pane-bar";
    const bSplitV = document.createElement("button");
    bSplitV.textContent = "⬍";
    bSplitV.title = "Découper haut/bas";
    bSplitV.onclick = (ev) => {
      ev.stopPropagation();
      this.cb.onSplit(paneId, "TopBottom");
    };
    const bSplitH = document.createElement("button");
    bSplitH.textContent = "⬌";
    bSplitH.title = "Découper gauche/droite";
    bSplitH.onclick = (ev) => {
      ev.stopPropagation();
      this.cb.onSplit(paneId, "LeftRight");
    };
    const bClose = document.createElement("button");
    bClose.textContent = "✕";
    bClose.title = "Fermer le volet";
    bClose.onclick = (ev) => {
      ev.stopPropagation();
      this.cb.onClose(paneId);
    };
    bar.append(bSplitV, bSplitH, bClose);
    el.appendChild(bar);
    el.addEventListener("mousedown", () => {
      this.cb.onFocus(paneId);
      term.focus();
    });
    term.onData((data) => {
      const bytes = Array.from(new TextEncoder().encode(data));
      this.cb.onInput(paneId, bytes);
    });
    const observer = new ResizeObserver(() => {
      try {
        fit.fit();
        this.emitResize(paneId, term.cols, term.rows);
      } catch {
        /* non mesurable : ignoré */
      }
    });
    observer.observe(el);
    const view: PaneView = { term, fit, el, observer };
    this.views.set(paneId, view);
    return view;
  }

  private buildNode(tree: LayoutNode): HTMLElement {
    if ("Leaf" in tree) {
      return this.ensureView(tree.Leaf.pane_id).el;
    }
    const s = tree.Split;
    const container = document.createElement("div");
    container.className = "split " + (s.dir === "LeftRight" ? "split-row" : "split-col");
    const a = document.createElement("div");
    a.className = "split-child";
    a.style.flexGrow = String(s.ratio);
    a.appendChild(this.buildNode(s.a));
    const sep = document.createElement("div");
    sep.className = "separator " + (s.dir === "LeftRight" ? "sep-v" : "sep-h");
    sep.dataset.nodeId = String(s.node_id);
    const b = document.createElement("div");
    b.className = "split-child";
    b.style.flexGrow = String(1 - s.ratio);
    b.appendChild(this.buildNode(s.b));
    sep.addEventListener("mousedown", (ev) => {
      ev.preventDefault();
      const isRow = s.dir === "LeftRight";
      const onMove = (m: MouseEvent) => {
        const rect = container.getBoundingClientRect();
        let ratio = isRow
          ? (m.clientX - rect.left) / rect.width
          : (m.clientY - rect.top) / rect.height;
        ratio = Math.max(0.1, Math.min(0.9, ratio));
        // Mise à jour optimiste locale ; le serveur ré-émettra window-layout.
        a.style.flexGrow = String(ratio);
        b.style.flexGrow = String(1 - ratio);
        this.emitRatio(s.node_id, ratio);
      };
      const onUp = () => {
        window.removeEventListener("mousemove", onMove);
        window.removeEventListener("mouseup", onUp);
      };
      window.addEventListener("mousemove", onMove);
      window.addEventListener("mouseup", onUp);
    });
    container.append(a, sep, b);
    this.splitChildren.set(s.node_id, { a, b });
    return container;
  }
}
