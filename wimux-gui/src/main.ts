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

// Volet actif (G1 : un seul).
let paneId = 0;

// Sortie serveur -> terminal.
listen<[number, number[]]>("pane-snapshot", (e) => {
  paneId = e.payload[0];
  term.write(new Uint8Array(e.payload[1]));
});
listen<[number, number[]]>("pane-output", (e) => {
  paneId = e.payload[0];
  term.write(new Uint8Array(e.payload[1]));
});

// Frappe -> serveur.
term.onData((data) => {
  const bytes = Array.from(new TextEncoder().encode(data));
  invoke("pane_input", { paneId, bytes });
});

// S'attacher a la session "dev" au demarrage (G1 : nom fixe).
invoke("gui_attach", { session: "dev" }).catch((err) => term.write(`\r\n[erreur: ${err}]\r\n`));
