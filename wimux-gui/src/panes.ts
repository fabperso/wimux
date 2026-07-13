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

  constructor(mount: HTMLElement, cb: PaneCallbacks) {
    this.mount = mount;
    this.cb = cb;
  }

  write(paneId: number, data: Uint8Array) {
    const v = this.views.get(paneId);
    if (v) v.term.write(data);
  }

  reset() {
    for (const v of this.views.values()) {
      v.observer.disconnect();
      v.term.dispose();
    }
    this.views.clear();
    this.mount.replaceChildren();
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
      }
    }
    // Reconstruire l'arbre DOM en RÉUTILISANT les wrappers existants.
    const root = this.buildNode(tree);
    this.mount.replaceChildren(root);
    // Marquer le volet actif + réajuster les tailles après reparentage.
    for (const [id, v] of this.views) {
      v.el.classList.toggle("active", id === active);
      try {
        v.fit.fit();
        this.cb.onResize(id, v.term.cols, v.term.rows);
      } catch {
        /* conteneur non mesurable (détaché) : ignoré */
      }
    }
  }

  private collectIds(tree: LayoutNode, into: Set<number>) {
    if ("Leaf" in tree) into.add(tree.Leaf.pane_id);
    else {
      this.collectIds(tree.Split.a, into);
      this.collectIds(tree.Split.b, into);
    }
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
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(el);
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
        this.cb.onResize(paneId, term.cols, term.rows);
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
    container.append(a, sep, b);
    return container;
  }
}
