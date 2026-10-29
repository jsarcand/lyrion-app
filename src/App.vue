<script setup lang="ts">
import { ref, computed } from "vue";
import Wizard from "./Wizard.vue";
import { Command, Child } from "@tauri-apps/api/shell";
import { emit as tauriEmit, listen } from "@tauri-apps/api/event";
import { process } from "@tauri-apps/api";
import { invoke } from "@tauri-apps/api/tauri";
import { getVersion } from "@tauri-apps/api/app";
import { exists, BaseDirectory, createDir, readTextFile, writeTextFile } from "@tauri-apps/api/fs";
import { appConfigDir } from "@tauri-apps/api/path";
import { platform, Platform } from "@tauri-apps/api/os";
import { http } from "@tauri-apps/api";
import { appWindow, LogicalPosition, LogicalSize } from "@tauri-apps/api/window";
import { register, unregister, isRegistered } from "@tauri-apps/api/globalShortcut";

const fileSpecVersion = 1;

const DEFAULT_HOTKEY = "CmdOrControl+Shift+Space";

const hotkeyPresets = [
  { value: "CmdOrControl+Shift+Space", label: "⌘⇧Space  /  Ctrl+Shift+Space" },
  { value: "CmdOrControl+Alt+Space", label: "⌘⌥Space  /  Ctrl+Alt+Space" },
  { value: "CmdOrControl+Shift+M", label: "⌘⇧M  /  Ctrl+Shift+M" },
  { value: "CmdOrControl+Shift+L", label: "⌘⇧L  /  Ctrl+Shift+L" },
  { value: "CmdOrControl+Alt+M", label: "⌘⌥M  /  Ctrl+Alt+M" },
];

/** Select items for LMS install channel (latest / stable nightly / dev) */
const lmsChannelItems = computed(() => {
  const list = state.value.lmsInstall.channels;
  if (list && list.length) {
    return list.map((c) => ({
      value: c.id,
      title:
        c.label +
        (c.version ? ` (${c.version})` : "") +
        (c.recommended ? " — recommended" : ""),
    }));
  }
  return [
    { value: "latest", title: "Latest release — recommended" },
    { value: "stable", title: "Stable nightly (bug fixes)" },
    { value: "dev", title: "Development (may break)" },
  ];
});

const lmsChannelHint = computed(() => {
  const id = state.value.lmsInstall.channel || "latest";
  const c = state.value.lmsInstall.channels?.find((x) => x.id === id);
  if (c?.description) return c.description;
  if (id === "stable") return "Latest release + recent bug fixes (stable branch builds).";
  if (id === "dev") return "Newest features — may be unstable.";
  return "Official stable release — recommended for most users.";
});

/** Pretty-print Tauri accelerators for the settings UI */
function formatHotkey(accel: string): string {
  if (!accel) return "-";
  const mac = /Mac|iPhone|iPad/.test(navigator.platform);
  const parts = accel.split("+").map((p) => p.trim()).filter(Boolean);
  const out = parts.map((p) => {
    const u = p.toLowerCase();
    if (u === "cmdorcontrol" || u === "command" || u === "cmd" || u === "super" || u === "meta") {
      return mac ? "⌘" : "Ctrl";
    }
    if (u === "control" || u === "ctrl") return mac ? "⌃" : "Ctrl";
    if (u === "alt" || u === "option") return mac ? "⌥" : "Alt";
    if (u === "shift") return mac ? "⇧" : "Shift";
    if (u === "space") return "Space";
    if (p.length === 1) return p.toUpperCase();
    return p;
  });
  return mac ? out.join("") : out.join("+");
}

const defaultWindow = {
  width: 440,
  height: 700,
  x: null as number | null,
  y: null as number | null,
  /** Show Dock icon + free-floating window (no auto-hide) */
  showDockIcon: false,
  /** Global show/hide accelerator (Tauri format) */
  globalHotkey: DEFAULT_HOTKEY,
};

// State including config
const state = ref({
  version: '',
  iframeSrc: 'about:blank',
  iframeGen: 0,
  /** Soft error when Material iframe fails to load (timeout / unreachable) */
  iframeError: '' as string,
  iframeLoading: false,
  configMode: true,
  /** First-run / no server: setup wizard. Can be reopened from the menu. */
  wizardMode: true,
  configLoaded: false,
  /** Short mark on returning launches while Material loads */
  bootSplash: false,
  showDockIcon: false,
  hotkeyError: '',
  config: {
    fileSpecVersion: fileSpecVersion,
    client: {
      enabled: true,
      keepOpen: false,
      name: '',
    },
    server: {
      autodetect: true,
      host: '',
      port: 9000,
      defaultHost: '',
      defaultPort: 9000,
    },
    window: { ...defaultWindow },
  },
  client: {
    running: false,
    needsRestart: false,
    squeezeliteProcess: <Child|null>(null),
    /** Second Squeezelite: soft preview + future local viz (hidden from other UIs) */
    previewProcess: <Child|null>(null),
    previewMac: '' as string,
    previewName: '' as string,
  },
  server: {
    loading: false
  },
  thisMachineHosts: [] as string[],
  /** Local LMS install / start (turnkey onboarding) */
  lmsInstall: {
    busy: false,
    /** latest | stable | dev — matches official downloads channels */
    channel: 'latest' as string,
    channels: [] as Array<{
      id: string;
      label: string;
      description: string;
      recommended: boolean;
      version?: string | null;
    }>,
    status: null as null | {
      platform: string;
      appInstalled: boolean;
      appPath?: string | null;
      serverRunning: boolean;
      host: string;
      port: number;
      versionHint?: string | null;
      message: string;
    },
    package: null as null | {
      channel?: string;
      channelLabel?: string;
      version: string;
      url: string;
      filename: string;
      platform: string;
      sizeHintMb?: number | null;
      notes: string;
    },
    progress: '' as string,
    progressPct: null as number | null,
    error: '' as string,
  },
  /** Full-window feedback while this computer's LMS is starting. */
  localBoot: {
    active: false,
    message: '' as string,
    error: '' as string,
  },
});

/** Preview player display name (local-only soft preview / viz clock). */
function previewPlayerName(base: string): string {
  const n = (base || "Lyrion").trim() || "Lyrion";
  return n + " · Preview";
}

/**
 * Deterministic locally-administered MAC so LMS playerid is stable.
 * Returns colon form for LMS hidePlayers/previewPlayer, and dash form for
 * squeezelite -m (some shells/arg parsers mishandle colons in argv).
 */
function previewMacFromName(name: string): { colon: string; dash: string } {
  let h = 2166136261;
  const s = name || "preview";
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  const bytes = [
    0x02,
    0x4c,
    0x4d,
    (h >>> 16) & 0xff,
    (h >>> 8) & 0xff,
    h & 0xff,
  ].map((x) => x.toString(16).padStart(2, "0"));
  return { colon: bytes.join(":"), dash: bytes.join("-") };
}

const hotkeyDisplay = computed(() => formatHotkey(state.value.config.window.globalHotkey || DEFAULT_HOTKEY));

// Quit menu item clicked
listen('quit', async () => {
  await mediaKeyShortcuts.disable();
  await client.stopPreview(true);
  if (!state.value.config.client.keepOpen) {
    await client.stopMain(true);
  }
  await process.exit(0);
});

/** Clear pending iframe load timeout */
let iframeLoadTimer: ReturnType<typeof setTimeout> | null = null;

function clearIframeLoadTimer() {
  if (iframeLoadTimer) {
    clearTimeout(iframeLoadTimer);
    iframeLoadTimer = null;
  }
}

function onPlayerFrameLoad() {
  // about:blank also fires load - ignore
  if (!state.value.iframeSrc || state.value.iframeSrc === "about:blank") return;
  clearIframeLoadTimer();
  state.value.iframeLoading = false;
  state.value.iframeError = '';
  applyShellChrome();
  bindFrameKeys();
  ensureChromeSync();
  ensurePlatineSync();
  console.log("Material iframe loaded:", state.value.iframeSrc);
  // WKWebView RecycleScroller (Accueil) measures window.innerHeight of the
  // iframe. Percentage height + overflow:hidden often reports 0 until a
  // pixel-size nudge after the frame document exists.
  kickPlayerFrameLayout();
  window.setTimeout(kickPlayerFrameLayout, 80);
  window.setTimeout(kickPlayerFrameLayout, 300);
  window.setTimeout(kickPlayerFrameLayout, 900);
}

/** Give the Material iframe a real CSS-pixel box so page-mode lists lay out. */
function kickPlayerFrameLayout() {
  const el = materialFrame();
  if (!el || !state.value.iframeSrc || state.value.iframeSrc === "about:blank") return;
  const w = Math.max(320, Math.round(document.documentElement.clientWidth || window.innerWidth || 0));
  const h = Math.max(480, Math.round(document.documentElement.clientHeight || window.innerHeight || 0));
  el.style.width = w + "px";
  el.style.height = Math.max(1, h - 1) + "px";
  requestAnimationFrame(() => {
    el.style.width = w + "px";
    el.style.height = h + "px";
    syncTitlebarDrag();
    // Same-origin (Vite LMS proxy): fire resize *inside* Material so RecycleScroller measures.
    try {
      const cw = el.contentWindow;
      if (cw) {
        cw.dispatchEvent(new Event("resize"));
        cw.dispatchEvent(new Event("scroll"));
      }
    } catch {
      /* cross-origin */
    }
  });
}

function retryPlayerLoad() {
  state.value.iframeError = '';
  void presentServer();
}

function materialFrame(): HTMLIFrameElement | null {
  return document.querySelector("iframe.player-frame") as HTMLIFrameElement | null;
}

/** Host we can point the LMS proxy at — never a half-typed IPv4. */
function isThisMachineHost(host: string): boolean {
  const h = (host || "").trim().toLowerCase();
  if (!h) return false;
  if (h === "127.0.0.1" || h === "localhost" || h === "::1") return true;
  return state.value.thisMachineHosts.some((ip) => String(ip).toLowerCase() === h);
}

/** Bumped so a newer server choice cancels an in-flight local start. */
let localBootGen = 0;
/** True while ensureLocalServer is awaiting lms_start_local (ready must not reload). */
let localBootWaiter = false;
/** True for the whole ensureLocalServer call, including the "ready" pause. */
let localBootOwned = false;

function delay(ms: number) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function cancelLocalBoot() {
  localBootGen += 1;
  localBootWaiter = false;
  localBootOwned = false;
  state.value.localBoot.active = false;
  state.value.localBoot.error = "";
  state.value.localBoot.message = "";
}

async function probeLms(host: string, port: number): Promise<boolean> {
  try {
    const r = await invoke<{ ok: boolean }>("lms_probe_server", { host, port });
    return !!r?.ok;
  } catch {
    return false;
  }
}

type LocalBootResult = "up" | "started" | "failed";

/**
 * If this computer's LMS is down, start it and keep the overlay up until it
 * answers. Does not load Material — the caller does that on "up" / "started".
 */
async function ensureLocalServer(): Promise<LocalBootResult> {
  const gen = ++localBootGen;
  localBootOwned = true;
  const host = String(state.value.config.server.host || "").trim();
  const port = Number(state.value.config.server.port) || 9000;
  if (!host) {
    if (gen === localBootGen) localBootOwned = false;
    return "failed";
  }

  let revealed = false;
  const reveal = window.setTimeout(() => {
    if (gen !== localBootGen) return;
    revealed = true;
    state.value.localBoot.active = true;
    state.value.localBoot.error = "";
    state.value.localBoot.message = "Vérification du serveur local…";
    state.value.wizardMode = false;
    state.value.configMode = false;
  }, 400);

  const already = await probeLms(host, port);
  window.clearTimeout(reveal);
  if (gen !== localBootGen) return "failed";
  if (already) {
    if (revealed) {
      state.value.localBoot.active = false;
      state.value.localBoot.error = "";
    }
    if (gen === localBootGen) localBootOwned = false;
    return "up";
  }

  state.value.localBoot.active = true;
  state.value.localBoot.error = "";
  state.value.localBoot.message = "Démarrage du serveur local…";
  state.value.wizardMode = false;
  state.value.configMode = false;
  messager.show("Démarrage du serveur local…", 0);

  try {
    const installed = await invoke<{
      appInstalled: boolean;
      serverRunning: boolean;
    }>("lms_install_status").catch(() => null);
    if (gen !== localBootGen) return "failed";
    if (installed?.serverRunning || (await probeLms(host, port))) {
      state.value.localBoot.error = "";
      state.value.localBoot.message = "Serveur prêt";
      messager.show("Serveur prêt", 1800);
      await delay(500);
      if (gen !== localBootGen) return "failed";
      state.value.localBoot.active = false;
      localBootOwned = false;
      return "up";
    }
    if (installed && !installed.appInstalled) {
      state.value.localBoot.error =
        "Lyrion Music Server n'est pas installé. Menu Serveur → Installer Lyrion Music Server…";
      messager.show(state.value.localBoot.error, 6000);
      if (gen === localBootGen) localBootOwned = false;
      return "failed";
    }

    localBootWaiter = true;
    const st = await invoke<{
      serverRunning: boolean;
      host: string;
      port: number;
      message: string;
      appInstalled?: boolean;
    }>("lms_start_local");
    if (gen === localBootGen) localBootWaiter = false;
    if (gen !== localBootGen) return "failed";
    state.value.lmsInstall.status = st as typeof state.value.lmsInstall.status;

    const up = !!st?.serverRunning || (await probeLms(host, port));
    if (gen !== localBootGen) return "failed";
    if (up) {
      state.value.localBoot.error = "";
      state.value.localBoot.message = "Serveur prêt";
      messager.show("Serveur prêt", 2200);
      await delay(650);
      if (gen !== localBootGen) return "failed";
      state.value.localBoot.active = false;
      localBootOwned = false;
      return "started";
    }
    state.value.localBoot.error =
      "Le serveur local ne répond pas. Dans la barre des menus, ouvrez Lyrion Music Server, puis Start Service.";
    messager.show(state.value.localBoot.error, 6000);
    if (gen === localBootGen) localBootOwned = false;
    return "failed";
  } catch (e: any) {
    if (gen === localBootGen) {
      localBootWaiter = false;
      localBootOwned = false;
    }
    if (gen !== localBootGen) return "failed";
    const raw = String(e?.message || e || "");
    state.value.localBoot.error = /not found|install/i.test(raw)
      ? "Lyrion Music Server n'est pas installé. Menu Serveur → Installer Lyrion Music Server…"
      : raw || "Impossible de démarrer le serveur local.";
    messager.show(state.value.localBoot.error, 6000);
    return "failed";
  }
}

/** Load Material. Starts this computer's server first when that is the target. */
async function presentServer(forceLocal = false): Promise<"remote" | LocalBootResult> {
  const host = String(state.value.config.server.host || "").trim();
  const local = forceLocal || (!!host && isThisMachineHost(host));
  if (local) {
    const result = await ensureLocalServer();
    if (result === "failed") return "failed";
    await server.showPlayer();
    return result;
  }
  cancelLocalBoot();
  await server.showPlayer();
  return "remote";
}

async function retryLocalBoot() {
  const result = await ensureLocalServer();
  if (result !== "failed") await server.showPlayer();
}

/** Same-origin Material: a lighter toolbar, without a skin redeploy.
 *  Do not touch --window-area-left: that shifts the player name. */
