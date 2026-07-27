// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Fabrice Andy
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";

/// Nature d'une feuille (miroir de `PaneKind` serveur, serde externally-tagged).
export type PaneKind = "Terminal" | { Web: { url: string } };

/// Arbre de disposition, miroir du `LayoutNode` serveur (serde externally-tagged).
export type LayoutNode =
  | { Leaf: { pane_id: number; kind: PaneKind } }
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
  onOpenWeb: (paneId: number) => void;
  onWebNavigate: (paneId: number, url: string) => void;
  onWebBack: (paneId: number) => void;
  onWebForward: (paneId: number) => void;
}

/// Miroir client de `url_autorisee` (`crates/wimux-server/src/session.rs`) :
/// seule une URL http(s) qui ne cible PAS l'origine de l'application doit être
/// posée en `src` d'iframe. Une iframe hérite de l'origine du parent pour un
/// schéma `javascript:` — ou, tout aussi bien, pour une URL SAME-ORIGIN avec
/// la GUI elle-même — ce qui donnerait accès à `window.parent.__TAURI__`
/// (`tauri.conf.json` a `withGlobalTauri: true` et `csp: null`). Le serveur
/// refuse déjà ces URL en amont ; ce garde côté frontend est une seconde ligne
/// de défense (défense en profondeur), pas la seule — voir aussi l'attribut
/// `sandbox` posé sur chaque iframe dans `ensureWebView` (troisième ligne).
function urlAutorisee(url: string): boolean {
  const lower = url.toLowerCase();
  if (!(lower.startsWith("http://") || lower.startsWith("https://"))) return false;
  return !cibleOrigineApplication(lower);
}