const SHELL_CHROME_CSS = `
#main-toolbar.lms-toolbar,
.lms-toolbar.v-toolbar,
.lms-toolbar {
  background-color: rgba(50, 50, 52, 0.96) !important;
  background-image: none !important;
}
/* Presets editor shipped without .drag-area-left, so its back button sits
   under the traffic lights. Match the other fullscreen toolbars until the
   skin template (class presets-editor-inset) is what the server serves. */
#presets-editor-toolbar:not(.presets-editor-inset) > .v-toolbar__content::before {
  content: "";
  display: block;
  flex: 0 0 var(--window-area-left, 78px);
  width: var(--window-area-left, 78px);
  min-width: var(--window-area-left, 78px);
  align-self: stretch;
}
#presets-editor-toolbar:not(.presets-editor-inset) > .v-toolbar__content > .v-btn--icon:first-child {
  margin-left: 6px !important;
}
/* Drawer open: native traffic lights are hidden, so the header is a normal bar.
   Chevron on the left, info and the ⋮ menu on the right.
   The logo is centered in the free space between those buttons.
   --nd-logo-left / --nd-logo-right are the measured button edges. */
.v-navigation-drawer.nd-glass.v-navigation-drawer--open .nd-header,
.v-navigation-drawer.nd-glass.nd-closing .nd-header {
  position: relative !important;
}
.v-navigation-drawer.nd-glass.v-navigation-drawer--open .nd-header-tile,
.v-navigation-drawer.nd-glass.nd-closing .nd-header-tile,
.v-navigation-drawer.nd-glass.v-navigation-drawer--open .nd-header-tile .v-list__tile,
.v-navigation-drawer.nd-glass.nd-closing .nd-header-tile .v-list__tile {
  position: relative !important;
  padding-left: 0 !important;
  padding-right: 0 !important;
}
.v-navigation-drawer.nd-glass.v-navigation-drawer--open .nd-header-back,
.v-navigation-drawer.nd-glass.nd-closing .nd-header-back {
  position: absolute !important;
  left: 0 !important;
  right: auto !important;
  top: 0 !important;
  z-index: 40 !important;
  width: 48px !important;
  min-width: 48px !important;
  max-width: 48px !important;
  height: 100% !important;
  margin: 0 !important;
  padding: 0 !important;
  display: flex !important;
  align-items: center !important;
  justify-content: center !important;
}
.v-navigation-drawer.nd-glass.v-navigation-drawer--open .nd-header .v-avatar,
.v-navigation-drawer.nd-glass.nd-closing .nd-header .v-avatar {
  padding-left: 0 !important;
}
.v-navigation-drawer.nd-glass.v-navigation-drawer--open .nd-header .lyrion-logo,
.v-navigation-drawer.nd-glass.nd-closing .nd-header .lyrion-logo,
.v-navigation-drawer.nd-glass.v-navigation-drawer--open .nd-header .nd-user,
.v-navigation-drawer.nd-glass.nd-closing .nd-header .nd-user {
  position: absolute !important;
  left: var(--nd-logo-left, 48px) !important;
  right: var(--nd-logo-right, 48px) !important;
  top: 0 !important;
  bottom: 0 !important;
  width: auto !important;
  height: auto !important;
  margin: 0 !important;
  padding: 0 !important;
  display: flex !important;
  align-items: center !important;
  justify-content: center !important;
  z-index: 2 !important;
  pointer-events: none !important;
}
/* Before the first measure: no chevron, or the ⋮ menu still a sibling at right:40px. */
.v-navigation-drawer.nd-glass.v-navigation-drawer--open .nd-header:not(:has(.nd-header-back)) .lyrion-logo,
.v-navigation-drawer.nd-glass.nd-closing .nd-header:not(:has(.nd-header-back)) .lyrion-logo,
.v-navigation-drawer.nd-glass.v-navigation-drawer--open .nd-header:not(:has(.nd-header-back)) .nd-user,
.v-navigation-drawer.nd-glass.nd-closing .nd-header:not(:has(.nd-header-back)) .nd-user {
  left: var(--nd-logo-left, 0px) !important;
}
.v-navigation-drawer.nd-glass.v-navigation-drawer--open .nd-header:has(> .v-menu) .lyrion-logo,
.v-navigation-drawer.nd-glass.nd-closing .nd-header:has(> .v-menu) .lyrion-logo,
.v-navigation-drawer.nd-glass.v-navigation-drawer--open .nd-header:has(> .v-menu) .nd-user,
.v-navigation-drawer.nd-glass.nd-closing .nd-header:has(> .v-menu) .nd-user {
  right: var(--nd-logo-right, calc(40px + var(--icon-size, 24px) + 12px)) !important;
}
.v-navigation-drawer.nd-glass.v-navigation-drawer--open .nd-header .nd-user > *,
.v-navigation-drawer.nd-glass.nd-closing .nd-header .nd-user > * {
  pointer-events: auto !important;
}
.v-navigation-drawer.nd-glass.v-navigation-drawer--open .nd-header .lyrion-logo img,
.v-navigation-drawer.nd-glass.nd-closing .nd-header .lyrion-logo img {
  position: relative !important;
  left: auto !important;
  right: auto !important;
  top: auto !important;
  margin: 0 !important;
  pointer-events: auto !important;
}
.v-navigation-drawer.nd-glass.v-navigation-drawer--open .nd-header-info,
.v-navigation-drawer.nd-glass.nd-closing .nd-header-info {
  position: absolute !important;
  right: 0 !important;
  left: auto !important;
  top: 0 !important;
  z-index: 40 !important;
  margin: 0 !important;
  height: 100% !important;
}
.lyrion-nd-ctx {
  position: fixed;
  z-index: 500;
  min-width: 220px;
  padding: 6px;
  border-radius: 10px;
  background: rgba(44, 44, 46, 0.96);
  color: #f5f5f7;
  border: 0.5px solid rgba(255, 255, 255, 0.16);
  box-shadow: 0 12px 40px rgba(0, 0, 0, 0.45);
  font: 13px -apple-system, BlinkMacSystemFont, "SF Pro Text", sans-serif;
}
.lyrion-nd-ctx button {
  display: block;
  width: 100%;
  text-align: left;
  background: transparent;
  color: inherit;
  border: 0;
  border-radius: 6px;
  padding: 8px 12px;
  font: inherit;
  cursor: default;
}
.lyrion-nd-ctx button:hover { background: #0a84ff; color: white; }
/* Glass keyboard cursor: a solid plate plus a ring above the row.
   Mirrors html/css/themes/glass.css so the app shows it before a skin deploy. */
html.theme-glass .lms-list .browse-kb-select,
html.theme-glass .lms-list .browse-kb-select .v-list__tile,
html.theme-glass .lms-list .search-highlight,
html.theme-glass .lms-list .search-highlight .v-list__tile,
html.theme-glass .lms-list .list-swipe-tile.highlight,
html.theme-glass .lms-list .list-swipe-tile.highlight .v-list__tile,
html.theme-glass .lms-list .browse-header.browse-kb-select.v-list__tile,
html.theme-glass .lms-list .grid-header.browse-kb-select.v-list__tile,
html.theme-glass .lms-list .browse-header.search-highlight.v-list__tile,
html.theme-glass .lms-list .grid-header.search-highlight.v-list__tile,
html.theme-glass #browse-view.browse-home-split .browse-home-pane-kb,
html.theme-glass #browse-view.browse-home-split .browse-home-pane-kb .v-list__tile {
  background-color: color-mix(in srgb, var(--active-color, #e3fdc4) 34%, rgb(18, 18, 22)) !important;
  background-image: none !important;
}
html.theme-glass .lms-list .browse-kb-select.image-grid-item,
html.theme-glass .lms-list .search-highlight.image-grid-item {
  background-color: transparent !important;
  background-image: none !important;
}
html.theme-glass .lms-list .list-swipe-row.browse-kb-select,
html.theme-glass .lms-list .browse-kb-select.image-grid-item,
html.theme-glass .lms-list .browse-kb-select.text-grid-item,
html.theme-glass .lms-list .search-highlight.image-grid-item,
html.theme-glass .lms-list .search-highlight.text-grid-item,
html.theme-glass .lms-list .lms-list-item.browse-kb-select:not(.list-swipe-tile),
html.theme-glass .lms-list .list-swipe-tile.highlight,
html.theme-glass #browse-view.browse-home-split .browse-home-pane-kb {
  position: relative;
}
html.theme-glass .lms-list .list-swipe-row.browse-kb-select::after,
html.theme-glass .lms-list .browse-kb-select.image-grid-item::after,
html.theme-glass .lms-list .browse-kb-select.text-grid-item::after,
html.theme-glass .lms-list .search-highlight.image-grid-item::after,
html.theme-glass .lms-list .search-highlight.text-grid-item::after,
html.theme-glass .lms-list .lms-list-item.browse-kb-select:not(.list-swipe-tile)::after,
html.theme-glass .lms-list .list-swipe-tile.highlight::after,
html.theme-glass #browse-view.browse-home-split .browse-home-pane-kb::after {
  content: "";
  position: absolute;
  inset: 1px;
  border: 2px solid var(--active-color, var(--primary-color, #e3fdc4));
  border-radius: 8px;
  pointer-events: none;
  z-index: 6;
}
/* Recycled rows: drop the persistent layer so the previous item does not
   flash at the new scroll position. Same rule as style.css. */
.vue-recycle-scroller.ready .vue-recycle-scroller__item-view {
  will-change: auto !important;
}
/* Live blur over a scrolling list is recomputed every frame. WKWebView on
   this Mac shows the previous frame until that finishes. */
html.theme-glass .lms-toolbar,
html.theme-glass .subtoolbar,
html.theme-glass .browse-subheader,
html.theme-glass .toolbar-blur,
#browse-list.bgnd-blur,
#browse-list.backdrop-blur,
#queue-list.bgnd-blur,
#queue-list.backdrop-blur,
.lms-jumplist.bgnd-blur,
.lms-jumplist.backdrop-blur {
  backdrop-filter: none !important;
  -webkit-backdrop-filter: none !important;
}
html.theme-glass .subtoolbar,
html.theme-glass #browse-view .browse-subheader,
html.theme-glass .browse-subheader {
  background-color: rgba(44, 44, 46, 0.94) !important;
  background-image: none !important;
}
`;

function applyShellChrome() {
  const frame = materialFrame();
  try {
    const doc = frame?.contentDocument;
    if (!doc?.head) return;
    // Drop a previous inset that pushed the player name right.
    const prev = doc.getElementById("lyrion-shell-chrome");
    if (prev && prev.textContent === SHELL_CHROME_CSS) return;
    prev?.remove();
    const style = doc.createElement("style");
    style.id = "lyrion-shell-chrome";
    style.textContent = SHELL_CHROME_CSS;
    doc.head.appendChild(style);
  } catch {
    /* cross-origin */
  }
}

/** Drag only the empty toolbar gap between the player name and the volume controls. */
function syncTitlebarDrag() {
  const strip = document.querySelector(".titlebar-drag") as HTMLElement | null;
  if (!strip) return;
  if (state.value.configMode || state.value.wizardMode || state.value.localBoot.active) {
    strip.style.width = "0px";
    strip.style.pointerEvents = "none";
    return;
  }
  let left = 0;
  let width = 0;
  let top = 0;
  let height = 52;
  try {
    const doc = materialFrame()?.contentDocument;
    const spacer = doc?.querySelector("#main-toolbar .drag-area") as HTMLElement | null;
    const shown = !!spacer && spacer.offsetParent !== null;
    if (spacer && shown) {
      const r = spacer.getBoundingClientRect();
      const pad = 8;
      left = Math.round(r.left + pad);
      width = Math.round(r.width - pad * 2);
      top = Math.round(r.top);
      height = Math.max(28, Math.round(r.height));
    }
  } catch {
    width = 0;
  }
  if (width < 24) {
    strip.style.width = "0px";
    strip.style.pointerEvents = "none";
    return;
  }
  strip.style.left = left + "px";
  strip.style.top = top + "px";
  strip.style.width = width + "px";
  strip.style.height = height + "px";
  strip.style.right = "auto";
  strip.style.pointerEvents = "auto";
}

let sortModeSent = "";
function currentSortMode(): "none" | "name" | "all" {
  if (state.value.configMode || state.value.wizardMode || state.value.localBoot.active) return "none";
  try {
    const doc = materialFrame()?.contentDocument;
    if (!doc) return "none";
    const fusion = (doc.querySelector(".lib-fusion") as any)?.__vue__;
    if (fusion && typeof fusion.setSort === "function") {
      return fusion.tab === "artists" ? "name" : "all";
    }
    const view = (doc.querySelector("#browse-view") as any)?.__vue__;
    const params = view?.command?.params;
    if (!Array.isArray(params)) return "none";
    const sortable = params.some(
      (p: unknown) => typeof p === "string" && (p.startsWith("sort:") || p.startsWith("msk-sort:"))
    );
    return sortable ? "all" : "none";
  } catch {
    return "none";
  }
}

async function reportSortMenu() {
  const mode = currentSortMode();
  if (mode === sortModeSent) return;
  sortModeSent = mode;
  await invoke("set_sort_menu", { mode }).catch(() => {
    sortModeSent = "";
  });
}

let viewLayoutSent = "";
function currentViewLayout(): "grid" | "list" | "none" | null {
  if (state.value.configMode || state.value.wizardMode || state.value.localBoot.active) return null;
  try {
    const doc = materialFrame()?.contentDocument;
    if (!doc) return null;
    const view = (doc.querySelector("#browse-view") as any)?.__vue__;
    if (view?.fusionMode) return "none";
    if (view?.grid && typeof view.grid.use !== "undefined") {
      return view.grid.allowed !== false && view.grid.use ? "grid" : "list";
    }
    const list = doc.querySelector("#browse-list");
    if (!list) return null;
    return list.classList.contains("lms-image-grid") ? "grid" : "list";
  } catch {
    return null;
  }
}

async function reportViewLayout() {
  const mode = currentViewLayout();
  if (mode === null || mode === viewLayoutSent) return;
  viewLayoutSent = mode;
  await invoke("set_view_layout", { mode }).catch(() => {
    viewLayoutSent = "";
  });
}

let chromeMenusBusy = false;
function syncChromeMenus() {
  if (chromeMenusBusy) return;
  chromeMenusBusy = true;
  void (async () => {
    try {
      // Layout first: a sort change rebuilds the tray from this stored mode.
      await reportViewLayout();
      await reportSortMenu();
    } finally {
      chromeMenusBusy = false;
    }
  })();
}

let chromeSyncTimer: ReturnType<typeof setInterval> | null = null;
/** Center the device-menu logo in the gap between the side buttons. */
function syncDrawerLogoGap() {
  try {
    const doc = materialFrame()?.contentDocument;
    if (!doc) return;
    doc.querySelectorAll(".v-navigation-drawer.nd-glass .nd-header").forEach((node) => {
      const header = node as HTMLElement;
      const rect = header.getBoundingClientRect();
      if (rect.width < 8) return;
      const logo = header.querySelector(".lyrion-logo, .nd-user");
      let leftEdge = rect.left;
      let rightEdge = rect.right;
      header.querySelectorAll(".v-btn").forEach((btn) => {
        if (logo?.contains(btn)) return;
        if ((btn as HTMLElement).closest(".v-menu__content")) return;
        const b = btn.getBoundingClientRect();
        if (b.width < 2 || b.height < 2) return;
        const vMid = (b.top + b.bottom) / 2;
        if (vMid < rect.top || vMid > rect.bottom) return;
        const mid = (b.left + b.right) / 2;
        if (mid <= rect.left + rect.width / 2) leftEdge = Math.max(leftEdge, b.right);
        else rightEdge = Math.min(rightEdge, b.left);
      });
      if (rightEdge - leftEdge < 32) return;
      const left = Math.max(0, Math.round(leftEdge - rect.left)) + "px";
      const right = Math.max(0, Math.round(rect.right - rightEdge)) + "px";
      if (header.style.getPropertyValue("--nd-logo-left") !== left) header.style.setProperty("--nd-logo-left", left);
      if (header.style.getPropertyValue("--nd-logo-right") !== right) header.style.setProperty("--nd-logo-right", right);
    });
  } catch {
    /* cross-origin */
  }
}

function ensureChromeSync() {
  syncTitlebarDrag();
  syncTrafficForDrawer();
  syncDrawerLogoGap();
  bindPlayerDrawer();
  syncChromeMenus();
  if (chromeSyncTimer) return;
  chromeSyncTimer = setInterval(() => {
    syncTitlebarDrag();
    syncTrafficForDrawer();
    syncDrawerLogoGap();
    bindPlayerDrawer();
    syncChromeMenus();
  }, 400);
}

let trafficLightsHidden = false;
function drawerOpen(doc: Document | null | undefined): boolean {
  if (!doc) return false;
  const open = !!doc.querySelector(
    ".v-navigation-drawer.nd-glass.v-navigation-drawer--open, .v-navigation-drawer.nd-glass.nd-closing"
  );
  const chevron = !!doc.querySelector(".v-navigation-drawer.nd-glass .nd-header-back");
  return open && chevron;
}

function syncTrafficForDrawer() {
  let open = false;
  try {
    open = drawerOpen(materialFrame()?.contentDocument);
  } catch {
    open = false;
  }
  if (open === trafficLightsHidden) return;
  trafficLightsHidden = open;
  void invoke("set_traffic_lights_hidden", { hidden: open }).catch(() => {
    trafficLightsHidden = !open;
  });
}

function navDrawerVm(doc: Document): any {
  const el = doc.querySelector(".v-navigation-drawer.nd-glass") as any;
  let vm = el?.__vue__;
  while (vm && !vm.visiblePlayers && vm.$parent) vm = vm.$parent;
  return vm?.visiblePlayers ? vm : null;
}

function closeNdMenu(doc: Document) {
  doc.getElementById("lyrion-nd-ctx")?.remove();
}

function ndLabel(win: any, key: string): string {
  return typeof win.i18n === "function" ? win.i18n(key) : key;
}

function showNdMenu(doc: Document, win: any, vm: any, player: any, x: number, y: number) {
  closeNdMenu(doc);
  const players = vm.$store?.state?.players || [];
  const items: { label: string; run: () => void }[] = [
    {
      label: ndLabel(win, "Sleep"),
      run: () => win.bus?.$emit("dlg.open", "sleep", player),
    },
    {
      label: ndLabel(win, "Alarms"),
      run: () => win.bus?.$emit("dlg.open", "playersettings", player, "alarms"),
    },
  ];
  if (players.length > 1 && typeof win.choose === "function") {
    items.push({
      label: ndLabel(win, "Transfer queue"),
      run: () => transferNdQueue(win, vm, player),
    });
  }
  items.push({
    label: ndLabel(win, "Presets"),
    run: () => win.bus?.$emit("dlg.open", "presetseditor", player.id, player.name),
  });
  const menu = doc.createElement("div");
  menu.id = "lyrion-nd-ctx";
  menu.className = "lyrion-nd-ctx";
  for (const item of items) {
    const btn = doc.createElement("button");
    btn.type = "button";
    btn.textContent = item.label;
    btn.addEventListener("click", (ev) => {
      ev.preventDefault();
      ev.stopPropagation();
      closeNdMenu(doc);
      if (vm.show !== undefined) vm.show = false;
      item.run();
    });
    menu.appendChild(btn);
  }
  doc.body.appendChild(menu);
  const w = menu.offsetWidth || 220;
  const h = menu.offsetHeight || 180;
  const left = Math.max(8, Math.min(x, win.innerWidth - w - 8));
  const top = Math.max(8, Math.min(y, win.innerHeight - h - 8));
  menu.style.left = left + "px";
  menu.style.top = top + "px";
}

function transferNdQueue(win: any, vm: any, target: any) {
  const from = vm.$store?.state?.player;
  const players = vm.$store?.state?.players || [];
  if (!target || !from || players.length < 2) return;
  const i18n = (s: string, a?: string) =>
    typeof win.i18n === "function" ? (a ? win.i18n(s, a) : win.i18n(s)) : s;
  const send = (fromId: string, toId: string, modeVal: number, toName: string) => {
    const mode = modeVal === 0 ? "copy" : modeVal === 1 ? "move" : "swap";
    win.lmsCommand?.("", ["material-skin", "transferqueue", "from:" + fromId, "to:" + toId, "mode:" + mode])
      ?.then?.((res: any) => {
        const err = res?.data?.result?.error;
        if (err) {
          win.bus?.$emit("showError", undefined, err);
          return;
        }
        if (modeVal === 0) win.bus?.$emit("showMessage", i18n("Queue copied to '%1'", toName));
        else vm.$store?.commit("setPlayer", toId);
        win.bus?.$emit("refreshStatus", toId);
      });
  };
  if (from.id === target.id) {
    const list = players
      .filter((p: any) => p.id !== from.id)
      .map((p: any) => ({ val: p.id, title: p.name }));
    win.choose(i18n("Transfer queue"), list, {
      options: [
        { val: 0, title: i18n("Copy the queue to") },
        { val: 1, title: i18n("Move the queue to") },
        { val: 2, title: i18n("Swap the queue with") },
      ],
      key: "movequeue",
      def: 1,
    }).then((choice: any) => {
      if (!choice?.item) return;
      const modeVal = choice.option ? choice.option.val : 1;
      send(from.id, choice.item.val, modeVal, choice.item.title);
    });
    return;
  }
  win.choose(i18n("Transfer queue"), [
    { val: 0, title: i18n("Copy the queue to") + " " + target.name },
    { val: 1, title: i18n("Move the queue to") + " " + target.name },
    { val: 2, title: i18n("Swap the queue with") + " " + target.name },
  ], { key: "movequeue", def: 1 }).then((choice: any) => {
    if (choice == null) return;
    const modeVal = choice.val != null ? choice.val : choice.item ? choice.item.val : 1;
    send(from.id, target.id, modeVal, target.name);
  });
}

const platineItems = new Map<string, any>();
let platineDspOn = false;
let platineSleepKey = "platine_sleep_0";
let platineSleepHoldUntil = 0;
let platineTimer: ReturnType<typeof setInterval> | null = null;
let platineBusy = false;
let platinePending = false;
let platineGen = 0;

function materialWinStore(): { win: any; store: any; doc: Document } | null {
  try {
    const frame = materialFrame();
    const doc = frame?.contentDocument;
    const win = frame?.contentWindow as any;
    if (!doc || !win) return null;
    let vm = (doc.querySelector("#app") as any)?.__vue__;
    while (vm && !vm.$store && vm.$parent) vm = vm.$parent;
    let store = vm?.$store;
    if (!store) store = (doc.querySelector("#main-toolbar") as any)?.__vue__?.$store;
    if (!store) return null;
    return { win, store, doc };
  } catch {
    return null;
  }
}

function platineRpc(r: any): any {
  if (!r) return null;
  if (r.data && r.data.result) return r.data.result;
  if (r.result) return r.result;
  if (r.data && !r.data.method) return r.data;
  return null;
}

function platineChoices(section: any, prefix: string): { key: string; label: string; selected: boolean }[] {
  const items = Array.isArray(section?.items) ? section.items : [];
  const out: { key: string; label: string; selected: boolean }[] = [];
  items.forEach((it: any, index: number) => {
    const raw = it?.id != null && String(it.id) !== "" ? String(it.id) : String(index);
    const key = prefix + raw;
    const label = String(it?.label || it?.text || raw);
    platineItems.set(key, it);
    out.push({ key, label, selected: !!it?.selected });
  });
  return out;
}

function replacePlatinePrefix(prefix: string) {
  for (const key of [...platineItems.keys()]) {
    if (key.startsWith(prefix)) platineItems.delete(key);
  }
}

let platineDebounce: ReturnType<typeof setTimeout> | null = null;
function schedulePlatineMenu() {
  if (platineDebounce) clearTimeout(platineDebounce);
  platineDebounce = window.setTimeout(() => {
    platineDebounce = null;
    void reportPlatineMenu();
  }, 500);
}

function platineSyncId(value: any): string {
  if (!value) return "";
  if (typeof value === "string") return value;
  if (typeof value === "object" && value.id) return String(value.id);
  return "";
}

function platineNormId(value: any): string {
  return platineSyncId(value).trim().toLowerCase();
}

/** LMS `sync ?` lists every other member. "-" or empty means this player is alone. */
function platineMembersFromSync(raw: any, selfId: string): Set<string> {
  const set = new Set<string>();
  const text = raw == null ? "" : String(raw).trim();
  if (!text || text === "-") return set;
  const self = selfId.trim().toLowerCase();
  for (const part of text.split(",")) {
    const id = part.trim().toLowerCase();
    if (!id || id === "-" || id === self) continue;
    set.add(id);
  }
  return set;
}

/** Toolbar status for the current player. A slave often lacks its siblings here. */
function platineMembersFromStatus(status: any, selfId: string): Set<string> {
  const set = new Set<string>();
  const self = selfId.trim().toLowerCase();
  const master = platineNormId(status?.syncmaster);
  if (master && master !== self) set.add(master);
  const slaves = status?.syncslaves;
  if (Array.isArray(slaves)) {
    for (const slave of slaves) {
      const id = platineNormId(slave);
      if (id && id !== self) set.add(id);
    }
  }
  return set;
}

let platineSyncKnown: string[] = [];
let platineSyncHoldIds: string[] | null = null;
let platineSyncHoldUntil = 0;

function platineApplySyncHold(members: Set<string>): Set<string> {
  if (platineSyncHoldIds == null) return members;
  if (Date.now() >= platineSyncHoldUntil) {
    platineSyncHoldIds = null;
    return members;
  }
  const hold = new Set(platineSyncHoldIds);
  const same = hold.size === members.size && [...hold].every((id) => members.has(id));
  if (same) {
    platineSyncHoldIds = null;
    return members;
  }
  return hold;
}

function ensurePlatineSync() {
  schedulePlatineMenu();
  if (platineTimer) return;
  platineTimer = setInterval(() => {
    void reportPlatineMenu();
  }, 6000);
}

async function reportPlatineMenu() {
  if (state.value.wizardMode || state.value.configMode || state.value.localBoot.active) return;
  if (!hasServer()) return;
  if (platineBusy) {
    platinePending = true;
    return;
  }
  const ctx = materialWinStore();
  if (!ctx) return;
  platineBusy = true;
  const gen = ++platineGen;
  try {
    const player = ctx.store.state?.player;
    const players = Array.isArray(ctx.store.state?.players) ? ctx.store.state.players : [];
    const enabled = !!(player?.id && !player.isgroup);
    const bar = (ctx.doc.querySelector("#main-toolbar") as any)?.__vue__;
    const status = bar?.playerStatus || {};
    const sleeping = Number(status.sleepTime || player?.will_sleep_in || 0) > 0;
    if (!sleeping && Date.now() > platineSleepHoldUntil) platineSleepKey = "platine_sleep_0";
    const sleepFlag = Date.now() < platineSleepHoldUntil
      ? (platineSleepKey === "platine_sleep_0" ? 0 : 1)
      : (sleeping ? 1 : 0);
    const selfId = String(player?.id || "");
    let members = platineMembersFromStatus(status, selfId);
    if (enabled && typeof ctx.win.lmsCommand === "function") {
      try {
        const syncRes = await ctx.win.lmsCommand(player.id, ["sync", "?"]);
        if (gen !== platineGen) return;
        const parsed = platineRpc(syncRes);
        if (parsed && parsed._sync != null) members = platineMembersFromSync(parsed._sync, selfId);
      } catch {
        /* keep the toolbar group */
      }
    }
    members = platineApplySyncHold(members);
    platineSyncKnown = [...members];
    const synced = members.size > 0;
    const syncPlayers: { id: string; name: string; selected: boolean }[] = [];
    for (const p of players) {
      if (!p?.id || p.isgroup || String(p.id) === selfId) continue;
      const id = String(p.id);
      syncPlayers.push({
        id,
        name: String(p.name || id),
        selected: members.has(id.trim().toLowerCase()),
      });
    }
    const canTransfer = enabled && players.some((p: any) => p?.id && p.id !== player.id);
    let inputs: { key: string; label: string; selected: boolean }[] = [];
    let outputs: { key: string; label: string; selected: boolean }[] = [];
    let dsp: { key: string; label: string; selected: boolean }[] = [];
    let presets: { key: string; label: string; selected: boolean }[] = [];
    let dspOn = false;
    let dspKnown = false;
    if (enabled && typeof ctx.win.lmsCommand === "function") {
      try {
        const can = await ctx.win.lmsCommand(player.id, ["can", "squeezedsp.filters", "?"]);
        if (gen !== platineGen) return;
        const res = platineRpc(can);
        const v = res && (res._can != null ? res._can : res.can);
        dspKnown = v === 1 || v === "1" || v === true;
      } catch {
        dspKnown = false;
      }
    }
    if (enabled && typeof ctx.win.PlayerPrefsMenu?.loadMenu === "function") {
      let loaded: any;
      try {
        loaded = await ctx.win.PlayerPrefsMenu.loadMenu(ctx.store);
      } catch {
        return;
      }
      if (gen !== platineGen) return;
      const sections = Array.isArray(loaded?.sections) ? loaded.sections : [];
      const section = (id: string) => sections.find((s: any) => s?.id === id);
      replacePlatinePrefix("platine_in:");
      replacePlatinePrefix("platine_out:");
      replacePlatinePrefix("platine_dsp:");
      replacePlatinePrefix("platine_preset:");
      inputs = platineChoices(section("input"), "platine_in:");
      outputs = platineChoices(section("output"), "platine_out:");
      dsp = platineChoices(section("dsp"), "platine_dsp:");
      presets = platineChoices(section("presets"), "platine_preset:");
      dspOn = sections.some((s: any) => s?.id === "dsp");
    } else {
      dspKnown = false;
    }
    if (gen !== platineGen) return;
    platineDspOn = dspOn;
    await invoke("set_platine_menu", {
      state: {
        enabled,
        sleep: sleepFlag,
        sleep_key: platineSleepKey,
        synced,
        sync_master: synced ? [...members].sort().join(",") : "",
        players: syncPlayers,
        can_transfer: canTransfer,
        inputs,
        outputs,
        dsp,
        dsp_on: dspOn,
        dsp_known: dspKnown,
        presets,
      },
    }).catch(() => {});
  } finally {
    platineBusy = false;
    if (platinePending) {
      platinePending = false;
      void reportPlatineMenu();
    }
  }
}