/// `urlLower` (déjà en minuscules, schéma http(s) déjà vérifié) désigne-t-elle
/// l'origine de l'application Tauri elle-même ? Voir `urlAutorisee` : la GUI
/// est servie sur `http://localhost:1420` en dev (`devUrl`) et sur
/// `http(s)://tauri.localhost` en prod (WebView2/Tauri v2 Windows) — une
/// iframe pointée dessus serait same-origin avec son parent. On bloque donc
/// l'hôte `tauri.localhost` (toute casse), et `localhost`/`127.0.0.1`/`[::1]`
/// SUR LE PORT 1420 précisément — pas un simple préfixe de port
/// (`localhost:1420x` doit rester autorisé), et surtout pas un autre port
/// (`localhost:5173`, le cas d'usage visé, doit rester autorisé).
function cibleOrigineApplication(urlLower: string): boolean {
  const idx = urlLower.indexOf("://");
  if (idx < 0) return false;
  const afterScheme = urlLower.slice(idx + 3);
  const end = afterScheme.search(/[/?#]/);
  let authority = end < 0 ? afterScheme : afterScheme.slice(0, end);
  const at = authority.lastIndexOf("@");
  if (at >= 0) authority = authority.slice(at + 1);
  const { host, port } = splitHostPort(authority);
  if (host === "tauri.localhost") return true;
  return (host === "localhost" || host === "127.0.0.1" || host === "[::1]") && port === "1420";
}

/// Sépare une autorité `hôte[:port]` en `{ host, port }`. Gère l'hôte IPv6
/// entre crochets (`[::1]:1420`), qui contient lui-même des `:`.
function splitHostPort(authority: string): { host: string; port: string | undefined } {
  if (authority.startsWith("[")) {
    const close = authority.indexOf("]");
    if (close >= 0) {
      const host = authority.slice(0, close + 1);
      const rest = authority.slice(close + 1);
      const port = rest.startsWith(":") ? rest.slice(1) : undefined;
      return { host, port };
    }
  }
  const i = authority.lastIndexOf(":");
  if (i < 0) return { host: authority, port: undefined };
  return { host: authority.slice(0, i), port: authority.slice(i + 1) };
}

interface PaneView {
  term: Terminal;
  fit: FitAddon;
  el: HTMLElement;
  observer: ResizeObserver;
}

export class PaneManager {
  private views = new Map<number, PaneView>();
  // Volets NAVIGATEUR (B1) : conteneur + iframe + champ URL, indexés par pane_id.
  private webViews = new Map<number, { el: HTMLElement; frame: HTMLIFrameElement; input: HTMLInputElement }>();
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
  // Navigation explicite demandée (◀ / ▶ / Entrée sur le champ URL) pour ces
  // volets : quand le serveur repousse la même URL (ex. ◀ déjà en tête de
  // pile d'historique), rien ne distingue cette mise à jour d'un simple
  // redraw — ni l'URL (identique à `src`), ni la signature de disposition
  // (identique elle aussi, donc `renderLayout` peut prendre la branche
  // « structure inchangée » sans même appeler `ensureWebView`). On marque
  // ici l'intention pour réaffirmer `src` inconditionnellement après le
  // rendu, quelle que soit la branche empruntée.
  private navDemandee = new Set<number>();

  constructor(mount: HTMLElement, cb: PaneCallbacks) {
    this.mount = mount;
    this.cb = cb;
  }

  // Debounce du resize PTY, par volet. Pendant un split, le layout « churn » :
  // le volet passe par plusieurs tailles intermédiaires avant de se stabiliser.
  // Envoyer CHAQUE taille au PTY fait redessiner Claude à chaque fois, et ses
  // redraws incrémentaux ne nettoient pas — les caractères des tailles
  // précédentes restent dans la grille (corruption visible côté VT ET xterm).
  // On n'informe donc le serveur qu'APRÈS stabilisation (taille finale unique).
  private resizeTimers = new Map<number, number>();
  private emitResize(paneId: number, cols: number, rows: number) {
    const prev = this.lastSizes.get(paneId);
    if (prev && prev.cols === cols && prev.rows === rows) return;
    this.lastSizes.set(paneId, { cols, rows });
    const t = this.resizeTimers.get(paneId);
    if (t !== undefined) clearTimeout(t);
    this.resizeTimers.set(
      paneId,
      window.setTimeout(() => {
        this.resizeTimers.delete(paneId);
        const sz = this.lastSizes.get(paneId);
        if (sz) this.cb.onResize(paneId, sz.cols, sz.rows);
      }, 120),
    );
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

  /// Redonne le focus clavier au terminal d'un volet, s'il existe encore.
  /// Utilisé par le repli clavier de `main.ts` : cliquer une zone non
  /// focalisable de l'interface (rail, onglets — de simples `div`) fait tomber
  /// le focus du document sur `<body>`, où les frappes se perdent.
  focus(paneId: number): boolean {
    const v = this.views.get(paneId);
    if (!v) return false;
    v.term.focus();
    return true;
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
    this.webViews.clear();
    this.mount.replaceChildren();
    for (const t of this.resizeTimers.values()) clearTimeout(t);
    this.resizeTimers.clear();
    this.lastSizes.clear();
    this.lastSignature = null;
    this.lastPaneIds = null;
    this.lastActive = null;
    this.splitChildren.clear();
    this.navDemandee.clear();
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
    // Idem pour les volets navigateur disparus (B1).
    for (const [id, v] of this.webViews) {
      if (!wanted.has(id)) {
        v.el.remove();
        this.webViews.delete(id);
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

    // Marquer le volet actif (terminaux ET navigateurs : le serveur ne fait
    // pas de différence de nature pour désigner le volet actif d'une fenêtre).
    for (const [id, v] of this.views) {
      v.el.classList.toggle("active", id === active);
    }
    for (const [id, v] of this.webViews) {
      v.el.classList.toggle("active", id === active);
    }
    // Focus clavier uniquement si le volet actif a changé depuis le dernier
    // rendu (évite de voler le focus pendant un drag de ratio ou un simple
    // redraw sans changement de volet actif).
    if (active !== this.lastActive) {
      this.views.get(active)?.term.focus();
      this.lastActive = active;
    }

    // Réaffirmation de navigation (◀ / ▶ / champ URL) : indépendamment de la
    // branche empruntée ci-dessus (structure inchangée ou reconstruite), on
    // reposte `src` pour tout volet ayant une intention de navigation en
    // attente, même si l'URL reçue est identique à l'attribut courant. C'est
    // ce qui permet à ◀ de fonctionner quand le serveur, déjà en tête de
    // pile, repousse la même URL que celle affichée après une navigation
    // interne à la page (cf. commentaire sur `navDemandee`).
    if (this.navDemandee.size > 0) {
      for (const paneId of this.navDemandee) {
        const url = this.findWebUrl(tree, paneId);
        const view = this.webViews.get(paneId);
        if (url !== undefined && view && urlAutorisee(url)) {
          view.frame.setAttribute("src", url);
        }
      }
      this.navDemandee.clear();
    }
  }

  /// Retrouve, dans l'arbre de disposition reçu, l'URL du volet navigateur
  /// `paneId` — ou `undefined` si ce volet n'existe pas dans l'arbre ou n'est
  /// pas un volet navigateur.
  private findWebUrl(tree: LayoutNode, paneId: number): string | undefined {
    if ("Leaf" in tree) {
      if (tree.Leaf.pane_id !== paneId) return undefined;
      const k = tree.Leaf.kind;
      return k !== "Terminal" && "Web" in k ? k.Web.url : undefined;
    }
    return this.findWebUrl(tree.Split.a, paneId) ?? this.findWebUrl(tree.Split.b, paneId);
  }

  private collectIds(tree: LayoutNode, into: Set<number>) {
    if ("Leaf" in tree) into.add(tree.Leaf.pane_id);
    else {
      this.collectIds(tree.Split.a, into);
      this.collectIds(tree.Split.b, into);
    }
  }

  private computeSignature(tree: LayoutNode): string {
    if ("Leaf" in tree) {
      // La nature (et l'URL pour un navigateur) fait partie de la signature :
      // sans ça une navigation (même arbre, URL différente) ne redéclencherait
      // aucune mise à jour, l'optimisation anti-rebuild l'ignorant.
      const k = tree.Leaf.kind;
      const kindSig = k === "Terminal" ? "T" : `W:${k.Web.url}`;
      return `L${tree.Leaf.pane_id}:${kindSig}`;
    }
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
    const bWeb = document.createElement("button");
    bWeb.textContent = "🌐";
    bWeb.title = "Ouvrir un navigateur à côté";
    bWeb.onclick = (ev) => {
      ev.stopPropagation();
      this.cb.onOpenWeb(paneId);
    };
    bar.append(bSplitV, bSplitH, bWeb, bClose);
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

  /// Construit (ou réutilise) le conteneur d'un volet NAVIGATEUR : barre de
  /// chrome (URL, précédent, suivant, recharger) + iframe.
  private ensureWebView(paneId: number, url: string) {
    const existing = this.webViews.get(paneId);
    if (existing) {
      // Le serveur est la source de vérité : on suit l'URL reçue. Ne poser
      // `src` que si l'URL passe le même test que côté serveur ; sinon on
      // laisse l'iframe telle quelle plutôt que de planter.
      if (existing.frame.getAttribute("src") !== url && urlAutorisee(url)) {
        existing.frame.setAttribute("src", url);
      }
      if (document.activeElement !== existing.input) existing.input.value = url;
      return existing;
    }
    const el = document.createElement("div");
    el.className = "pane pane-web";
    el.dataset.paneId = String(paneId);

    const bar = document.createElement("div");
    bar.className = "web-bar";
    const back = document.createElement("button");
    back.textContent = "◀";
    back.title = "Précédent";
    back.onclick = () => {
      // Intention explicite : réaffirmer `src` après coup même si le serveur
      // repousse la même URL (cf. commentaire sur `navDemandee`).
      this.navDemandee.add(paneId);
      this.cb.onWebBack(paneId);
    };
    const fwd = document.createElement("button");
    fwd.textContent = "▶";
    fwd.title = "Suivant";
    fwd.onclick = () => {
      this.navDemandee.add(paneId);
      this.cb.onWebForward(paneId);
    };
    const reload = document.createElement("button");
    reload.textContent = "⟳";
    // Ce bouton réassigne `src` à l'URL du volet : après une navigation
    // interne à la page (clic sur un lien, même origine), ça n'est PAS un
    // rechargement de la page courante mais un retour à l'URL connue du
    // serveur — d'où l'infobulle honnête plutôt que « Recharger ».
    reload.title = "Revenir à l'URL du volet";
    const input = document.createElement("input");
    input.className = "web-url";
    input.value = url;
    input.onkeydown = (ev) => {
      if (ev.key === "Enter") {
        this.navDemandee.add(paneId);
        this.cb.onWebNavigate(paneId, input.value.trim());
      }
    };
    const close = document.createElement("button");
    close.textContent = "✕";
    close.title = "Fermer le volet";
    close.onclick = () => this.cb.onClose(paneId);
    bar.append(back, fwd, reload, input, close);

    const frame = document.createElement("iframe");
    frame.className = "web-frame";
    // Défense en profondeur (Fix 1, 3e ligne) : PAS de `allow-same-origin`.
    // Ça neutralise toute la classe d'attaques visée ici — même si une URL
    // d'origine app passait malgré tout les deux gardes précédentes (schéma +
    // origine), le contenu chargé dans ce cadre serait forcé dans une origine
    // opaque distincte et n'atteindrait donc jamais `window.parent.__TAURI__`.
    // La page de test locale utilisée pour vérifier l'affichage n'a besoin ni
    // de cookies ni de `localStorage` : ces attributs suffisent (scripts,
    // formulaires, popups) sans réintroduire l'accès same-origin.
    frame.setAttribute("sandbox", "allow-scripts allow-forms allow-popups");
    // Ne poser `src` que si l'URL passe le garde (cf. urlAutorisee) ; sinon
    // l'iframe reste vide plutôt que de planter.
    if (urlAutorisee(url)) {
      frame.setAttribute("src", url);
    }
    // Purement client : pas d'aller-retour serveur. Revalide quand même l'URL
    // cible (le garde serveur a déjà dû l'accepter en amont, mais on ne fait
    // jamais confiance à un attribut DOM comme unique ligne de défense).
    reload.onclick = () => {
      const target = frame.getAttribute("src") ?? url;
      if (urlAutorisee(target)) {
        frame.setAttribute("src", target);
      }
    };

    // Avertissement permanent : un refus d'affichage en cadre n'est PAS
    // détectable de façon fiable depuis la page hôte, donc on informe au lieu
    // de prétendre diagnostiquer.
    const hint = document.createElement("div");
    hint.className = "web-hint";
    hint.textContent = "Certains sites refusent l'affichage en cadre.";

    el.append(bar, frame, hint);
    el.addEventListener("mousedown", () => this.cb.onFocus(paneId));
    const view = { el, frame, input };
    this.webViews.set(paneId, view);
    return view;
  }

  private buildNode(tree: LayoutNode): HTMLElement {
    if ("Leaf" in tree) {
      const { pane_id, kind } = tree.Leaf;
      if (kind !== "Terminal" && "Web" in kind) {
        return this.ensureWebView(pane_id, kind.Web.url).el;
      }
      return this.ensureView(pane_id).el;
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