async function togglePlatineDsp(win: any, playerId: string, turnOff: boolean): Promise<boolean> {
  if (typeof win.lmsCommand !== "function") return false;
  const read = await win.lmsCommand(playerId, ["squeezedsp.readclientSettings"]);
  const res = platineRpc(read);
  let data: any = {};
  try {
    const raw = res?.json;
    data = raw ? (typeof raw === "string" ? JSON.parse(raw) : raw) : {};
  } catch {
    data = {};
  }
  if (!data || typeof data !== "object") data = {};
  if (!data.Client || typeof data.Client !== "object") data.Client = {};
  if (turnOff) {
    data.Client.Bypass = "1";
    data.Client.Preset = "";
    data.Client.Last_preset = "";
    data.PresetName = "None";
  } else {
    data.Client.Bypass = "0";
  }
  await win.lmsCommand(playerId, ["squeezedsp.saveall", "val:" + JSON.stringify(data)]);
  try {
    await win.lmsCommand(playerId, ["time", "+0.5"]);
  } catch {
    /* not playing */
  }
  win.PlayerPrefsMenu?.invalidateCaps?.(playerId);
  return true;
}

async function applyPlatineAction(id: string) {
  const ctx = materialWinStore();
  if (!ctx) return;
  const player = ctx.store.state?.player;
  const playerId = String(player?.id || "");
  const dialog = id === "platine_alarm" || id === "platine_transfer" || id === "platine_preset_edit";
  if (dialog) await showMainWindow();
  if (id === "platine_alarm") {
    ctx.win.bus?.$emit("dlg.open", "playersettings", undefined, "alarms");
    return;
  }
  if (!playerId) return;
  if (id === "platine_transfer") {
    if (typeof ctx.win.choose === "function") transferNdQueue(ctx.win, { $store: ctx.store }, player);
    return;
  }
  if (id === "platine_preset_edit") {
    ctx.win.bus?.$emit("dlg.open", "presetseditor", player.id, player.name);
    return;
  }
  if (id.startsWith("platine_sleep_")) {
    platineSleepKey = id;
    platineSleepHoldUntil = Date.now() + 4000;
    if (id === "platine_sleep_end") await ctx.win.lmsCommand?.(playerId, ["jiveendoftracksleep"]);
    else if (id === "platine_sleep_0") await ctx.win.lmsCommand?.(playerId, ["sleep", 0]);
    else {
      const secs = Number(id.slice("platine_sleep_".length));
      if (secs > 0) await ctx.win.lmsCommand?.(playerId, ["sleep", secs]);
    }
  } else if (id === "platine_sync_off") {
    platineSyncHoldIds = [];
    platineSyncHoldUntil = Date.now() + 4000;
    void reportPlatineMenu();
    await ctx.win.lmsCommand?.(playerId, ["sync", "-"]);
  } else if (id.startsWith("platine_sync:")) {
    const dest = id.slice("platine_sync:".length);
    if (dest) {
      const next = new Set(platineSyncKnown);
      next.add(dest.trim().toLowerCase());
      platineSyncHoldIds = [...next];
      platineSyncHoldUntil = Date.now() + 4000;
      void reportPlatineMenu();
      await ctx.win.lmsCommand?.(dest, ["sync", playerId]);
    }
  } else if (id === "platine_dsp_off") {
    const turnOff = platineDspOn;
    const ok = await togglePlatineDsp(ctx.win, playerId, turnOff);
    if (ok) platineDspOn = !turnOff;
  } else {
    const item = platineItems.get(id);
    if (item && typeof ctx.win.PlayerPrefsMenu?.applyItem === "function") {
      await ctx.win.PlayerPrefsMenu.applyItem(playerId, item);
    }
  }
  ctx.win.bus?.$emit("refreshStatus", playerId);
  window.setTimeout(() => {
    void reportPlatineMenu();
  }, 600);
}

function ndPlayerTarget(doc: Document, target: EventTarget | null): { vm: any; player: any } | null {
  const el = target as HTMLElement | null;
  const tile = el?.closest?.("[id^='nd-player-']") as HTMLElement | null;
  if (!tile || !tile.closest("#nd-list")) return null;
  const vm = navDrawerVm(doc);
  if (!vm) return null;
  const idx = parseInt(tile.id.replace("nd-player-", ""), 10);
  const player = vm.visiblePlayers?.[idx];
  if (!player) return null;
  return { vm, player };
}

function openNdContext(doc: Document, win: any, ev: MouseEvent) {
  const hit = ndPlayerTarget(doc, ev.target);
  if (!hit) return false;
  ev.preventDefault();
  ev.stopPropagation();
  if (typeof hit.vm.playerContextMenu === "function") {
    hit.vm.playerContextMenu(hit.player, ev);
    return true;
  }
  showNdMenu(doc, win, hit.vm, hit.player, ev.clientX, ev.clientY);
  return true;
}

function bindPlayerDrawer() {
  const frame = materialFrame();
  const doc = frame?.contentDocument;
  const win = frame?.contentWindow as any;
  if (!doc?.documentElement || !win || doc.documentElement.dataset.lyrionNdCtx === "1") return;
  doc.documentElement.dataset.lyrionNdCtx = "1";
  doc.addEventListener(
    "mousedown",
    (ev: Event) => {
      if ((ev as MouseEvent).button !== 0) return;
      if (!(ev.target as HTMLElement | null)?.closest?.("#lyrion-nd-ctx")) closeNdMenu(doc);
    },
    true
  );
  // Ctrl-click selects the row (and closes the drawer) before contextmenu.
  doc.addEventListener(
    "click",
    (ev: Event) => {
      const mouse = ev as MouseEvent;
      if (mouse.button !== 0 || !mouse.ctrlKey || mouse.metaKey) return;
      if (!ndPlayerTarget(doc, ev.target)) return;
      mouse.preventDefault();
      mouse.stopPropagation();
    },
    true
  );
  doc.addEventListener(
    "contextmenu",
    (ev: Event) => {
      openNdContext(doc, win, ev as MouseEvent);
    },
    true
  );
  doc.addEventListener("keydown", (ev: Event) => {
    if ((ev as KeyboardEvent).key === "Escape") closeNdMenu(doc);
  });
  const obs = new win.MutationObserver(() => {
    syncTrafficForDrawer();
    syncDrawerLogoGap();
  });
  if (doc.body) obs.observe(doc.body, { attributes: true, subtree: true, attributeFilter: ["class"] });
  syncTrafficForDrawer();
}

function isTypingTarget(target: EventTarget | null): boolean {
  const el = target as HTMLElement | null;
  if (!el || !el.tagName) return false;
  return el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.tagName === "SELECT" || !!el.isContentEditable;
}

let lastTransport = "";
let lastTransportAt = 0;
function handleTransport(action: string) {
  const now = Date.now();
  if (action === lastTransport && now - lastTransportAt < 280) return;
  lastTransport = action;
  lastTransportAt = now;
  if (action === "playpause") void player.playPause();
  else if (action === "next") void player.next();
  else if (action === "previous") void player.previous();
  else if (action === "volup") void player.volumeUp();
  else if (action === "voldown") void player.volumeDown();
}

function onTransportKey(event: KeyboardEvent) {
  if (isTypingTarget(event.target)) return;
  if (state.value.configMode) return;
  const meta = event.metaKey || event.ctrlKey;
  if (meta && !event.shiftKey && !event.altKey && (event.key === "ArrowUp" || event.code === "ArrowUp")) {
    event.preventDefault();
    event.stopPropagation();
    handleTransport("volup");
    return;
  }
  if (meta && !event.shiftKey && !event.altKey && (event.key === "ArrowDown" || event.code === "ArrowDown")) {
    event.preventDefault();
    event.stopPropagation();
    handleTransport("voldown");
    return;
  }
  const code = event.code || event.key;
  if (code === "MediaPlayPause") {
    event.preventDefault();
    handleTransport("playpause");
  } else if (code === "MediaTrackNext") {
    event.preventDefault();
    handleTransport("next");
  } else if (code === "MediaTrackPrevious") {
    event.preventDefault();
    handleTransport("previous");
  }
}

function bindFrameKeys() {
  const doc = materialFrame()?.contentDocument;
  if (!doc || doc.documentElement.dataset.lyrionKeys === "1") return;
  doc.documentElement.dataset.lyrionKeys = "1";
  doc.addEventListener("keydown", onTransportKey, true);
}

function fusionSortId(tab: string, action: string): string | null {
  if (tab === "artists") {
    return action === "name" || action === "default" ? "name" : null;
  }
  if (tab === "songs") {
    if (action === "name") return "title";
    if (action === "date") return "year";
    if (action === "added") return "added";
    return "title";
  }
  if (action === "date") return "year";
  if (action === "added") return "added";
  return "album";
}

function applyBrowseSort(win: any, view: any, action: string) {
  const params: string[] = view?.command?.params;
  if (!Array.isArray(params)) {
    messager.show("Ouvrez Ma musique pour trier", 2000);
    return;
  }
  const rev = "msk-revsort:1";
  if (action === "reverse") {
    const i = params.indexOf(rev);
    if (i >= 0) params.splice(i, 1);
    else params.push(rev);
    view.refreshList?.(false);
    return;
  }
  const key =
    action === "date" ? "yearalbum" : action === "added" ? "new" : "album";
  let replaced = false;
  for (let i = 0; i < params.length; i++) {
    const p = params[i];
    if (typeof p === "string" && (p.startsWith("sort:") || p.startsWith("msk-sort:"))) {
      params[i] = "sort:" + key;
      replaced = true;
      break;
    }
  }
  if (!replaced) {
    messager.show("Ouvrez Ma musique pour trier", 2000);
    return;
  }
  try {
    if (typeof win.setAlbumSort === "function") {
      win.setAlbumSort(view.command, view.inGenre, key, params.indexOf(rev) >= 0, view.current);
    }
  } catch {
    /* browse still refreshes with the replaced sort param */
  }
  view.refreshList?.(false);
}

/** Filter / Sort from the macOS menu, against the same-origin Material iframe. */
function runLibraryCommand(action: string) {
  const frame = materialFrame();
  const doc = frame?.contentDocument;
  const win = frame?.contentWindow as any;
  if (!doc || !win) {
    messager.show("Ouvrez d'abord le lecteur", 2000);
    return;
  }
  const fusion = (doc.querySelector(".lib-fusion") as any)?.__vue__;
  if (fusion && typeof fusion.setSort === "function") {
    if (action === "filter") {
      fusion.openFilter?.();
      return;
    }
    if (action === "reverse") {
      fusion.toggleSortRev?.();
      return;
    }
    const id = fusionSortId(String(fusion.tab || "albums"), action);
    if (!id) {
      messager.show("Ce tri n'existe pas pour les artistes", 1800);
      return;
    }
    fusion.setSort(id);
    return;
  }
  const view = (doc.querySelector("#browse-view") as any)?.__vue__;
  if (!view) {
    messager.show("Ouvrez Ma musique pour filtrer ou trier", 2200);
    return;
  }
  if (action === "filter") {
    view.searchActive = 2;
    return;
  }
  applyBrowseSort(win, view, action);
}

function isUsableLmsHost(host: string): boolean {
  const s = (host || "").trim();
  if (!s) return false;
  if (s === "localhost" || s === "::1") return true;
  if (/^\d/.test(s)) {
    const parts = s.split(".");
    if (parts.length !== 4) return false;
    return parts.every((p) => /^(0|[1-9]\d{0,2})$/.test(p) && Number(p) <= 255);
  }
  return /^[a-zA-Z0-9]([a-zA-Z0-9-]*[a-zA-Z0-9])?(\.[a-zA-Z0-9]([a-zA-Z0-9-]*[a-zA-Z0-9])?)*$/.test(s);
}

async function notifyLmsTarget() {
  const host = String(state.value.config.server.host || "").trim();
  const port = Number(state.value.config.server.port) || 9000;
  if (!host) return;
  await tauriEmit("lms-target-changed", { host, port }).catch(() => {});
}

function postToMaterial(type: string, extra: Record<string, unknown> = {}) {
  const frame = materialFrame();
  try {
    frame?.contentWindow?.postMessage({ source: "lyrion-app", type, ...extra }, "*");
  } catch {
    /* cross-origin / missing frame */
  }
}

function configSnapshot() {
  return {
    version: state.value.version,
    config: JSON.parse(JSON.stringify(state.value.config)),
    clientRunning: state.value.client.running,
    showDockIcon: state.value.showDockIcon,
    hotkeyError: state.value.hotkeyError,
    hotkeyPresets,
    lmsInstall: JSON.parse(JSON.stringify(state.value.lmsInstall)),
    thisMachineHosts: state.value.thisMachineHosts.slice(),
  };
}

function hasServer() {
  return !!(state.value.config.server.host || "").trim();
}

async function showMainWindow() {
  try {
    await appWindow.show();
    await appWindow.setFocus();
  } catch {
    /* ignore */
  }
}

async function openWizard() {
  state.value.wizardMode = true;
  state.value.configMode = true;
  await showMainWindow();
}

async function openAppSettings() {
  await showMainWindow();
  // Prefer Material's own dialog when the player iframe is up.
  if (
    hasServer() &&
    state.value.configLoaded &&
    !state.value.configMode &&
    !state.value.wizardMode &&
    state.value.iframeSrc &&
    state.value.iframeSrc !== "about:blank"
  ) {
    postToMaterial("open-app-settings");
    return;
  }
  state.value.wizardMode = false;
  state.value.configMode = true;
}

async function closeAppSettings() {
  if (!hasServer()) {
    state.value.wizardMode = true;
    state.value.configMode = true;
    return;
  }
  state.value.wizardMode = false;
  state.value.configMode = false;
  await presentServer();
}

async function onWizardConnect(payload: { host: string; port: number }) {
  state.value.config.server.host = payload.host;
  state.value.config.server.port = payload.port;
  state.value.config.server.autodetect = false;
  await invoke('set_current_lms', { host: payload.host, port: Number(payload.port) || 9000 }).catch(() => {});
  await configManager.save();
  await notifyLmsTarget();
  state.value.configLoaded = true;
  state.value.wizardMode = false;
  if (state.value.config.client.enabled !== false) {
    await client.startMain(true);
  }
  await client.releasePreview();
  await presentServer(isThisMachineHost(payload.host));
}

listen('config', async () => {
  await openAppSettings();
});

listen('open-wizard', async () => {
  await openWizard();
});

listen('media-key', (event) => {
  const action = typeof event.payload === 'string' ? event.payload : '';
  if (action) handleTransport(action);
});

listen('platine-action', (event) => {
  const id = typeof event.payload === 'string' ? event.payload : '';
  if (!id) return;
  void applyPlatineAction(id).catch((e) => console.warn("platine", e));
});

listen('lms-local-starting', () => {
  const currentLocal = isThisMachineHost(String(state.value.config.server.host || ""));
  if (localBootWaiter || currentLocal || !hasServer()) {
    state.value.wizardMode = false;
    state.value.configMode = false;
    state.value.localBoot.active = true;
    state.value.localBoot.error = "";
    if (!state.value.localBoot.message || state.value.localBoot.message === "Vérification du serveur local…") {
      state.value.localBoot.message = "Démarrage du serveur local…";
    }
  }
  if (!state.value.localBoot.active) {
    messager.show("Démarrage du serveur local…", 0);
  }
});

listen('lms-local-ready', async (event) => {
  const p = event.payload as { host?: string; port?: number | string };
  const host = String(p?.host || "127.0.0.1").trim();
  const port = Number(p?.port) || 9000;
  if (localBootWaiter || localBootOwned) {
    state.value.localBoot.error = "";
    state.value.localBoot.message = "Serveur prêt";
    return;
  }
  const currentLocal = isThisMachineHost(String(state.value.config.server.host || ""));
  if (!hasServer()) {
    state.value.localBoot.active = false;
    await onWizardConnect({ host, port });
    return;
  }
  if (currentLocal || state.value.localBoot.active) {
    state.value.localBoot.active = true;
    state.value.localBoot.error = "";
    state.value.localBoot.message = "Serveur prêt";
    messager.show("Serveur prêt", 2200);
    await delay(600);
    if (localBootWaiter) return;
    state.value.localBoot.active = false;
    state.value.localBoot.error = "";
    await server.showPlayer();
    return;
  }
  messager.show("Serveur local prêt", 2500);
});

listen('select-server', async (event) => {
  const p = event.payload as {
    host?: string;
    port?: string | number;
    name?: string;
    default?: string | boolean;
    local?: string | boolean;
  };
  const host = String(p?.host || '').trim();
  const port = Number(p?.port) || 9000;
  if (!host) return;
  const prevHost = String(state.value.config.server.host || '');
  const prevPort = Number(state.value.config.server.port) || 9000;
  state.value.config.server.host = host;
  state.value.config.server.port = port;
  state.value.config.server.autodetect = false;
  const asDefault = p?.default === true || p?.default === '1' || p?.default === 'true';
  if (asDefault) {
    state.value.config.server.defaultHost = host;
    state.value.config.server.defaultPort = port;
    await invoke('set_default_lms', { host, port }).catch(() => {});
  }
  await invoke('set_current_lms', { host, port }).catch(() => {});
  await configManager.save();
  await notifyLmsTarget();
  if (host !== prevHost || port !== prevPort) {
    await client.releasePreview();
  }
  const local = p?.local === true || p?.local === '1' || p?.local === 'true' || isThisMachineHost(host);
  const result = await presentServer(local);
  if (result === "up" || result === "remote") {
    messager.show(p?.name ? ('Connecté à ' + p.name) : ('Connecté à ' + host), 1800);
  }
});

listen('lms-server-cmd', (event) => {
  const msg = typeof event.payload === "string" ? event.payload : "";
  if (msg) messager.show(msg, 4000);
});

listen('refresh-player', async () => {
  if (!hasServer()) {
    await openWizard();
    return;
  }
  await showMainWindow();
  state.value.wizardMode = false;
  state.value.configMode = false;
  await presentServer();
});

listen('browse-nav', async (event) => {
  if (!hasServer()) {
    await openWizard();
    return;
  }
  try {
    await appWindow.show();
    await appWindow.setFocus();
  } catch { /* ignore */ }
  state.value.wizardMode = false;
  if (state.value.configMode) {
    state.value.configMode = false;
  }
  const action = typeof event.payload === 'string' ? event.payload : String(event.payload || '');
  if (action) {
    postToMaterial('browse-nav', { action });
  }
  if (action === 'grid' || action === 'list') {
    window.setTimeout(() => void reportViewLayout(), 80);
    window.setTimeout(() => void reportViewLayout(), 320);
  }
});

listen('library-cmd', async (event) => {
  if (!hasServer()) {
    await openWizard();
    return;
  }
  if (state.value.localBoot.active) return;
  try {
    await appWindow.show();
    await appWindow.setFocus();
  } catch { /* ignore */ }
  state.value.wizardMode = false;
  state.value.configMode = false;
  const action = typeof event.payload === 'string' ? event.payload : String(event.payload || '');
  if (action) runLibraryCommand(action);
});

listen('open-server-settings', async () => {
  if (!hasServer()) {
    await openWizard();
    return;
  }
  try {
    await appWindow.show();
    await appWindow.setFocus();
  } catch { /* ignore */ }
  state.value.wizardMode = false;
  if (state.value.configMode) {
    state.value.configMode = false;
  }
  postToMaterial('open-server-settings');
});

listen('open-ui-settings', async () => {
  try {
    await appWindow.show();
    await appWindow.setFocus();
  } catch { /* ignore */ }
  if (state.value.configMode) {
    state.value.configMode = false;
  }
  postToMaterial('open-ui-settings');
});

listen('open-plugins', async () => {
  try {
    await appWindow.show();
    await appWindow.setFocus();
  } catch { /* ignore */ }
  if (state.value.configMode) {
    state.value.configMode = false;
  }
  postToMaterial('open-plugins');
});

listen('open-player-settings', async (event) => {
  const p = event.payload as { id?: string; name?: string };
  try {
    await appWindow.show();
    await appWindow.setFocus();
  } catch { /* ignore */ }
  if (state.value.configMode) {
    state.value.configMode = false;
  }
  postToMaterial('open-player-settings', { playerId: p?.id || '', id: p?.id || '', name: p?.name || '' });
});

listen('hud-reorder-players', async (event) => {
  const ids = (event.payload as { ids?: string[] })?.ids || [];
  postToMaterial('reorder-players', { ids });
});

listen('hud-set-player', async (event) => {
  const p = event.payload as { id?: string; name?: string };
  const id = String(p?.id || '').trim();
  if (!id) return;
  await invoke('set_current_player', { id }).catch(() => {});
  postToMaterial('set-player', { playerId: id, id, name: p?.name || '' });
});

listen('hud-open-player', async (event) => {
  const p = event.payload as { id?: string; name?: string };
  const id = String(p?.id || '').trim();
  try {
    await appWindow.show();
    await appWindow.setFocus();
  } catch { /* ignore */ }
  if (state.value.configMode) {
    state.value.configMode = false;
  }
  if (id) {
    await invoke('set_current_player', { id }).catch(() => {});
    postToMaterial('set-player', { playerId: id, id, name: p?.name || '' });
  }
});

// Material ↔ app prefs (postMessage; Material is cross-origin on the LMS host)
window.addEventListener('message', async (event) => {
  const data = event.data;
  if (!data || data.source !== 'lyrion-minim') return;
  if (data.type === 'ready') {
    clearIframeLoadTimer();
    state.value.iframeLoading = false;
    state.value.iframeError = '';
    kickPlayerFrameLayout();
    window.setTimeout(kickPlayerFrameLayout, 200);
    schedulePlatineMenu();
    return;
  }
  if (data.type === 'players') {
    const players = Array.isArray(data.players) ? data.players : [];
    const current = String(data.current || '');
    void invoke('set_menu_players', { players, current: current || null }).catch(() => {});
    schedulePlatineMenu();
    void tauriEmit('material-players', { players, current }).catch(() => {});
    if (current) {
      void invoke('set_current_player', { id: current }).catch(() => {});
    }
    return;
  }
  if (data.type === 'open-settings' || data.type === 'open-ack') {
    if (data.type === 'open-settings') {
      await openAppSettings();
    }
    return;
  }
  if (data.type === 'get-config') {
    void lmsLocal.refreshStatus().finally(() => {
      postToMaterial('config', configSnapshot());
    });
    return;
  }
  if (data.type === 'set-config' && data.config) {
    const cfg = data.config as typeof state.value.config;
    const prevHost = String(state.value.config.server.host || '');
    const prevPort = String(state.value.config.server.port || '');
    if (cfg.client) {
      if (cfg.client.name && cfg.client.name !== state.value.config.client.name) {
        state.value.config.client.name = cfg.client.name;
        state.value.client.needsRestart = true;
      }
      const enabled = cfg.client.enabled !== undefined
        ? !!cfg.client.enabled
        : (cfg.client as { autostart?: boolean }).autostart !== false;
      state.value.config.client.enabled = enabled;
      state.value.config.client.keepOpen = !!cfg.client.keepOpen;
    }
    if (cfg.server) {
      const newHost = String(cfg.server.host || "").trim();
      const newPort = Number(cfg.server.port) || 9000;
      if (isUsableLmsHost(newHost)) {
        state.value.config.server.host = newHost;
        state.value.config.server.port = newPort;
      }
      state.value.config.server.autodetect = !!cfg.server.autodetect;
    }
    // Persist host before Dock/hotkey — those can throw and used to skip save,
    // so prefs showed 1.4 while the LMS proxy kept serving 1.10 from disk.
    await configManager.save();
    const nowHost = String(state.value.config.server.host || '').trim();
    const nowPort = Number(state.value.config.server.port) || 9000;
    if (nowHost) {
      await invoke('set_current_lms', { host: nowHost, port: nowPort }).catch(() => {});
    }
    if (cfg.window) {
      try {
        if (typeof cfg.window.showDockIcon === 'boolean') {
          await dockIcon.apply(cfg.window.showDockIcon);
        }
      } catch (e) {
        console.warn("dock pref", e);
      }
      try {
        if (cfg.window.globalHotkey) {
          await hotkey.apply(String(cfg.window.globalHotkey));
        }
      } catch (e) {
        console.warn("hotkey pref", e);
      }
    }
    if (state.value.config.client.enabled && !state.value.client.running) {
      await client.startMain(true).catch((e) => console.warn("player start", e));
    } else if (!state.value.config.client.enabled && state.value.client.running) {
      await client.stopMain(true).catch((e) => console.warn("player stop", e));
    } else if (state.value.client.needsRestart && state.value.client.running) {
      await client.restart().catch((e) => console.warn("player restart", e));
      state.value.client.needsRestart = false;
    }
    await client.releasePreview();
    postToMaterial('config-saved', { ok: true, hotkeyError: state.value.hotkeyError });
    const host = String(state.value.config.server.host || '');
    const port = Number(state.value.config.server.port) || 9000;
    if (host && (host !== prevHost || port !== (Number(prevPort) || 9000))) {
      await notifyLmsTarget();
      await presentServer();
    }
    return;
  }
  if (data.type === 'app-action') {
    const action = String(data.action || '');
    try {
      if (action === 'start-player') {
        state.value.config.client.enabled = true;
        await client.startMain();
      } else if (action === 'stop-player') {
        state.value.config.client.enabled = false;
        await client.stopMain();
      } else if (action === 'detect-servers') {
        const found = await invoke<Array<Record<string, string>>>('detect_lms_servers');
        postToMaterial('servers', {
          servers: (found || []).map((s) => ({
            host: s.host || '',
            port: s.port || '9000',
            name: s.name || '',
          })),
        });
      } else if (action === 'start-local') {
        await lmsLocal.startLocal();
      } else if (action === 'stop-local') {
        state.value.lmsInstall.busy = true;
        try {
          await invoke('lms_stop_local');
        } finally {
          state.value.lmsInstall.busy = false;
          await lmsLocal.refreshStatus();
        }
      } else if (action === 'restart-local') {
        state.value.lmsInstall.busy = true;
        try {
          await invoke('lms_stop_local');
          await lmsLocal.startLocal();
        } finally {
          state.value.lmsInstall.busy = false;
          await lmsLocal.refreshStatus();
        }
      } else if (action === 'activate-local') {
        await lmsLocal.activate();
      } else if (action === 'install-lms') {
        await lmsLocal.downloadAndInstall();
      } else if (action === 'open-docs') {
        await lmsLocal.openDocs();
      } else if (action === 'open-logs') {
        const local = isThisMachineHost(state.value.config.server.host);
        const href = local
          ? '/material/server.log'
          : 'http://127.0.0.1:9000/material/server.log';
        postToMaterial('open-logs', { href });
      }
    } catch (e) {
      console.log('app-action', action, e);
    }
    postToMaterial('lms-status', {
      busy: state.value.lmsInstall.busy,
      progress: state.value.lmsInstall.progress,
      error: state.value.lmsInstall.error,
      status: state.value.lmsInstall.status,
    });
    postToMaterial('config', configSnapshot());
  }
});

// Window shown/hidden from tray or focus loss
// Do NOT register Space/arrows as OS global shortcuts - that steals typing
// in Material search fields and settings. Playback keys: Material (iframe)
// + media keys below; shell onKeyDown only when not in an input.
listen('window-visibility', async (event) => {
  const visible = event.payload !== false;
  await mediaKeyShortcuts.bindVolume(visible);
});

// Dock icon preference changed from tray menu or Rust
async function onDockIconChanged(show: boolean) {
  state.value.showDockIcon = show;
  state.value.config.window.showDockIcon = show;
  if (state.value.configLoaded || !state.value.configMode) {
    await configManager.save();
  }
}
listen('dock-icon-changed', async (event) => {
  await onDockIconChanged(event.payload === true);
});
// Back-compat with older Rust event name
listen('lms-install-progress', (event) => {
  const p = event.payload as { stage?: string; percent?: number | null; message?: string };
  if (p?.message) state.value.lmsInstall.progress = p.message;
  if (p?.percent != null) state.value.lmsInstall.progressPct = p.percent;
  if (state.value.localBoot.active && p?.message && p.stage !== "error") {
    state.value.localBoot.message = p.message;
  }
  if (p?.stage === "error" && state.value.localBoot.active && !localBootWaiter) {
    state.value.localBoot.error =
      p.message ||
      "Le serveur local ne répond pas. Dans la barre des menus, ouvrez Lyrion Music Server, puis Start Service.";
    return;
  }
  if (state.value.localBoot.active) return;
  if (p?.stage === "start" && p.message) {
    messager.show(p.message, 0);
  }
  if (p?.stage === "ready" && p.message) {
    messager.show(p.message, 2800);
  }
});

listen('detached-changed', async (event) => {
  await onDockIconChanged(event.payload === true);
});

// Config Manager
let configManager = {
  configFileName: 'config.json',
  load: async () => {
    const loadedConfig = await readTextFile(configManager.configFileName, { dir: BaseDirectory.AppConfig })
      .then((data) => JSON.parse(data))
      .catch(() => false);
    
    if (loadedConfig && loadedConfig.fileSpecVersion === fileSpecVersion) {
      state.value.configLoaded = true;
      const loadedWin = loadedConfig.window || {};
      const win = {
        ...defaultWindow,
        ...loadedWin,
        // Migrate older `detached` key → showDockIcon
        showDockIcon: !!(loadedWin.showDockIcon ?? loadedWin.detached),
        globalHotkey: (loadedWin.globalHotkey as string) || DEFAULT_HOTKEY,
      };
      delete (win as { detached?: boolean }).detached;
      const loadedClient = loadedConfig.client || {};
      const client = {
        ...state.value.config.client,
        ...loadedClient,
        enabled: loadedClient.enabled !== undefined
          ? !!loadedClient.enabled
          : loadedClient.autostart !== false,
        keepOpen: !!loadedClient.keepOpen,
        name: String(loadedClient.name || ''),
      };
      const loadedServer = loadedConfig.server || {};
      const serverCfg = {
        ...state.value.config.server,
        ...loadedServer,
        defaultHost: String(loadedServer.defaultHost || loadedServer.host || ''),
        defaultPort: Number(loadedServer.defaultPort || loadedServer.port) || 9000,
      };
      if (serverCfg.defaultHost) {
        serverCfg.host = serverCfg.defaultHost;
        serverCfg.port = serverCfg.defaultPort || 9000;
      }
      state.value.config = {
        ...state.value.config,
        ...loadedConfig,
        client,
        server: serverCfg,
        window: win,
      };
      state.value.showDockIcon = !!win.showDockIcon;
      const host = String(state.value.config.server.host || "").trim();
      state.value.wizardMode = !host;
      state.value.configMode = !host;
    }
  },
  save: async () => {
    await configManager.createConfigDir();
    await writeTextFile(
      configManager.configFileName, 
      JSON.stringify(state.value.config), 
      { dir: BaseDirectory.AppConfig }
    ).catch((err) => messager.show('Unable to save configuration: ' + err));
  },
  createConfigDir: async () => {
    const configDir = await appConfigDir();
    if (!await exists(configDir)) {
      await createDir(await appConfigDir());
    }
  },
  handleSave: async () => {
    const host = (state.value.config.server.host || "").trim();
    if (!host) {
      messager.show("Enter a server host, or use Auto-detect / Install local", 3000);
      return;
    }

    // Probe is informational only. Busy LMS (pCP) can stall keep-alive while Material still loads.
    void server.validate();

    // Apply hotkey before leaving settings
    const ok = await hotkey.apply(state.value.config.window.globalHotkey);
    if (!ok) return;

    await configManager.save();
    state.value.configLoaded = true;

    if (state.value.client.needsRestart && state.value.client.running) {
      await client.restart();
      state.value.client.needsRestart = false;
    }
    
    await closeAppSettings();
  }
}

// Persist window size & position across sessions
let windowGeometry = {
  saveTimer: null as ReturnType<typeof setTimeout> | null,
  restoring: false,
  restore: async () => {
    const geom = state.value.config.window;
    if (!geom) return;

    windowGeometry.restoring = true;
    try {
      await appWindow.setSize(new LogicalSize(geom.width || defaultWindow.width, geom.height || defaultWindow.height));
      if (geom.x !== null && geom.x !== undefined && geom.y !== null && geom.y !== undefined) {
        // Keep free placement - only nudge off the absolute top of the screen
        // so the window isn't glued under the macOS menu bar (y≈0–25).
        const y = Math.max(28, Math.round(geom.y));
        const x = Math.round(geom.x);
        await appWindow.setPosition(new LogicalPosition(x, y));
        await invoke('mark_window_positioned');
      }
    } catch (err) {
      console.log('Unable to restore window geometry', err);
    } finally {
      // Allow the window to settle before listening for moves
      setTimeout(() => { windowGeometry.restoring = false; }, 250);
    }
  },
  scheduleSave: () => {
    if (windowGeometry.restoring) return;
    if (windowGeometry.saveTimer) clearTimeout(windowGeometry.saveTimer);
    windowGeometry.saveTimer = setTimeout(() => { void windowGeometry.save(); }, 350);
  },
  save: async () => {
    if (windowGeometry.restoring) return;
    try {
      const scale = await appWindow.scaleFactor();
      const size = await appWindow.innerSize();
      const pos = await appWindow.outerPosition();
      // Preserve non-geometry window prefs (dock icon, hotkey)
      state.value.config.window = {
        ...state.value.config.window,
        width: Math.max(320, Math.round(size.width / scale)),
        height: Math.max(480, Math.round(size.height / scale)),
        x: Math.round(pos.x / scale),
        y: Math.round(pos.y / scale),
      };
      await invoke('mark_window_positioned');
      // Only write geometry if we already have a saved config (avoid creating config on first resize)
      if (state.value.configLoaded || !state.value.configMode) {
        await configManager.save();
      }
    } catch (err) {
      console.log('Unable to save window geometry', err);
    }
  },
  listen: async () => {
    await appWindow.onResized(() => {
      windowGeometry.scheduleSave();
      kickPlayerFrameLayout();
    });
    await appWindow.onMoved(() => windowGeometry.scheduleSave());
  }
};

// LMS JSON-RPC playback control. Uses the player id Material reported (a MAC),
// not the display name — a name is not a playerid and the command is ignored.
let player = {
  command: async (cmd: (string | number)[]) => {
    if (!state.value.config.server.host) return;
    try {
      await invoke("lms_player_command", { cmd: cmd.map((part) => String(part)) });
    } catch (err) {
      console.log("Player command failed", cmd, err);
    }
  },
  playPause: () => player.command(['pause']),
  next: () => player.command(['playlist', 'index', '+1']),
  previous: () => player.command(['playlist', 'index', '-1']),
  volumeUp: () => player.command(['mixer', 'volume', '+5']),
  volumeDown: () => player.command(['mixer', 'volume', '-5']),
};

async function registerShortcut(shortcut: string, action: () => void): Promise<boolean> {
  try {
    const already = await isRegistered(shortcut).catch(() => false);
    if (already) {
      try { await unregister(shortcut); } catch { /* ignore */ }
    }
    await register(shortcut, action);
    return true;
  } catch (err) {
    console.log('Shortcut not registered:', shortcut, err);
    return false;
  }
}

async function unregisterShortcut(shortcut: string): Promise<void> {
  try {
    if (await isRegistered(shortcut).catch(() => false)) {
      await unregister(shortcut);
    }
  } catch {
    /* ignore */
  }
}

// Hardware media keys - always on (even when the menubar window is hidden)
// On macOS these may require Accessibility permission for the app.
let mediaKeyShortcuts = {
  enabled: false,
  volBound: false,
  // Try both camelCase (Tauri/JS) and uppercase (global-hotkey parser) forms
  handlers: [
    { shortcuts: ['MediaPlayPause', 'MEDIAPLAYPAUSE'], action: () => handleTransport('playpause') },
    { shortcuts: ['MediaTrackNext', 'MEDIATRACKNEXT'], action: () => handleTransport('next') },
    { shortcuts: ['MediaTrackPrevious', 'MEDIATRACKPREV', 'MEDIATRACKPREVIOUS'], action: () => handleTransport('previous') },
  ] as { shortcuts: string[]; action: () => void }[],
  registered: [] as string[],
  volRegistered: [] as string[],
  enable: async () => {
    if (mediaKeyShortcuts.enabled) return;
    mediaKeyShortcuts.registered = [];

    for (const { shortcuts, action } of mediaKeyShortcuts.handlers) {
      let ok = false;
      for (const name of shortcuts) {
        ok = await registerShortcut(name, action);
        if (ok) {
          mediaKeyShortcuts.registered.push(name);
          break;
        }
      }
      if (!ok) {
        console.log('Media key unavailable (try System Settings → Privacy → Accessibility):', shortcuts[0]);
      }
    }
    mediaKeyShortcuts.enabled = true;
  },
  // Volume is handled in the window (shell + Material iframe), not as a global
  // hotkey: Cmd+Up/Down must still move the caret in a search field, and must
  // not be stolen from other apps.
  bindVolume: async (_on: boolean) => {},
  disable: async () => {
    await mediaKeyShortcuts.bindVolume(false);
    for (const name of mediaKeyShortcuts.registered) {
      await unregisterShortcut(name);
    }
    mediaKeyShortcuts.registered = [];
    mediaKeyShortcuts.enabled = false;
  }
};

// Document-level keys only on the shell (config is free to type).
// Never register Space/arrows as Tauri *global* shortcuts - they block typing
// in Material (search, dialogs) and can steal keys from other apps while visible.
function onKeyDown(event: KeyboardEvent) {
  // Always allow typing in any field (settings or Material if same-doc)
  const target = event.target as HTMLElement | null;
  if (
    target &&
    (target.tagName === 'INPUT' ||
      target.tagName === 'TEXTAREA' ||
      target.tagName === 'SELECT' ||
      target.isContentEditable)
  ) {
    return;
  }
  // In app settings UI, don't hijack keys for playback
  if (state.value.configMode) return;

  const code = event.code || event.key;
  const meta = event.metaKey || event.ctrlKey;
  if (meta && (code === 'KeyR' || event.key === 'r' || event.key === 'R')) {
    event.preventDefault();
    if (hasServer()) {
      void presentServer();
    }
    return;
  }
  onTransportKey(event);
}

// Squeezelite Client Management (main + hidden preview instance)
let client = {
  startMain: async (silent: boolean = false) => {
    const already = await invoke<boolean>('squeezelite_running').catch(() => false);
    if (already) {
      state.value.client.running = true;
      return;
    }
    if (!silent) messager.show('Starting Squeezelite');
    const model = await client.getClientType();
    const mainName = state.value.config.client.name || 'Lyrion';
    try {
      const host = (state.value.config.server.host || '').trim();
      const pid = await invoke<number>('squeezelite_start', {
        name: mainName,
        model,
        server: host || null,
      });
      state.value.client.running = true;
      if (!silent) messager.show('Squeezelite process started ' + pid);
    } catch (err) {
      const missing = String(err).toLowerCase().includes('not found');
      if (!silent || !missing) {
        messager.show('Unable to start Squeezelite ' + err);
      }
      console.log(err);
      state.value.client.running = false;
    }
  },
  start: async (silent: boolean = false) => {
    await client.startMain(silent);
  },
  /** Stop a leftover second squeezelite. A future visualizer may reuse this
   *  process, but it must not register as another LMS player. */
  releasePreview: async () => {
    await client.stopPreview(true);
    await invoke("squeezelite_reap_previews").catch(() => {});
  },
  /** Kept for a later VU / visualizer. Not started: one LMS player is enough. */
  startPreview: async (silent: boolean = false) => {
    if (state.value.client.previewProcess !== null) {
      return;
    }
    // HMR / remount loses the Child handle while leftover previews keep
    // hammering LMS (CLOSE_WAIT flood on pCP).
    await invoke("squeezelite_reap_previews").catch(() => {});
    const mainName = state.value.config.client.name || 'Lyrion Minim';
    const pName = previewPlayerName(mainName);
    // Keep preview name free of special spaces that confuse argv display; use ASCII hyphen
    const safeName = pName.replace(/\s+/g, ' ').trim();
    const mac = previewMacFromName(safeName);
    state.value.client.previewName = safeName;
    state.value.client.previewMac = mac.colon;
    try {
      const model = await client.getClientType();
      // Pass MAC as one token. Prefer colon form (squeezelite docs); if argv ever
      // splits on ':', we also accept that LMS will still register a player.
      const args = ['-n', safeName, '-M', model, '-m', mac.colon];
      const host = (state.value.config.server.host || '').trim();
      if (host) {
        args.push('-s', host);
      }
      console.log('Starting preview Squeezelite', JSON.stringify(args));
      const previewCmd = Command.sidecar('binaries/squeezelite', args);
      state.value.client.previewProcess = await previewCmd.spawn();
      console.log('Preview Squeezelite started', safeName, mac.colon, state.value.client.previewProcess?.pid);
      if (!silent) {
        messager.show('Preview player ready', 1200);
      }
    } catch (err) {
      console.log('Unable to start preview Squeezelite', err);
      state.value.client.previewProcess = null;
    }
  },
  stopMain: async (silent: boolean = false) => {
    if (!silent) messager.show('Stopping Squeezelite');
    try {
      await invoke('squeezelite_stop');
    } catch (e) {
      console.log(e);
    }
    if (state.value.client.squeezeliteProcess !== null) {
      try {
        await state.value.client.squeezeliteProcess.kill();
      } catch (e) {
        console.log(e);
      }
      state.value.client.squeezeliteProcess = null;
    }
    state.value.client.running = false;
    if (!silent) messager.show('Squeezelite process stopped');
  },
  stop: async (silent: boolean = false) => {
    await client.stopPreview(silent);
    await client.stopMain(silent);
  },
  stopPreview: async (_silent: boolean = false) => {
    if (state.value.client.previewProcess !== null) {
      try {
        await state.value.client.previewProcess.kill();
      } catch (e) {
        console.log(e);
      }
      state.value.client.previewProcess = null;
    }
  },
  toggle: async (enabled: boolean) => {
    enabled ? await client.start() : await client.stop();
  },
  restart: async () => {
    messager.show('Restarting Squeezelite');
    await client.stopMain(true);
    await client.startMain(true);
    await client.releasePreview();
  },
  getClientType: async () => {
    const platformName: Platform = await platform();
    const map = {
      'linux': 'Linux',
      'darwin': 'Mac',
      'ios': 'Mac',
      'freebsd': '',
      'dragonfly': '',
      'netbsd': '',
      'openbsd': '',
      'solaris': '',
      'android': '',
      'win32': 'Windows',
    };
    return 'SqueezeLite' + (map[platformName] ?? '');
  },
  handleToggle: async (item: Event) => {
    const on = ((<HTMLInputElement>item.target).checked);
    state.value.config.client.enabled = on;
    if (on) {
      await client.startMain();
    } else {
      await client.stopMain();
    }
    return true;
  }
}

// Snackbar message state
const messageState = ref({
  snackbar: false,
  snackbarText: '',
  timeout: 1000
});
let messager = {
  show: (text: string, timeout: number = 1000) => {
    messager.hide();
    messageState.value.timeout = timeout === 0 ? -1 : timeout;
    messageState.value.snackbarText = text;
    messageState.value.snackbar = true;
  },
  hide: () => {
    messageState.value.snackbarText = '';
    messageState.value.snackbar = false;
  }
}

// Local LMS install / start (turnkey)
const lmsLocal = {
  refreshStatus: async () => {
    try {
      // Local status is cheap (port probe + path check)
      state.value.lmsInstall.status = await invoke("lms_install_status");
      // Channels/package scrape lyrion.org — only refresh when panel needs it / empty
      if (!state.value.lmsInstall.channels?.length) {
        try {
          state.value.lmsInstall.channels = await invoke("lms_channels");
        } catch (_) {
          state.value.lmsInstall.channels = [
            { id: "latest", label: "Latest release", description: "Official stable", recommended: true, version: null },
            { id: "stable", label: "Stable nightly", description: "Bug-fix branch", recommended: false, version: null },
            { id: "dev", label: "Development", description: "Newest features", recommended: false, version: null },
          ];
        }
      }
      if (!state.value.lmsInstall.package) {
        await lmsLocal.refreshPackage();
      }
    } catch (e) {
      console.log("lms status", e);
    }
  },
  refreshPackage: async () => {
    try {
      state.value.lmsInstall.package = await invoke("lms_package_info", {
        channel: state.value.lmsInstall.channel || "latest",
        version: null,
      });
      state.value.lmsInstall.error = "";
    } catch (e: any) {
      state.value.lmsInstall.package = null;
      state.value.lmsInstall.error = String(e?.message || e || "Could not resolve package");
    }
  },
  onChannelChange: async () => {
    state.value.lmsInstall.progress = "";
    await lmsLocal.refreshPackage();
  },
  downloadAndInstall: async () => {
    state.value.lmsInstall.busy = true;
    state.value.lmsInstall.error = "";
    state.value.lmsInstall.progress = "Starting download…";
    try {
      const info = await invoke<{ version: string; channelLabel?: string }>("lms_download_and_install", {
        channel: state.value.lmsInstall.channel || "latest",
        version: null,
      });
      messager.show("Installer launched — complete setup, then Start if needed", 4000);
      state.value.lmsInstall.progress =
        "Installer launched" +
        (info?.channelLabel ? " (" + info.channelLabel + ")" : "") +
        (info?.version ? " v" + info.version : "");
      // Poll local server after user finishes installer
      const ready = await invoke<boolean>("lms_wait_ready", {
        host: "127.0.0.1",
        port: 9000,
        timeoutSecs: 120,
      });
      if (ready) {
        state.value.config.server.host = "127.0.0.1";
        state.value.config.server.port = 9000;
        state.value.config.server.autodetect = false;
        messager.show("Local Lyrion Music Server is ready", 3000);
        state.value.lmsInstall.progress = "Server ready on 127.0.0.1:9000";
      } else {
        state.value.lmsInstall.progress =
          "Waiting for server… On macOS use menu bar → Start Service after installing.";
      }
      await lmsLocal.refreshStatus();
    } catch (e: any) {
      state.value.lmsInstall.error = String(e?.message || e || "Install failed");
      messager.show(state.value.lmsInstall.error, 5000);
    } finally {
      state.value.lmsInstall.busy = false;
    }
  },
  startLocal: async () => {
    state.value.lmsInstall.busy = true;
    state.value.lmsInstall.error = "";
    try {
      const st = await invoke<{
        serverRunning: boolean;
        host: string;
        port: number;
        message: string;
      }>("lms_start_local");
      state.value.lmsInstall.status = st as any;
      if (st?.serverRunning) {
        state.value.config.server.host = st.host || "127.0.0.1";
        state.value.config.server.port = st.port || 9000;
        state.value.config.server.autodetect = false;
        messager.show("Local server is running", 2500);
      } else {
        messager.show(st?.message || "Server not ready yet", 4000);
      }
    } catch (e: any) {
      state.value.lmsInstall.error = String(e?.message || e || "Start failed");
      messager.show(state.value.lmsInstall.error, 5000);
    } finally {
      state.value.lmsInstall.busy = false;
      await lmsLocal.refreshStatus();
    }
  },
  activate: async () => {
    await lmsLocal.refreshStatus();
    const st = state.value.lmsInstall.status;
    if (!st?.appInstalled) {
      await lmsLocal.downloadAndInstall();
    }
    const now = state.value.lmsInstall.status;
    state.value.config.server.host = now?.host || "127.0.0.1";
    state.value.config.server.port = now?.port || 9000;
    state.value.config.server.autodetect = false;
    await invoke("set_current_lms", {
      host: state.value.config.server.host,
      port: Number(state.value.config.server.port) || 9000,
    }).catch(() => {});
    await configManager.save();
    await notifyLmsTarget();
    const result = await presentServer(true);
    if (result !== "failed") {
      messager.show("Serveur local", 2000);
    }
  },
  useLocalhost: () => {
    state.value.config.server.host = "127.0.0.1";
    state.value.config.server.port = 9000;
    state.value.config.server.autodetect = false;
    messager.show("Using 127.0.0.1:9000", 1500);
  },
  openDocs: async () => {
    try {
      await invoke("lms_open_getting_started");
    } catch (e) {
      console.log(e);
    }
  },
};

// Manage everything to do with the LMS server
let server = {
  detect: async () => {
    if (!state.value.config.server.autodetect) return;
    state.value.server.loading = true;
    messager.show('Autodetecting server');
    // Prefer local install if already running
    try {
      const local = await invoke<{ serverRunning: boolean; host: string; port: number }>("lms_install_status");
      if (local?.serverRunning) {
        state.value.config.server.host = local.host || "127.0.0.1";
        state.value.config.server.port = local.port || 9000;
        state.value.server.loading = false;
        messager.show("Found local Lyrion Music Server", 1500);
        await lmsLocal.refreshStatus();
        return;
      }
    } catch (_) { /* fall through to UDP discovery */ }
    let server: { host: string, port: number } = await invoke('detect_lms_server');
    if (!server?.host) {
      messager.show('No server found — install locally or enter host manually', 3000);
      await lmsLocal.refreshStatus();
      state.value.server.loading = false;
      return;
    }
    state.value.config.server.host = server.host;
    state.value.config.server.port = server.port ?? 9000;
    state.value.server.loading = false;
  },
  validate: async () => {
    // Prefer Rust/reqwest probe — Tauri http.fetch often false-negatives on LAN IPs
    // even when LMS is fine (and the iframe loads).
    const host = String(state.value.config.server.host || "").trim();
    const port = Number(state.value.config.server.port) || 9000;
    if (!host) return false;
    try {
      const r = await invoke<{ ok: boolean; message?: string; url?: string }>("lms_probe_server", {
        host,
        port,
      });
      if (r?.ok) {
        console.log("LMS probe ok:", r.url || r.message);
        return true;
      }
      console.warn("LMS probe failed:", r?.message);
    } catch (e) {
      console.warn("LMS probe invoke failed", e);
    }
    // Last resort: Tauri HTTP (may still fail spuriously)
    const base = server.getBaseUri();
    try {
      const response = await http.fetch(base + "/", { method: "GET", timeout: 5 });
      if (response.status >= 200 && response.status < 400) return true;
    } catch (e1) {
      console.log("validate tauri http failed", base, e1);
    }
    try {
      const root = "http://" + host + ":" + port + "/";
      const response = await http.fetch(root, { method: "GET", timeout: 5 });
      return response.status >= 200 && response.status < 400;
    } catch (e2) {
      console.log("validate root failed", e2);
      return false;
    }
  },
  showPlayer: async () => {
    const host = (state.value.config.server.host || "").trim();
    if (!host) {
      state.value.iframeError = "";
      state.value.wizardMode = true;
      state.value.configMode = true;
      return;
    }
    // Do NOT block iframe load on Tauri http.validate — webview can reach LMS even when
    // the HTTP allowlist rejects the probe. Still log a soft warning.
    server.validate().then((ok) => {
      if (!ok) {
        console.warn("Material HTTP probe failed (iframe will still try):", server.getBaseUri());
      }
    });
    const uri = server.getUri();
    console.log("Loading Material:", uri);
    clearIframeLoadTimer();
    state.value.iframeError = '';
    state.value.iframeLoading = true;
    state.value.wizardMode = false;
    state.value.configMode = false;
    void appWindow.show().then(() => appWindow.setFocus()).catch(() => {});
    // Bump :key so WKWebView cannot keep the previous Material/cometd document
    // when only the proxied LMS host changed (same http://127.0.0.1:1420/material).
    state.value.iframeGen += 1;
    state.value.iframeSrc = uri;
    iframeLoadTimer = setTimeout(() => {
      if (state.value.iframeSrc === uri && !state.value.configMode && state.value.iframeLoading) {
        state.value.iframeLoading = false;
        // Snackbar only: a full-screen overlay hid a working player when onload never fired.
        messager.show("Still waiting on Material at " + server.getBaseUri(), 3500);
      }
    }, 20000);
  },
  getBaseUri: () => {
    // Lowercase /material is the canonical path; /Material redirects
    const host = String(state.value.config.server.host || "").trim();
    const port = String(state.value.config.server.port || "9000").trim() || "9000";
    return "http://" + host + ":" + port + "/material";
  },
  /** Iframe origin. Same origin as this window so WKWebView lays Material out.
   *  Dev: Vite proxies /material. Release: the shell proxy on 127.0.0.1 does. */
  getIframeBase: () => {
    if (typeof window !== "undefined" && window.location?.origin && window.location.origin !== "null") {
      const origin = window.location.origin;
      if (origin.startsWith("http://127.0.0.1") || origin.startsWith("http://localhost")) {
        return origin + "/material";
      }
    }
    return server.getBaseUri();
  },
  getUri: () => {
    // Full Material shell (~15KB HTML). Do not use /now-playing path alone -
    // that only returns a tiny client-side redirect stub (~600B).
    //
    // Dev iframe is same-origin via Vite /material proxy (RecycleScroller).
    // css/js=msk--minim: Lyrion chrome (traffic-light inset, opaque lists).
    const player = encodeURIComponent(state.value.config.client.name || "");
    const r = String(Date.now());
    const lmsHost = encodeURIComponent(String(state.value.config.server.host || "").trim());
    const lmsPort = encodeURIComponent(String(state.value.config.server.port || "9000"));
    let uri =
      server.getIframeBase()
      + "/?player=" + player
      + "&css=msk--minim"
      + "&js=msk--minim"
      + "&appSettings=minim-app-settings"
      + "&lmsHost=" + lmsHost
      + "&lmsPort=" + lmsPort
      + "&r=" + r;
    // No previewPlayer: that id was a second squeezelite we do not start.
    // Material then previews on this Mac's player and puts its queue back after.
    return uri;
  }
}

// Show / hide Dock icon (free-floating vs menubar popup)
let dockIcon = {
  apply: async (show: boolean, persist: boolean = true) => {
    try {
      await invoke('set_show_dock_icon', { show });
      state.value.showDockIcon = show;
      state.value.config.window.showDockIcon = show;
      if (persist && (state.value.configLoaded || !state.value.configMode)) {
        await configManager.save();
      }
      messager.show(show ? 'Dock icon shown' : 'Menu bar only', 1500);
    } catch (err) {
      console.log('set_show_dock_icon failed', err);
    }
  },
  restore: async () => {
    const want = !!state.value.config.window.showDockIcon;
    if (want) {
      await dockIcon.apply(true, false);
    } else {
      state.value.showDockIcon = await invoke<boolean>('is_show_dock_icon').catch(() => false);
    }
  }
};

// Global show/hide hotkey (registered in Rust)
let hotkey = {
  apply: async (accelerator: string): Promise<boolean> => {
    const accel = (accelerator || DEFAULT_HOTKEY).trim();
    state.value.hotkeyError = '';
    try {
      const registered = await invoke<string>('set_toggle_hotkey', { accelerator: accel });
      state.value.config.window.globalHotkey = registered;
      return true;
    } catch (err) {
      const msg = String(err);
      state.value.hotkeyError = msg;
      messager.show('Hotkey failed: ' + msg, 3000);
      return false;
    }
  },
  restore: async () => {
    const want = state.value.config.window.globalHotkey || DEFAULT_HOTKEY;
    await hotkey.apply(want);
  }
};

async function init() {

  console.log('Init');
  state.value.version = await getVersion();
  await configManager.load();
  await windowGeometry.restore();
  await windowGeometry.listen();
  window.addEventListener('keydown', onKeyDown);
  window.addEventListener('resize', kickPlayerFrameLayout);

  // Restore Dock first so hide-on-blur cannot swallow the window before Material paints.
  void dockIcon.restore();
  void hotkey.restore();
  void appWindow.show().then(() => appWindow.setFocus()).catch(() => {});

  try {
    state.value.thisMachineHosts = await invoke<string[]>('this_machine_hosts');
  } catch {
    state.value.thisMachineHosts = ['127.0.0.1', 'localhost'];
  }

  if (!state.value.config.client.name) {
    try {
      const n = await invoke<string>('lms_machine_name');
      if (n) state.value.config.client.name = n;
    } catch {
      state.value.config.client.name = 'Lyrion';
    }
  }

  const host = String(state.value.config.server.host || '').trim();
  const port = Number(state.value.config.server.port) || 9000;
  if (host) {
    void invoke('set_current_lms', { host, port }).catch(() => {});
    const defHost = String(state.value.config.server.defaultHost || host || '').trim();
    const defPort = Number(state.value.config.server.defaultPort) || port;
    if (defHost) {
      void invoke('set_default_lms', { host: defHost, port: defPort }).catch(() => {});
    }
    state.value.wizardMode = false;
    const opened = await presentServer(isThisMachineHost(host));
    if (opened === "up" || opened === "remote") {
      state.value.bootSplash = true;
      setTimeout(() => { state.value.bootSplash = false; }, 420);
    }
    if (state.value.config.client.enabled !== false) {
      void client.startMain(true);
    } else {
      void client.stopMain(true);
    }
    void client.releasePreview();
  } else {
    state.value.wizardMode = true;
    state.value.configMode = true;
    void invoke('set_current_lms', { host: '', port: 9000 }).catch(() => {});
  }

  // Media keys + settings metadata: never block the player iframe.
  void mediaKeyShortcuts.enable();
  try {
    await appWindow.onFocusChanged(({ payload: focused }) => {
      void mediaKeyShortcuts.bindVolume(!!focused);
    });
  } catch {
    /* ignore */
  }
  void lmsLocal.refreshStatus();
}

init();
</script>

<template>
  <!--
    Drag region in the *parent* webview (required for Tauri Overlay title bars).
    Must NOT cover Material's menu, player name, or volume slider.
    Its box is measured from #main-toolbar .drag-area (the empty gap).
  -->
  <div
    v-show="!state.configMode && !state.wizardMode && !state.localBoot.active"
    class="titlebar-drag"
    data-tauri-drag-region
    aria-hidden="true"
    title="Drag to move"
  ></div>

  <iframe
    class="player-frame"
    :key="state.iframeGen"
    :src="state.iframeSrc"
    scrolling="yes"
    frameborder="0"
    allow="autoplay"
    referrerpolicy="no-referrer-when-downgrade"
    v-show="!state.configMode && !state.wizardMode"
    @load="onPlayerFrameLoad"
  ></iframe>

  <!-- Soft error when Material iframe fails / times out (iframe has no reliable onerror) -->
  <div
    v-if="!state.configMode && !state.wizardMode && state.iframeError"
    class="iframe-error"
    role="alert"
  >
    <div class="iframe-error-card">
      <div class="iframe-error-title">Couldn’t load player</div>
      <div class="iframe-error-body">{{ state.iframeError }}</div>
      <div class="iframe-error-actions">
        <v-btn color="primary" variant="flat" size="small" @click="retryPlayerLoad">
          Retry
        </v-btn>
        <v-btn variant="text" size="small" class="text-medium-emphasis" @click="openAppSettings">
          Settings
        </v-btn>
      </div>
    </div>
  </div>

  <div v-if="state.localBoot.active" class="local-boot" role="status">
    <div class="local-boot-card">
      <div
        v-if="!state.localBoot.error && state.localBoot.message !== 'Serveur prêt'"
        class="local-boot-spinner"
        aria-hidden="true"
      ></div>
      <div class="local-boot-title">
        {{
          state.localBoot.error
            ? "Le serveur local n'a pas démarré"
            : state.localBoot.message === "Serveur prêt"
              ? "Serveur prêt"
              : "Démarrage du serveur local…"
        }}
      </div>
      <div
        v-if="state.localBoot.error || (state.localBoot.message && state.localBoot.message !== 'Démarrage du serveur local…' && state.localBoot.message !== 'Serveur prêt')"
        class="local-boot-body"
      >
        {{ state.localBoot.error || state.localBoot.message }}
      </div>
      <v-btn
        v-if="state.localBoot.error"
        color="primary"
        variant="flat"
        size="small"
        @click="retryLocalBoot"
      >
        Réessayer
      </v-btn>
    </div>
  </div>

  <transition name="boot-splash">
    <div v-if="state.bootSplash && !state.configMode && !state.wizardMode" class="boot-splash" aria-hidden="true">
      <img src="/images/lyrion-logo.png" alt="" />
    </div>
  </transition>

  <Wizard
    v-if="state.wizardMode"
    :version="state.version"
    :skip-splash="state.configLoaded"
    @connect="onWizardConnect"
  />

  <v-container id="config" class="config-root" fluid v-show="state.configMode && !state.wizardMode">
    <v-sheet class="config-sheet pa-5 text-white mx-auto">

      <header class="config-header mb-5">
        <div class="config-brand">
          <img class="config-logo" src="/images/lyrion-logo.png" alt="" />
          <div>
            <div class="config-title">Lyrion</div>
            <div class="config-subtitle">Application settings · v{{ state.version }}</div>
          </div>
        </div>
        <v-btn
          v-if="state.configLoaded"
          variant="text"
          size="small"
          class="text-medium-emphasis"
          @click="closeAppSettings"
        >
          Back to player
        </v-btn>
      </header>

      <!-- Display -->
      <section class="config-section mb-4">
        <div class="config-section-title">Display</div>
        <div class="config-section-desc">How Lyrion appears on your desktop</div>
        <v-switch
          class="mt-3"
          :model-value="state.showDockIcon"
          @update:model-value="(v: boolean | null) => dockIcon.apply(!!v)"
          label="Show Dock icon"
          color="primary"
          hint="When enabled, Lyrion stays open and can be dragged freely by the title bar"
          persistent-hint
          density="comfortable"
        ></v-switch>
        <div class="hotkey-block mt-4">
          <div class="hotkey-label">Global show / hide hotkey</div>
          <div class="hotkey-preview" aria-live="polite">
            <kbd class="hotkey-kbd">{{ hotkeyDisplay }}</kbd>
          </div>
          <v-select
            class="mt-3"
            label="Preset"
            :items="hotkeyPresets"
            item-title="label"
            item-value="value"
            v-model="state.config.window.globalHotkey"
            variant="outlined"
            density="comfortable"
            hide-details
          ></v-select>
        </div>
      </section>

      <!-- Local player -->
      <section class="config-section mb-4">
        <div class="config-section-title">Local player</div>
        <div class="config-section-desc">One Squeezelite on this computer.</div>
        <v-row dense class="mt-2">
          <v-col cols="12" sm="6">
            <v-switch
              v-model="state.client.running"
              @change="client.handleToggle"
              label="Enable"
              color="primary"
              hide-details
              density="comfortable"
            ></v-switch>
          </v-col>
          <v-col cols="12" sm="6">
            <v-switch
              v-model="state.config.client.keepOpen"
              label="Keep open when the application is closed"
              color="primary"
              hide-details
              density="comfortable"
            ></v-switch>
          </v-col>
          <v-col cols="12">
            <v-text-field
              label="Player name"
              :placeholder="state.config.client.name || 'This computer'"
              v-model="state.config.client.name"
              v-on:update:model-value="state.client.needsRestart = true"
              hint="How this client appears in Lyrion Music Server"
              persistent-hint
              variant="outlined"
              density="comfortable"
            ></v-text-field>
          </v-col>
        </v-row>
      </section>

      <!-- Server -->
      <section class="config-section mb-4" :class="{ 'config-section--loading': state.server.loading }">
        <div class="config-section-title">Server</div>
        <div class="config-section-desc">Discover a network LMS or install one on this computer</div>
        <v-row dense class="mt-2">
          <v-col cols="12">
            <v-switch
              v-model="state.config.server.autodetect"
              @change="server.detect"
              label="Auto-detect on network"
              color="primary"
              hide-details
              density="comfortable"
              :disabled="state.server.loading"
            ></v-switch>
          </v-col>
          <v-col cols="12" sm="8">
            <v-text-field
              :disabled="state.config.server.autodetect"
              label="Host"
              placeholder="192.168.1.4"
              v-model="state.config.server.host"
              hint="Hostname or IP address"
              persistent-hint
              variant="outlined"
              density="comfortable"
            ></v-text-field>
          </v-col>
          <v-col cols="12" sm="4">
            <v-text-field
              :disabled="state.config.server.autodetect"
              label="Port"
              placeholder="9000"
              v-model="state.config.server.port"
              hint="Usually 9000"
              persistent-hint
              variant="outlined"
              density="comfortable"
            ></v-text-field>
          </v-col>
        </v-row>
      </section>

      <!-- Local LMS (turnkey install) — always available -->
      <section class="config-section mb-4">
        <div class="config-section-title">Install Lyrion Music Server</div>
        <div class="config-section-desc">
          Official packages from
          <a class="config-link" href="#" @click.prevent="lmsLocal.openDocs">lyrion.org</a>
          · choose release channel, always available here
        </div>

        <p v-if="state.lmsInstall.status" class="lms-status text-caption mt-3 mb-2">
          {{ state.lmsInstall.status.message }}
          <span v-if="state.lmsInstall.status.serverRunning" class="lms-pill lms-pill--ok">running</span>
          <span v-else-if="state.lmsInstall.status.appInstalled" class="lms-pill">installed</span>
          <span v-else class="lms-pill lms-pill--muted">not found</span>
        </p>

        <v-select
          class="mt-2 mb-1"
          label="Release channel"
          :items="lmsChannelItems"
          item-title="title"
          item-value="value"
          v-model="state.lmsInstall.channel"
          @update:model-value="lmsLocal.onChannelChange"
          :disabled="state.lmsInstall.busy"
          variant="outlined"
          density="comfortable"
          hide-details
        ></v-select>
        <p class="text-caption text-medium-emphasis mb-3">
          {{ lmsChannelHint }}
        </p>

        <p v-if="state.lmsInstall.package" class="text-caption text-medium-emphasis mb-3">
          <template v-if="state.lmsInstall.package.channelLabel">
            {{ state.lmsInstall.package.channelLabel }} ·
          </template>
          v{{ state.lmsInstall.package.version }} ·
          {{ state.lmsInstall.package.filename }}
          <template v-if="state.lmsInstall.package.sizeHintMb">
            (≈{{ state.lmsInstall.package.sizeHintMb }}&nbsp;MB)
          </template>
        </p>

        <v-progress-linear
          v-if="state.lmsInstall.busy && state.lmsInstall.progressPct != null"
          :model-value="state.lmsInstall.progressPct"
          color="primary"
          height="4"
          class="mb-2"
        ></v-progress-linear>
        <p v-if="state.lmsInstall.progress" class="text-caption mb-2">{{ state.lmsInstall.progress }}</p>
        <p v-if="state.lmsInstall.error" class="text-caption text-error mb-2">{{ state.lmsInstall.error }}</p>

        <div class="lms-actions d-flex flex-wrap ga-2">
          <v-btn
            size="small"
            variant="flat"
            color="primary"
            :loading="state.lmsInstall.busy"
            :disabled="state.lmsInstall.busy"
            @click="lmsLocal.downloadAndInstall"
          >
            Download &amp; install
          </v-btn>
          <v-btn
            size="small"
            variant="tonal"
            :loading="state.lmsInstall.busy"
            :disabled="state.lmsInstall.busy"
            @click="lmsLocal.startLocal"
          >
            Start local server
          </v-btn>
          <v-btn
            size="small"
            variant="text"
            :disabled="state.lmsInstall.busy"
            @click="lmsLocal.useLocalhost"
          >
            Use 127.0.0.1
          </v-btn>
          <v-btn
            size="small"
            variant="text"
            :disabled="state.lmsInstall.busy"
            @click="lmsLocal.refreshStatus"
          >
            Refresh status
          </v-btn>
        </div>
        <p v-if="state.lmsInstall.package?.notes" class="text-caption text-medium-emphasis mt-3 mb-0">
          {{ state.lmsInstall.package.notes }}
        </p>
      </section>

      <v-btn
        size="large"
        variant="flat"
        color="primary"
        block
        class="config-save"
        @click="configManager.handleSave"
      >
        Save and continue
      </v-btn>

    </v-sheet>
  </v-container>

  <v-snackbar v-model="messageState.snackbar" :timeout="messageState.timeout">{{messageState.snackbarText}}</v-snackbar>

</template>

<style>
html, body, #app {
  margin: 0;
  height: 100%;
  overflow: hidden;
  scrollbar-width: none;
  -ms-overflow-style: none;
  /* Solid, same as the window chrome. Transparent showed WKWebView's default
     white on first launch and whenever Material had not painted yet. */
  background: #1c1c1e !important;
  font-family: -apple-system, BlinkMacSystemFont, "SF Pro Text", system-ui, sans-serif;
  position: relative;
  /* Square top so the toolbar sits flush with the window. Bottom stays rounded. */
  border-radius: 0 0 12px 12px;
}

/* Soft hairline like native HUD / menubar panels */
#app {
  box-shadow: inset 0 0 0 0.5px rgba(255, 255, 255, 0.14);
}

html, body, #app, #config {
  scrollbar-width: none;
  -ms-overflow-style: none;
}
html::-webkit-scrollbar,
body::-webkit-scrollbar,
#app::-webkit-scrollbar,
#config::-webkit-scrollbar {
  display: none;
  width: 0;
  height: 0;
  background: transparent;
}

html::-webkit-scrollbar,
body::-webkit-scrollbar,
#app::-webkit-scrollbar,
iframe.player-frame::-webkit-scrollbar,
#config::-webkit-scrollbar {
  width: 0;
  height: 0;
  background: transparent;
}

iframe.player-frame, #config {
  width: 100%;
  height: 100%;
  border: none;
  padding: 0;
  margin: 0;
  /* overflow:hidden + %-height makes WKWebView report innerHeight=0 to
     Material's RecycleScroller — Accueil stays on the 3-dot loader. */
  overflow: auto;
  scrollbar-width: none;
  -ms-overflow-style: none;
  background: #1c1c1e;
  position: absolute;
  inset: 0;
  z-index: 1;
  border-radius: 0 0 12px 12px;
}

.iframe-error {
  position: absolute;
  inset: 0;
  z-index: 20;
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 24px;
  background: rgba(28, 28, 30, 0.92);
  box-sizing: border-box;
}

.iframe-error-card {
  max-width: 360px;
  text-align: center;
  color: #f5f5f7;
}

.iframe-error-title {
  font-size: 1.1rem;
  font-weight: 650;
  letter-spacing: -0.02em;
  margin-bottom: 8px;
}

.iframe-error-body {
  font-size: 0.9rem;
  line-height: 1.45;
  color: rgba(235, 235, 245, 0.65);
  margin-bottom: 18px;
}

.iframe-error-actions {
  display: flex;
  gap: 8px;
  justify-content: center;
  flex-wrap: wrap;
}

/*
 * Title-bar drag hit target. The iframe cannot drag the window, so this overlay
 * tracks #main-toolbar .drag-area (the empty gap). It stays at width 0 until
 * that gap is measured.
 */
.titlebar-drag {
  position: absolute;
  top: 0;
  left: 0;
  width: 0;
  height: 52px;
  z-index: 10;
  -webkit-app-region: drag;
  app-region: drag;
  cursor: default;
  background: transparent;
  pointer-events: none;
}

.local-boot {
  position: absolute;
  inset: 0;
  z-index: 40;
  display: flex;
  align-items: center;
  justify-content: center;
  background: #1c1c1e;
  color: #f5f5f7;
}

.local-boot-card {
  max-width: 380px;
  text-align: center;
  padding: 24px;
}

.local-boot-spinner {
  width: 28px;
  height: 28px;
  margin: 0 auto 16px;
  border-radius: 50%;
  border: 2px solid rgba(255, 255, 255, 0.18);
  border-top-color: #f5f5f7;
  animation: local-boot-spin 0.8s linear infinite;
}

@keyframes local-boot-spin {
  to { transform: rotate(360deg); }
}

.local-boot-title {
  font-size: 1.05rem;
  font-weight: 650;
  letter-spacing: -0.02em;
  margin-bottom: 8px;
}

.local-boot-body {
  font-size: 0.9rem;
  line-height: 1.45;
  color: rgba(235, 235, 245, 0.65);
  margin-bottom: 18px;
}

/* -- Settings layout -- */
.config-root {
  overflow: auto !important;
  height: 100%;
  background: #1c1c1e !important;
  max-width: none !important;
}

.config-sheet {
  max-width: 520px;
  min-height: 100%;
  background: transparent !important;
  box-sizing: border-box;
}

.config-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding-bottom: 16px;
  border-bottom: 1px solid rgba(255, 255, 255, 0.08);
}

.config-brand {
  display: flex;
  align-items: center;
  gap: 12px;
  min-width: 0;
}

.config-logo {
  width: 40px;
  height: 40px;
  border-radius: 10px;
  object-fit: contain;
  background: rgba(255, 255, 255, 0.06);
}

.config-title {
  font-size: 1.25rem;
  font-weight: 700;
  letter-spacing: -0.02em;
  line-height: 1.2;
}

.config-subtitle {
  font-size: 0.8rem;
  color: rgba(235, 235, 245, 0.55);
  margin-top: 2px;
}

.config-section {
  padding: 16px 16px 8px;
  border-radius: 14px;
  background: rgba(44, 44, 46, 0.55);
  border: 1px solid rgba(255, 255, 255, 0.06);
}

.config-section-title {
  font-size: 0.95rem;
  font-weight: 600;
  letter-spacing: -0.01em;
}

.config-section-desc {
  font-size: 0.75rem;
  color: rgba(235, 235, 245, 0.5);
  margin-top: 2px;
}

.config-link {
  color: #64b5f6;
  text-decoration: none;
}
.config-link:hover {
  text-decoration: underline;
}
.lms-status {
  color: rgba(235, 235, 245, 0.75);
  line-height: 1.4;
}
.lms-pill {
  display: inline-block;
  margin-left: 6px;
  padding: 1px 7px;
  border-radius: 999px;
  font-size: 0.7rem;
  font-weight: 600;
  letter-spacing: 0.02em;
  background: rgba(255, 255, 255, 0.12);
  color: rgba(245, 245, 247, 0.9);
  vertical-align: middle;
}
.lms-pill--ok {
  background: rgba(52, 199, 89, 0.25);
  color: #34c759;
}
.lms-pill--muted {
  background: rgba(255, 255, 255, 0.08);
  color: rgba(235, 235, 245, 0.45);
}
.lms-actions {
  gap: 8px;
}
.hotkey-block {
  padding: 12px 0 4px;
  border-top: 1px solid rgba(255, 255, 255, 0.06);
}

.hotkey-label {
  font-size: 0.8rem;
  font-weight: 500;
  color: rgba(235, 235, 245, 0.75);
  margin-bottom: 8px;
}

.hotkey-preview {
  display: flex;
  align-items: center;
  gap: 8px;
}

.hotkey-kbd {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  min-height: 36px;
  padding: 6px 14px;
  border-radius: 8px;
  font: 600 1.05rem/1.2 -apple-system, BlinkMacSystemFont, system-ui, sans-serif;
  letter-spacing: 0.04em;
  color: #f5f5f7;
  background: rgba(0, 0, 0, 0.35);
  border: 1px solid rgba(255, 255, 255, 0.12);
  box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.06);
}

.hotkey-help {
  line-height: 1.4;
}

.config-save {
  font-weight: 600 !important;
  letter-spacing: 0.01em;
  text-transform: none !important;
  height: 48px !important;
  border-radius: 12px !important;
}

.boot-splash {
  position: absolute;
  inset: 0;
  z-index: 25;
  display: flex;
  align-items: center;
  justify-content: center;
  background: #1c1c1e;
}
.boot-splash img {
  width: 64px;
  height: 64px;
  border-radius: 14px;
}
.boot-splash-leave-active {
  transition: opacity 0.35s cubic-bezier(0.22, 1, 0.36, 1);
}
.boot-splash-leave-to { opacity: 0; }
</style>
