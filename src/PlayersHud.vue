<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref } from "vue";
import { emit, listen, UnlistenFn } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/tauri";
import { http } from "@tauri-apps/api";
import { Body } from "@tauri-apps/api/http";
import { readTextFile, BaseDirectory } from "@tauri-apps/api/fs";
import { appWindow, LogicalSize } from "@tauri-apps/api/window";
import { platform } from "@tauri-apps/api/os";

type HudPlayer = {
  id: string;
  name: string;
  track: string;
  artist: string;
  cover: string;
  volume: number;
  muted: boolean;
  isplaying: boolean;
  waiting: boolean;
  ison: boolean;
  isgroup: boolean;
  hasTrack: boolean;
  weight: number;
  syncmaster: string;
  synced: boolean;
  sleepUntil: number;
  alarmAt: number;
  shuffle: number;
  repeat: number;
  icon: string;
  svg: string;
};

type MenuPane = "root" | "sleep" | "sync";

const HUD_WIDTH = 360;
const ROW_H = 44;
const SEP_H = 22;
const PAD = 10;
const IDLE_MS = 3800;
const POLL_VISIBLE = 8000;
const POLL_HIDDEN = 30000;
const LONG_MS = 550;

const players = ref<HudPlayer[]>([]);
const materialOrder = ref<string[]>([]);
const currentId = ref("");
const fading = ref(false);
const loading = ref(true);
const error = ref("");
const hudVisible = ref(false);
const kbNav = ref(false);
const menu = ref<{
  show: boolean;
  x: number;
  y: number;
  player: HudPlayer | null;
  pane: MenuPane;
}>({ show: false, x: 0, y: 0, player: null, pane: "root" });

const toast = ref("");
const swipingId = ref("");
const dragId = ref("");
const dropId = ref("");
const dragX = ref(0);
const dragY = ref(0);
const volDragId = ref("");
const dragPlayer = computed(() => (dragId.value ? playerById(dragId.value) : undefined));
const hudPlat = ref<"mac" | "win" | "linux">("mac");

if (typeof navigator !== "undefined" && /Mac OS X 10_15/.test(navigator.userAgent)) {
  document.documentElement.classList.add("os-catalina");
}

let host = "";
let port = 9000;
let pollTimer: ReturnType<typeof setInterval> | null = null;
let idleTimer: ReturnType<typeof setTimeout> | null = null;
let fadeTimer: ReturnType<typeof setTimeout> | null = null;
let longTimer: ReturnType<typeof setTimeout> | null = null;
let unlisteners: UnlistenFn[] = [];
let alive = true;
let hovered = false;
let userSized = false;
let fitting = false;
let longFired = false;
let skipClick = false;
let press = { x: 0, y: 0, id: "", moved: false, swipe: false, lift: false };
let refreshGen = 0;
let mergeRefreshTimer: ReturnType<typeof setTimeout> | null = null;
let tickTimer: ReturnType<typeof setInterval> | null = null;
const nowTick = ref(Date.now());
const volDragging = new Set<string>();
const volHold = new Map<string, { v: number; until: number }>();
const volGen = new Map<string, number>();
const volLive = ref<Record<string, number>>({});
const coverCache = new Map<string, string>();
const iconInk = ref(
  typeof window !== "undefined" && window.matchMedia("(prefers-color-scheme: light)").matches ? "1d1d1f" : "edece7"
);
type IconSpec = { icon?: string; svg?: string; ends?: string; begins?: string };
const playerIconModels = new Map<string, Record<string, unknown>>();
const playerIconById = new Map<string, IconSpec>();
let iconsReady = false;

const displayPlayers = computed(() => {
  const list = players.value.slice();
  const order = materialOrder.value;
  if (order.length) {
    const idx = new Map(order.map((id, i) => [id, i]));
    list.sort((a, b) => {
      if (a.isgroup !== b.isgroup) return a.isgroup ? 1 : -1;
      const ia = idx.has(a.id) ? (idx.get(a.id) as number) : 1000;
      const ib = idx.has(b.id) ? (idx.get(b.id) as number) : 1000;
      if (ia !== ib) return ia - ib;
      return a.name.localeCompare(b.name, undefined, { sensitivity: "base" });
    });
    return list.filter((p) => idx.has(p.id) || !order.length);
  }
  list.sort((a, b) => {
    if (a.isgroup !== b.isgroup) return a.isgroup ? 1 : -1;
    if (a.weight !== b.weight) return a.weight - b.weight;
    return a.name.localeCompare(b.name, undefined, { sensitivity: "base" });
  });
  return list;
});

type Nest = "solo" | "master" | "mid" | "last";

type SlaveRow = { player: HudPlayer; nest: "mid" | "last" };
type HudBlock = { player: HudPlayer; nest: "solo" | "master"; slaves: SlaveRow[] };
type HudSection = { id: "players" | "groups"; label: string; blocks: HudBlock[] };

function groupKey(p: HudPlayer) {
  const master = idKey(p.syncmaster);
  const self = idKey(p.id);
  if (master && master !== self) return master;
  if (p.synced) return self;
  return "";
}

function sameSyncGroup(a: HudPlayer, b: HudPlayer) {
  const ga = groupKey(a);
  const gb = groupKey(b);
  return !!ga && ga === gb;
}

/** Drop target's sync group. A slave joins its master; an idle player is its own group. */
function syncDest(p: HudPlayer) {
  const master = idKey(p.syncmaster);
  const self = idKey(p.id);
  if (master && master !== self) return p.syncmaster;
  return p.id;
}

function nestPlayers(list: HudPlayer[]): { player: HudPlayer; nest: Nest }[] {
  const ids = new Set(list.map((p) => idKey(p.id)));
  const slaves = new Map<string, HudPlayer[]>();
  for (const p of list) {
    const master = idKey(p.syncmaster);
    const self = idKey(p.id);
    if (master && master !== self && ids.has(master)) {
      const arr = slaves.get(master) || [];
      arr.push(p);
      slaves.set(master, arr);
    }
  }
  const child = new Set<string>();
  slaves.forEach((arr) => arr.forEach((p) => child.add(idKey(p.id))));
  const out: { player: HudPlayer; nest: Nest }[] = [];
  for (const p of list) {
    if (child.has(idKey(p.id))) continue;
    const kids = slaves.get(idKey(p.id)) || [];
    if (!kids.length) {
      out.push({ player: p, nest: "solo" });
      continue;
    }
    out.push({ player: p, nest: "master" });
    kids.forEach((s, i) => out.push({ player: s, nest: i === kids.length - 1 ? "last" : "mid" }));
  }
  return out;
}

const sections = computed((): HudSection[] => {
  const nested = nestPlayers(displayPlayers.value);
  const out: HudSection[] = [];
  let current: HudSection | null = null;
  let block: HudBlock | null = null;
  for (const item of nested) {
    if (item.nest === "mid" || item.nest === "last") {
      block?.slaves.push({ player: item.player, nest: item.nest });
      continue;
    }
    const id: HudSection["id"] = item.player.isgroup ? "groups" : "players";
    if (!current || current.id !== id) {
      current = {
        id,
        label: id === "groups" ? "Groupes de platines" : "Platines",
        blocks: [],
      };
      out.push(current);
      block = null;
    }
    block = {
      player: item.player,
      nest: item.nest === "master" ? "master" : "solo",
      slaves: [],
    };
    current.blocks.push(block);
  }
  return out;
});

function rpcUrl() {
  return `http://${host}:${port}/jsonrpc.js`;
}

async function rpc(playerId: string, cmd: (string | number)[], timeout = 4) {
  if (!host) return null;
  try {
    const res = await http.fetch(rpcUrl(), {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      timeout,
      body: Body.json({
        id: 1,
        method: "slim.request",
        params: [playerId, cmd],
      }),
    });
    return (res.data as { result?: Record<string, unknown> })?.result ?? null;
  } catch (e) {
    console.log("hud rpc", cmd, e);
    return null;
  }
}

function coverUrl(id: string, artwork: string, trackKey: string) {
  if (artwork && /^https?:/i.test(artwork)) return artwork;
  if (artwork && artwork.startsWith("/")) return `http://${host}:${port}${artwork}`;
  const prev = coverCache.get(id + trackKey);
  if (prev) return prev;
  const url = `http://${host}:${port}/music/current/cover.jpg?player=${encodeURIComponent(id)}&t=${encodeURIComponent(trackKey)}`;
  coverCache.set(id + trackKey, url);
  const img = new Image();
  img.src = url;
  return url;
}

function isPreview(name: string, id: string) {
  const n = (name || "").toLowerCase();
  const i = (id || "").toLowerCase();
  return n.includes("preview") || i.startsWith("02:4c:4d") || i.startsWith("02-4c-4d");
}

function hiddenIds(): Set<string> {
  const out = new Set<string>();
  try {
    String(localStorage.getItem("hidePlayers") || "")
      .split(",")
      .forEach((s) => {
        const t = s.trim();
        if (t) {
          out.add(t);
          out.add(t.toLowerCase());
        }
      });
    String(localStorage.getItem("disabledPlayers") || "")
      .split(",")
      .forEach((s) => {
        const t = s.trim();
        if (t) out.add(t);
      });
  } catch {
    /* ignore */
  }
  return out;
}

function weightMap(): Record<string, number> {
  try {
    return JSON.parse(localStorage.getItem("playerWeightMap") || "{}") || {};
  } catch {
    return {};
  }
}

function roundVol(n: number) {
  if (!Number.isFinite(n)) return 0;
  return Math.max(0, Math.min(100, Math.round(n)));
}

async function loadConfig() {
  try {
    const raw = await readTextFile("config.json", { dir: BaseDirectory.AppConfig });
    const cfg = JSON.parse(raw);
    host = String(cfg?.server?.host || "").trim();
    port = Number(cfg?.server?.port) || 9000;
  } catch {
    host = "";
  }
}

function pickIcon(spec: IconSpec | undefined): IconSpec {
  if (!spec) return { icon: "speaker" };
  if (spec.svg) return { svg: spec.svg };
  if (spec.icon) return { icon: spec.icon };
  return { icon: "speaker" };
}

function resolveIcon(id: string, model: string, modelname: string, firmware: string): IconSpec {
  const custom = playerIconById.get(idKey(id));
  if (custom?.icon || custom?.svg) return pickIcon(custom);
  const entry = playerIconModels.get(model);
  if (!entry) return { icon: "speaker" };
  const firmwareList = entry.firmware as IconSpec[] | undefined;
  if (firmware && firmwareList) {
    for (const item of firmwareList) {
      if (item.ends && firmware.endsWith(item.ends)) return pickIcon(item);
    }
  }
  const idList = entry.playerid as IconSpec[] | undefined;
  if (idList) {
    const mac = id.toLowerCase();
    for (const item of idList) {
      if (item.begins && mac.startsWith(item.begins.toLowerCase())) return pickIcon(item);
    }
  }
  if (typeof entry.icon === "string" || typeof entry.svg === "string") {
    return pickIcon({ icon: typeof entry.icon === "string" ? entry.icon : "", svg: typeof entry.svg === "string" ? entry.svg : "" });
  }
  const named = modelname ? (entry[modelname] as IconSpec | undefined) : undefined;
  if (named) return pickIcon(named);
  return { icon: "speaker" };
}

function ensureIconFont() {
  if (!host || document.getElementById("hud-mi-font")) return;
  const link = document.createElement("link");
  link.id = "hud-mi-font";
  link.rel = "stylesheet";
  link.href = `http://${host}:${port}/material/html/font/font.css`;
  document.head.appendChild(link);
}

async function loadPlayerIcons() {
  if (iconsReady || !host) return;
  ensureIconFont();
  try {
    const res = await http.fetch(`http://${host}:${port}/material/html/misc/player-icons.json`, {
      method: "GET",
      timeout: 4,
    });
    const data = res.data as Record<string, Record<string, unknown>>;
    if (data && typeof data === "object") {
      playerIconModels.clear();
      for (const [key, value] of Object.entries(data)) {
        if (value && typeof value === "object") playerIconModels.set(key, value);
      }
    }
  } catch {
    /* model map stays empty; rows fall back to the speaker glyph */
  }
  const settings = await rpc("", ["material-skin", "playersettings"]);
  const list = (settings?.players as Array<Record<string, unknown>>) || [];
  playerIconById.clear();
  for (const row of list) {
    const id = String(row.id || "");
    if (!id || row.icon == null) continue;
    let spec: unknown = row.icon;
    if (typeof spec === "string") {
      try {
        spec = JSON.parse(spec);
      } catch {
        continue;
      }
    }
    if (!spec || typeof spec !== "object") continue;
    const parsed = spec as IconSpec;
    playerIconById.set(idKey(id), { icon: parsed.icon || "", svg: parsed.svg || "" });
  }
  iconsReady = playerIconModels.size > 0 || playerIconById.size > 0;
}

function svgUrl(name: string) {
  return `http://${host}:${port}/material/svg/${encodeURIComponent(name)}?c=${iconInk.value}`;
}

function shownVol(p: HudPlayer) {
  const live = volLive.value[p.id];
  return live == null ? p.volume : live;
}

function leadStyle(p: HudPlayer) {
  return { "--hud-lead": (p.ison && p.cover ? 78 : 40) + "px" };
}

function mapPlayer(
  id: string,
  name: string,
  isgroup: boolean,
  weight: number,
  st: Record<string, unknown> | null,
  prev?: HudPlayer,
  listedPower?: unknown,
  groups: Map<string, string> | null = null,
  glyph?: IconSpec
): HudPlayer {
  const tracks = (st?.playlist_loop as Array<Record<string, unknown>>) || [];
  const tr = tracks[0] || {};
  const title = String(tr.title || st?.current_title || "");
  const artist = String(tr.artist || tr.trackartist || "");
  const volRaw = st?.["mixer volume"];
  let volume = typeof volRaw === "number" ? volRaw : Number(volRaw);
  if (!Number.isFinite(volume)) volume = prev?.volume ?? 0;
  const muted = volume < 0;
  volume = roundVol(Math.abs(volume));
  const held = volHold.get(id);
  if (held && Date.now() < held.until) volume = held.v;
  else if (held) volHold.delete(id);
  const mode = String(st?.mode || "");
  const ison = powerOn(st, listedPower);
  const sync = syncFields(id, st, groups);
  const art = String(tr.artwork_url || "");
  const trackKey = title + "|" + artist;
  const sleepSec = Number(st?.will_sleep_in);
  const sleepUntil =
    st && Number.isFinite(sleepSec) && sleepSec > 1 ? Date.now() + sleepSec * 1000 : st ? 0 : prev?.sleepUntil || 0;
  const alarmRaw = st?.alarm_next2 ?? st?.alarm_next;
  const alarmNum = Number(alarmRaw);
  const alarmAt = st ? (Number.isFinite(alarmNum) && alarmNum > 0 ? alarmNum : 0) : prev?.alarmAt || 0;
  return {
    id,
    name,
    track: title || (ison ? "Rien en lecture" : "Éteinte"),
    artist,
    cover: ison ? coverUrl(id, art, trackKey) : "",
    volume,
    muted,
    isplaying: mode === "play",
    waiting: mode === "play" && !!st?.waitingToPlay,
    ison,
    isgroup,
    hasTrack: !!title,
    weight,
    syncmaster: sync.master,
    synced: sync.synced,
    sleepUntil,
    alarmAt,
    shuffle: modeNum(st?.["playlist shuffle"]),
    repeat: modeNum(st?.["playlist repeat"]),
    icon: glyph ? (glyph.svg ? "" : glyph.icon || "speaker") : prev?.icon || "speaker",
    svg: glyph ? glyph.svg || "" : prev?.svg || "",
  };
}

function modeNum(v: unknown) {
  const n = typeof v === "number" ? v : Number(v);
  return Number.isFinite(n) ? n : 0;
}

function pad2(n: number) {
  return String(n).padStart(2, "0");
}

function formatRemain(secs: number) {
  const s = Math.max(0, Math.floor(secs));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  if (h > 0) return h + ":" + pad2(m) + ":" + pad2(sec);
  return m + ":" + pad2(sec);
}

function formatAlarmClock(unix: number) {
  const d = new Date(unix * 1000);
  const hhmm = pad2(d.getHours()) + ":" + pad2(d.getMinutes());
  const now = new Date(nowTick.value);
  if (d.toDateString() === now.toDateString()) return hhmm;
  const days = ["dim.", "lun.", "mar.", "mer.", "jeu.", "ven.", "sam."];
  return days[d.getDay()] + " " + hhmm;
}

function sleepSecs(p: HudPlayer) {
  if (!p.sleepUntil) return 0;
  return Math.max(0, Math.floor((p.sleepUntil - nowTick.value) / 1000));
}

function alarmLabel(p: HudPlayer) {
  if (!p.alarmAt) return "";
  const rem = p.alarmAt - Math.floor(nowTick.value / 1000);
  if (rem > 0 && rem < 24 * 3600) return formatRemain(rem);
  return formatAlarmClock(p.alarmAt);
}

function armTick(on: boolean) {
  if (tickTimer) {
    clearInterval(tickTimer);
    tickTimer = null;
  }
  if (on) {
    nowTick.value = Date.now();
    tickTimer = setInterval(() => {
      nowTick.value = Date.now();
    }, 1000);
  }
}

function idKey(id: string) {
  return (id || "").toLowerCase().replace(/-/g, ":");
}

/** First `syncgroups` member is the master. Null means the query failed. */
function syncMasterMap(
  res: Record<string, unknown> | null,
  ids: Map<string, string>
): Map<string, string> | null {
  if (!res) return null;
  const out = new Map<string, string>();
  const loop = (res.syncgroups_loop as Array<Record<string, unknown>>) || [];
  for (const g of loop) {
    const members = String(g.sync_members ?? "")
      .split(",")
      .map((s) => s.trim())
      .filter((s) => s && s !== "-");
    if (members.length < 2) continue;
    const master = ids.get(idKey(members[0])) || members[0];
    for (const member of members) out.set(idKey(member), master);
  }
  return out;
}

function syncFields(
  id: string,
  st: Record<string, unknown> | null,
  groups: Map<string, string> | null
): { master: string; synced: boolean } {
  const self = idKey(id);
  if (groups) {
    const master = groups.get(self) || "";
    return master ? { master, synced: true } : { master: "", synced: false };
  }
  const rawMaster = String(st?.sync_master || st?.syncmaster || "");
  const slavesRaw = st?.sync_slaves ?? st?.syncslaves ?? "";
  const hasSlaves = Array.isArray(slavesRaw)
    ? slavesRaw.length > 0
    : String(slavesRaw || "")
        .split(",")
        .some((s) => {
          const t = s.trim();
          return !!t && t !== "-";
        });
  const masterKey = idKey(rawMaster);
  if (hasSlaves || (masterKey && masterKey !== self)) {
    return { master: !masterKey || masterKey === self ? id : rawMaster, synced: true };
  }
  return { master: "", synced: false };
}

function powerOn(st: Record<string, unknown> | null, listed: unknown) {
  const power = st && Object.prototype.hasOwnProperty.call(st, "power") ? st.power : listed;
  return !(power === 0 || power === "0" || power === false);
}

function mergeFromMaterial(list: { id: string; name: string }[], current?: string) {
  if (current) currentId.value = current;
  const clean = list.filter((p) => p.id && !isPreview(p.name, p.id));
  if (!clean.length) return;
  materialOrder.value = clean.map((p) => p.id);
  const have = new Map(players.value.map((p) => [idKey(p.id), p]));
  const next = clean.map((p) => {
    const prev = have.get(idKey(p.id));
    if (prev) return { ...prev, name: p.name || prev.name, id: p.id };
    return mapPlayer(p.id, p.name, false, -1, null, undefined);
  });
  players.value = next;
  error.value = "";
  loading.value = false;
  if (mergeRefreshTimer) clearTimeout(mergeRefreshTimer);
  mergeRefreshTimer = setTimeout(() => {
    if (alive) void refreshPlayers();
  }, 700);
}

async function refreshPlayers() {
  if (volDragging.size || dragId.value || swipingId.value) return;
  if (!host) {
    if (!players.value.length) error.value = "Aucun serveur configuré";
    loading.value = false;
    return;
  }
  const gen = ++refreshGen;
  const [list, groupsRes] = await Promise.all([
    rpc("", ["players", "0", "99"]),
    rpc("", ["syncgroups", "?"]),
  ]);
  if (gen !== refreshGen) return;
  if (!list) {
    loading.value = false;
    if (!players.value.length) error.value = "Aucune platine";
    return;
  }
  const loop = (list?.players_loop as Array<Record<string, unknown>>) || [];
  const hidden = hiddenIds();
  const weights = weightMap();
  const wanted = loop
    .map((p) => ({
      id: String(p.playerid || ""),
      name: String(p.name || p.playerid || ""),
      isgroup: String(p.model || "") === "group",
      model: String(p.model || ""),
      modelname: String(p.modelname || ""),
      firmware: String(p.firmware || ""),
      connected: String(p.connected ?? "1") === "1",
      power: p.power,
    }))
    .filter(
      (p) =>
        p.id &&
        p.connected &&
        !isPreview(p.name, p.id) &&
        !hidden.has(p.id) &&
        !hidden.has(p.id.toLowerCase())
    );
  await loadPlayerIcons();
  if (gen !== refreshGen) return;
  const prevById = new Map(players.value.map((p) => [idKey(p.id), p]));
  const idByKey = new Map(wanted.map((p) => [idKey(p.id), p.id]));
  const groups = syncMasterMap(groupsRes, idByKey);
  let out: HudPlayer[] = await Promise.all(
    wanted.map(async (p) => {
      const st = await rpc(p.id, ["status", "-", 1, "tags:AalK", "alarmData:1"]);
      const w = weights[p.id];
      return mapPlayer(
        p.id,
        p.name,
        p.isgroup,
        typeof w === "number" ? w : -1,
        st,
        prevById.get(idKey(p.id)),
        p.power,
        groups,
        resolveIcon(p.id, p.model, p.modelname, p.firmware)
      );
    })
  );
  if (gen !== refreshGen) return;
  if (materialOrder.value.length) {
    const allow = new Set(materialOrder.value.map(idKey));
    const filtered = out.filter((p) => allow.has(idKey(p.id)));
    if (filtered.length) out = filtered;
  }
  if (!out.length) {
    loading.value = false;
    if (!players.value.length) error.value = "Aucune platine";
    return;
  }
  players.value = out;
  loading.value = false;
  error.value = "";
  if (hudVisible.value) {
    await fitWindow();
    await nextTick();
    scrollCurrent();
  }
}

async function fitWindow() {
  if (userSized) return;
  let blocks = 0;
  for (const sec of sections.value) {
    blocks += SEP_H;
    for (const b of sec.blocks) blocks += ROW_H * (1 + b.slaves.length);
  }
  const screenH = window.screen?.availHeight || window.innerHeight || 900;
  const maxH = Math.floor(screenH * 0.9);
  const h = Math.min(Math.max(140, blocks + PAD), Math.max(140, maxH));
  fitting = true;
  try {
    await appWindow.setSize(new LogicalSize(HUD_WIDTH, h));
    await invoke("position_players_window");
  } catch (e) {
    console.log("hud size", e);
  }
  window.setTimeout(() => {
    fitting = false;
  }, 80);
}

function scrollCurrent() {
  const id = currentId.value;
  if (!id) return;
  document.getElementById("hud-" + id)?.scrollIntoView({ block: "nearest" });
}

function bump() {
  if (fading.value) {
    fading.value = false;
    void invoke("restore_players_window_alpha").catch(() => {});
  }
  if (!hudVisible.value) return;
  if (fadeTimer) {
    clearTimeout(fadeTimer);
    fadeTimer = null;
  }
  if (idleTimer) {
    clearTimeout(idleTimer);
    idleTimer = null;
  }
  if (hovered || menu.value.show) return;
  idleTimer = setTimeout(() => {
    if (hovered || menu.value.show) return;
    void hideHud();
  }, IDLE_MS);
}

function onHudEnter() {
  hovered = true;
  bump();
}

function onHudLeave() {
  hovered = false;
  bump();
}

async function hideHud() {
  if (!hudVisible.value) return;
  fading.value = true;
  closeMenu();
  armTick(false);
  try {
    await invoke("fade_hide_players_window");
  } catch {
    try {
      await appWindow.hide();
    } catch {
      /* ignore */
    }
  }
}

function armPoll(visible: boolean) {
  if (pollTimer) {
    clearInterval(pollTimer);
    pollTimer = null;
  }
  pollTimer = setInterval(() => {
    if (alive) void refreshPlayers();
  }, visible ? POLL_VISIBLE : POLL_HIDDEN);
}

function startVisible() {
  hudVisible.value = true;
  fading.value = false;
  userSized = false;
  kbNav.value = false;
  armTick(true);
  armPoll(true);
  if (!players.value.length) loading.value = true;
  void refreshPlayers();
  void nextTick(() => {
    hovered = !!document.querySelector(".hud")?.matches(":hover");
    bump();
  });
}

async function cmd(id: string, c: (string | number)[], refresh = false) {
  bump();
  void rpc(id, c);
  if (refresh) {
    window.setTimeout(() => {
      if (alive) void refreshPlayers();
    }, 220);
  }
}

function prev(p: HudPlayer, ev: Event) {
  ev.stopPropagation();
  p.waiting = true;
  void cmd(syncDest(p), ["button", "jump_rew"], true);
}
function next(p: HudPlayer, ev: Event) {
  ev.stopPropagation();
  p.waiting = true;
  void cmd(syncDest(p), ["playlist", "index", "+1"], true);
}
function playPause(p: HudPlayer, ev: Event) {
  ev.stopPropagation();
  p.waiting = true;
  p.isplaying = !p.isplaying;
  void cmd(syncDest(p), ["pause"], true);
}
function cycleShuffle(p: HudPlayer, ev: Event) {
  ev.stopPropagation();
  const nextMode = p.shuffle === 1 ? 2 : p.shuffle === 2 ? 0 : 1;
  p.shuffle = nextMode;
  void cmd(syncDest(p), ["playlist", "shuffle", nextMode], true);
}
function cycleRepeat(p: HudPlayer, ev: Event) {
  ev.stopPropagation();
  const nextMode = p.repeat === 0 ? 2 : p.repeat === 2 ? 1 : 0;
  p.repeat = nextMode;
  void cmd(syncDest(p), ["playlist", "repeat", nextMode], true);
}
function shuffleTitle(p: HudPlayer) {
  if (p.shuffle === 2) return "Aléatoire par album";
  if (p.shuffle === 1) return "Aléatoire";
  return "Aléatoire désactivé";
}
function repeatTitle(p: HudPlayer) {
  if (p.repeat === 1) return "Répéter le titre";
  if (p.repeat === 2) return "Répéter la liste";
  return "Répétition désactivée";
}
function openMore(p: HudPlayer, ev: MouseEvent) {
  ev.stopPropagation();
  onContext(p, ev);
}
const volPending = new Map<string, number>();
const volBusy = new Set<string>();

function holdVol(id: string, v: number) {
  volHold.set(id, { v, until: Date.now() + 1800 });
  volDragging.add(id);
  volLive.value = { ...volLive.value, [id]: v };
  const gen = (volGen.get(id) || 0) + 1;
  volGen.set(id, gen);
  window.setTimeout(() => {
    if (volGen.get(id) !== gen) return;
    if (volDragId.value === id) {
      holdVol(id, v);
      return;
    }
    volDragging.delete(id);
    const next = { ...volLive.value };
    delete next[id];
    volLive.value = next;
  }, 1800);
}

async function flushVol(id: string) {
  if (volBusy.has(id)) return;
  volBusy.add(id);
  try {
    while (volPending.has(id)) {
      const v = volPending.get(id) as number;
      volPending.delete(id);
      const volume = String(v);
      try {
        await invoke("lms_mixer_volume", { host, port: String(port), player: id, volume });
      } catch (e) {
        console.log("hud volume", e);
        await rpc(id, ["mixer", "volume", volume]);
      }
    }
  } finally {
    volBusy.delete(id);
    if (volPending.has(id)) void flushVol(id);
  }
}

function endVolDrag() {
  volDragId.value = "";
  document.removeEventListener("pointerup", endVolDrag);
  document.removeEventListener("pointercancel", endVolDrag);
}
function onVolDown(p: HudPlayer, ev: PointerEvent) {
  volDragId.value = p.id;
  const el = ev.currentTarget as HTMLElement;
  try {
    el.setPointerCapture(ev.pointerId);
  } catch {
    /* ignore */
  }
  document.addEventListener("pointerup", endVolDrag);
  document.addEventListener("pointercancel", endVolDrag);
}
function onVolUp() {
  endVolDrag();
}
function volumeFromPointer(el: HTMLInputElement, ev: PointerEvent) {
  const rect = el.getBoundingClientRect();
  if (rect.width < 4) return roundVol(Number(el.value));
  const t = (ev.clientX - rect.left) / rect.width;
  return roundVol(Math.min(1, Math.max(0, t)) * 100);
}
function commitVol(p: HudPlayer, v: number) {
  p.volume = v;
  p.muted = false;
  volDragId.value = p.id;
  holdVol(p.id, v);
  bump();
  volPending.set(p.id, v);
  void flushVol(p.id);
}
function setVol(p: HudPlayer, ev: Event) {
  ev.stopPropagation();
  commitVol(p, roundVol(Number((ev.target as HTMLInputElement).value)));
}
function onVolMove(p: HudPlayer, ev: PointerEvent) {
  if (volDragId.value !== p.id || ev.buttons === 0) return;
  const el = ev.currentTarget as HTMLInputElement;
  const v = volumeFromPointer(el, ev);
  if (v === shownVol(p)) return;
  el.value = String(v);
  commitVol(p, v);
}

async function openPlayer(p: HudPlayer) {
  if (longFired || skipClick || dragId.value || swipingId.value) {
    skipClick = false;
    longFired = false;
    return;
  }
  bump();
  await invoke("set_current_player", { id: p.id }).catch(() => {});
  await emit("hud-open-player", { id: p.id, name: p.name });
  await hideHud();
}

function showToast(msg: string) {
  toast.value = msg;
  window.setTimeout(() => {
    if (toast.value === msg) toast.value = "";
  }, 1800);
}

function closeMenu() {
  menu.value.show = false;
  menu.value.player = null;
  menu.value.pane = "root";
}

function onContext(p: HudPlayer, ev: MouseEvent) {
  ev.preventDefault();
  ev.stopPropagation();
  bump();
  const pad = 8;
  const mw = 220;
  const mh = 220;
  menu.value = {
    show: true,
    x: Math.min(ev.clientX, window.innerWidth - mw - pad),
    y: Math.min(ev.clientY, window.innerHeight - mh - pad),
    player: p,
    pane: "root",
  };
}

async function menuPrefs() {
  const p = menu.value.player;
  closeMenu();
  if (!p) return;
  await invoke("set_current_player", { id: p.id }).catch(() => {});
  await emit("open-player-settings", { id: p.id, name: p.name });
  await hideHud();
}

function togglePower(p: HudPlayer) {
  const next = !p.ison;
  p.ison = next;
  void cmd(p.id, ["power", next ? "1" : "0"], true);
  void nextTick(() => {
    if (hudVisible.value) void fitWindow();
  });
}

function menuPower() {
  const p = menu.value.player;
  if (!p) return;
  togglePower(p);
  closeMenu();
}

function refreshSoon() {
  void refreshPlayers();
  window.setTimeout(() => {
    if (alive) void refreshPlayers();
  }, 450);
}

function menuSleep(secs: number) {
  const p = menu.value.player;
  if (!p) return;
  void cmd(p.id, ["sleep", secs], true);
  closeMenu();
}

async function menuSyncTo(target: HudPlayer) {
  const p = menu.value.player;
  closeMenu();
  if (!p || p.id === target.id) return;
  const dest = syncDest(target);
  await rpc(dest, ["sync", p.id]);
  showToast("Synchronisé avec " + target.name);
  refreshSoon();
}

async function unsyncPlayer(p: HudPlayer) {
  skipClick = true;
  await rpc(p.id, ["sync", "-"]);
  showToast("Désynchronisé");
  refreshSoon();
}

async function menuUnsync() {
  const p = menu.value.player;
  closeMenu();
  if (!p) return;
  await unsyncPlayer(p);
}

async function transferQueue(to: HudPlayer) {
  const fromId = currentId.value;
  if (!fromId || fromId === to.id) {
    currentId.value = to.id;
    return;
  }
  const res = await rpc("", [
    "material-skin",
    "transferqueue",
    "from:" + fromId,
    "to:" + to.id,
    "mode:move",
  ]);
  if (res && (res as { error?: string }).error) {
    showToast(String((res as { error: string }).error));
    return;
  }
  currentId.value = to.id;
  await invoke("set_current_player", { id: to.id }).catch(() => {});
  await emit("hud-set-player", { id: to.id, name: to.name });
  void rpc(to.id, ["play"]);
  showToast("File transférée vers " + to.name);
  void refreshPlayers();
}

let swipeEl: HTMLElement | null = null;

function playerById(id: string) {
  const key = idKey(id);
  return players.value.find((p) => idKey(p.id) === key);
}


function beginLift(p: HudPlayer, ev: PointerEvent) {
  press.lift = true;
  dragId.value = p.id;
  dropId.value = "";
  dragX.value = ev.clientX;
  dragY.value = ev.clientY;
  document.documentElement.classList.add("hud-dragging");
}

function moveLift(ev: PointerEvent) {
  dragX.value = ev.clientX;
  dragY.value = ev.clientY;
  const hit = document.elementFromPoint(ev.clientX, ev.clientY);
  const row = hit?.closest("[data-hud-player]") as HTMLElement | null;
  const tid = row?.dataset.hudPlayer || "";
  const from = playerById(dragId.value);
  const to = tid ? playerById(tid) : undefined;
  if (!from || !to || idKey(from.id) === idKey(to.id) || sameSyncGroup(from, to)) {
    dropId.value = "";
    return;
  }
  dropId.value = to.id;
}

async function dropSync(from: HudPlayer, to: HudPlayer) {
  const dest = syncDest(to);
  if (idKey(dest) === idKey(from.id) || sameSyncGroup(from, to)) return;
  await rpc(dest, ["sync", from.id]);
  showToast("Synchronisé avec " + to.name);
  refreshSoon();
}

function onRowDown(p: HudPlayer, ev: PointerEvent) {
  if (ev.button !== 0 || ev.ctrlKey || ev.metaKey) return;
  const target = ev.target as HTMLElement | null;
  if (target?.closest("button, input, .hud-controls, .hud-power")) return;
  longFired = false;
  press = { x: ev.clientX, y: ev.clientY, id: p.id, moved: false, swipe: false, lift: false };
  swipeEl = ev.currentTarget as HTMLElement;
  try {
    swipeEl.setPointerCapture(ev.pointerId);
  } catch {
    /* ignore */
  }
  if (longTimer) clearTimeout(longTimer);
  longTimer = setTimeout(() => {
    longTimer = null;
    if (press.moved || press.id !== p.id || press.lift) return;
    longFired = true;
    void transferQueue(p);
  }, LONG_MS);
}

function onRowMove(p: HudPlayer, ev: PointerEvent) {
  if (!press.id || press.id !== p.id) return;
  if (press.lift) {
    moveLift(ev);
    return;
  }
  const dx = ev.clientX - press.x;
  const dy = ev.clientY - press.y;
  if (!press.swipe) {
    if (Math.hypot(dx, dy) < 8) return;
    press.moved = true;
    if (longTimer) {
      clearTimeout(longTimer);
      longTimer = null;
    }
    if (p.ison && Math.abs(dx) > Math.abs(dy)) {
      press.swipe = true;
      swipingId.value = p.id;
      swipeEl?.classList.add("swiping");
    } else {
      beginLift(p, ev);
      return;
    }
  }
  const el = swipeEl;
  if (!el) return;
  const x = Math.max(-140, Math.min(140, dx));
  el.style.setProperty("--swipe", x.toFixed(1) + "px");
  el.style.setProperty("--hint-prev", dx > 12 ? Math.min(1, (dx - 12) / 80).toFixed(3) : "0");
  el.style.setProperty("--hint-next", dx < -12 ? Math.min(1, (-dx - 12) / 80).toFixed(3) : "0");
}

function finishSwipe(el: HTMLElement | null, p: HudPlayer | undefined, dx: number) {
  const go = !!p?.ison && Math.abs(dx) >= 52;
  swipingId.value = "";
  if (!el) return;
  el.classList.remove("swiping");
  if (go && p) {
    const forward = dx < 0;
    void el.offsetWidth;
    el.style.setProperty("--swipe", (forward ? -140 : 140) + "px");
    el.style.setProperty("--hint-prev", forward ? "0" : "1");
    el.style.setProperty("--hint-next", forward ? "1" : "0");
    if (forward) next(p, new Event("swipe"));
    else prev(p, new Event("swipe"));
    window.setTimeout(() => {
      el.classList.add("no-swipe-anim");
      el.style.setProperty("--swipe", "0px");
      el.style.setProperty("--hint-prev", "0");
      el.style.setProperty("--hint-next", "0");
      requestAnimationFrame(() => el.classList.remove("no-swipe-anim"));
    }, 180);
    return;
  }
  el.style.setProperty("--swipe", "0px");
  el.style.setProperty("--hint-prev", "0");
  el.style.setProperty("--hint-next", "0");
}

function endLift() {
  const from = playerById(dragId.value);
  const to = dropId.value ? playerById(dropId.value) : undefined;
  dragId.value = "";
  dropId.value = "";
  document.documentElement.classList.remove("hud-dragging");
  if (from && to) void dropSync(from, to);
}

function onRowUp(ev?: PointerEvent) {
  if (longTimer) {
    clearTimeout(longTimer);
    longTimer = null;
  }
  const cap = ev?.currentTarget as HTMLElement | null;
  if (cap && ev && cap.hasPointerCapture?.(ev.pointerId)) {
    try {
      cap.releasePointerCapture(ev.pointerId);
    } catch {
      /* ignore */
    }
  }
  const dx = ev ? ev.clientX - press.x : 0;
  const didSwipe = press.swipe;
  const didLift = press.lift;
  const id = press.id;
  const moved = press.moved;
  press = { x: 0, y: 0, id: "", moved: false, swipe: false, lift: false };
  if (didLift) {
    skipClick = true;
    endLift();
    swipeEl = null;
    return;
  }
  if (didSwipe) {
    skipClick = true;
    finishSwipe(swipeEl, playerById(id), dx);
    swipeEl = null;
    return;
  }
  swipeEl = null;
  if (moved) skipClick = true;
}

function onRowEnter(ev: MouseEvent) {
  const root = ev.currentTarget as HTMLElement;
  root.querySelectorAll<HTMLElement>(".hud-line").forEach((el) => {
    const span = el.querySelector("span");
    if (!span) return;
    const overflow = span.scrollWidth > el.clientWidth + 2;
    el.classList.toggle("overflow", overflow);
    if (overflow) {
      el.style.setProperty("--shift", el.clientWidth - span.scrollWidth - 8 + "px");
      el.style.setProperty("--dur", Math.max(4, Math.abs(span.scrollWidth - el.clientWidth) / 28) + "s");
    }
  });
}

onMounted(async () => {
  try {
    const os = await platform();
    hudPlat.value = os === "win32" ? "win" : os === "linux" ? "linux" : "mac";
  } catch {
    hudPlat.value = "mac";
  }
  document.documentElement.classList.add("plat-" + hudPlat.value);
  const ink = window.matchMedia("(prefers-color-scheme: light)");
  const applyInk = () => {
    iconInk.value = ink.matches ? "1d1d1f" : "edece7";
  };
  if (ink.addEventListener) ink.addEventListener("change", applyInk);
  else ink.addListener(applyInk);
  await loadConfig();
  currentId.value = (await invoke<string | null>("get_current_player").catch(() => null)) || "";
  void refreshPlayers();
  armPoll(false);
  unlisteners.push(
    await listen<{ host?: string; port?: number | string }>("lms-target-changed", async (e) => {
      const h = String(e.payload?.host || "").trim();
      if (!h) return;
      host = h;
      port = Number(e.payload?.port) || 9000;
      coverCache.clear();
      iconsReady = false;
      playerIconModels.clear();
      playerIconById.clear();
      void refreshPlayers();
    })
  );
  unlisteners.push(
    await listen<{ players?: { id: string; name: string }[]; current?: string }>(
      "material-players",
      (e) => {
        const list = (e.payload?.players || []).filter((p) => p?.id);
        if (list.length) mergeFromMaterial(list, e.payload?.current);
        else if (e.payload?.current) currentId.value = e.payload.current;
      }
    )
  );
  unlisteners.push(
    await listen("players-visibility", async (e) => {
      const on = e.payload === true;
      if (on) {
        await loadConfig();
        currentId.value =
          (await invoke<string | null>("get_current_player").catch(() => null)) || currentId.value;
        startVisible();
      } else {
        hudVisible.value = false;
        armTick(false);
        armPoll(false);
        if (idleTimer) clearTimeout(idleTimer);
        if (fadeTimer) clearTimeout(fadeTimer);
        fading.value = false;
        closeMenu();
      }
    })
  );
  unlisteners.push(
    await listen<string>("players-current", (e) => {
      if (typeof e.payload === "string" && e.payload) currentId.value = e.payload;
    })
  );
  unlisteners.push(
    await listen("players-focus", (e) => {
      if (e.payload === false) void hideHud();
    })
  );
  try {
    unlisteners.push(
      await appWindow.onResized(() => {
        if (!fitting) userSized = true;
      })
    );
  } catch {
    /* ignore */
  }
  window.addEventListener("keydown", onHudKey);
});

function onHudKey(ev: KeyboardEvent) {
  if (ev.key === "Tab" || ev.key.startsWith("Arrow")) kbNav.value = true;
  if (ev.key === "Escape") {
    ev.preventDefault();
    void hideHud();
  }
}

onUnmounted(() => {
  alive = false;
  document.documentElement.classList.remove("hud-dragging");
  endVolDrag();
  window.removeEventListener("keydown", onHudKey);
  if (pollTimer) clearInterval(pollTimer);
  if (tickTimer) clearInterval(tickTimer);
  if (mergeRefreshTimer) clearTimeout(mergeRefreshTimer);
  if (idleTimer) clearTimeout(idleTimer);
  if (fadeTimer) clearTimeout(fadeTimer);
  if (longTimer) clearTimeout(longTimer);
  unlisteners.forEach((u) => u());
});
</script>

<template>
  <div
    class="hud"
    :class="['plat-' + hudPlat, { kb: kbNav, dragging: !!dragId }]"
    tabindex="-1"
    @mouseenter="onHudEnter"
    @mouseleave="onHudLeave"
    @mousemove="bump"
    @wheel="bump"
    @keydown="bump"
    @pointerdown="bump"
    @selectstart.prevent
    @dragstart.prevent
    @click="menu.show && closeMenu()"
  >
    <div class="hud-panel">
      <div v-if="loading && !displayPlayers.length" class="hud-empty">Chargement…</div>
      <div v-else-if="error && !displayPlayers.length" class="hud-empty">{{ error }}</div>
      <div v-else class="hud-list">
        <template v-for="sec in sections" :key="sec.id">
          <div class="hud-sep">{{ sec.label }}</div>
          <div class="hud-sort" :data-hud-sort="sec.id">
            <div
              v-for="block in sec.blocks"
              :key="block.player.id"
              class="hud-block"
              :class="{ 'has-slaves': block.slaves.length }"
              :data-id="block.player.id"
            >
              <article
                :id="'hud-' + block.player.id"
                :data-hud-player="block.player.id"
                class="hud-row"
                :class="{
                  current: block.player.id === currentId,
                  off: !block.player.ison,
                  group: block.player.isgroup,
                  synced: block.player.synced,
                  'has-art': block.player.ison && !!block.player.cover,
                  'is-master': block.nest === 'master',
                  'has-controls': block.player.ison,
                  swiping: swipingId === block.player.id,
                  'drag-src': dragId === block.player.id,
                  'drop-on': dropId === block.player.id,
                  'vol-active': volDragId === block.player.id,
                }"
                :style="leadStyle(block.player)"
                @click="openPlayer(block.player)"
                @contextmenu="onContext(block.player, $event)"
                @pointerdown="onRowDown(block.player, $event)"
                @pointermove="onRowMove(block.player, $event)"
                @pointerup="onRowUp($event)"
                @pointercancel="onRowUp($event)"
                @mouseenter="onRowEnter($event)"
              >
                <div class="hud-face">
                  <div class="hud-icon" aria-hidden="true">
                    <img v-if="block.player.svg" class="hud-svg" :src="svgUrl(block.player.svg)" alt="" draggable="false" />
                    <span v-else class="hud-mi">{{ block.player.icon || "speaker" }}</span>
                  </div>
                  <div v-if="block.player.ison && block.player.cover" class="hud-cover">
                    <img
                      :src="block.player.cover"
                      alt=""
                      draggable="false"
                      @error="($event.target as HTMLImageElement).style.display = 'none'"
                    />
                  </div>
                  <div class="hud-copy">
                    <div class="hud-name-row">
                      <div class="hud-line hud-name"><span>{{ block.player.name }}</span></div>
                      <span v-if="block.player.ison && block.player.isplaying" class="hud-vu" aria-hidden="true"><i></i><i></i><i></i></span>
                      <span v-if="sleepSecs(block.player) > 0" class="hud-flag" title="Mise en veille">
                        <svg viewBox="0 0 24 24"><path fill="currentColor" d="M7 13c1.66 0 3-1.34 3-3S8.66 7 7 7 4 8.34 4 10s1.34 3 3 3zm12-6h-8v7H3V5H1v15h2v-3h18v3h2v-9c0-2.21-1.79-4-4-4z"/></svg>
                        {{ formatRemain(sleepSecs(block.player)) }}
                      </span>
                      <span v-if="block.player.alarmAt" class="hud-flag" title="Alarme">
                        <svg viewBox="0 0 24 24"><path fill="currentColor" d="M12 22c1.1 0 2-.9 2-2h-4a2 2 0 0 0 2 2zm6-6v-5c0-3.07-1.64-5.64-4.5-6.32V4c0-.83-.67-1.5-1.5-1.5s-1.5.67-1.5 1.5v.68C7.63 5.36 6 7.92 6 11v5l-2 2v1h16v-1l-2-2z"/></svg>
                        {{ alarmLabel(block.player) }}
                      </span>
                    </div>
                    <div v-if="block.player.ison" class="hud-line hud-track">
                      <span>{{ block.player.track }}<template v-if="block.player.artist"> · {{ block.player.artist }}</template></span>
                    </div>
                  </div>
                </div>
                <button
                  v-if="!block.player.ison"
                  type="button"
                  class="hud-power"
                  title="Allumer"
                  aria-label="Allumer"
                  tabindex="-1"
                  @click.stop="togglePower(block.player)"
                  @pointerdown.stop
                  @pointerup.stop
                >
                  <svg viewBox="0 0 24 24"><path fill="currentColor" d="M13 3h-2v10h2V3zm4.83 2.17-1.42 1.42A6.92 6.92 0 0 1 19 12a7 7 0 1 1-14 0c0-2.22.9-4.23 2.59-5.41L6.17 5.17A8.94 8.94 0 0 0 3 12a9 9 0 1 0 18 0 8.94 8.94 0 0 0-3.17-6.83z"/></svg>
                </button>
                <div v-if="block.player.ison" class="hud-controls" @click.stop @pointerdown.stop @mousedown.stop>
                  <button type="button" class="hud-skip" tabindex="-1" :disabled="!block.player.hasTrack" title="Précédent" @click="prev(block.player, $event)">
                    <svg viewBox="0 0 24 24"><path fill="currentColor" d="M6 6h2v12H6V6zm3.5 6 8.5 6V6l-8.5 6z"/></svg>
                  </button>
                  <button
                    type="button"
                    class="hud-pp"
                    tabindex="-1"
                    :class="{ waiting: block.player.waiting, pause: block.player.isplaying || block.player.waiting, play: !block.player.isplaying && !block.player.waiting }"
                    title="Lecture / pause"
                    @click="playPause(block.player, $event)"
                  >
                    <svg class="np-pp-icon" viewBox="0 0 24 24" aria-hidden="true">
                      <circle class="np-pp-buffer-ring" cx="12" cy="12" r="10.6" fill="none" stroke="currentColor" stroke-width="1.35" stroke-linecap="round"/>
                      <circle class="np-pp-circle" cx="12" cy="12" r="10" fill="none" stroke="currentColor" stroke-width="1.5"/>
                      <g class="np-pp-layer np-pp-play-layer"><path fill="currentColor" d="M10.5 8.2v7.6l5.8-3.8z"/></g>
                      <g class="np-pp-layer np-pp-pause-layer">
                        <g class="np-pp-pause-filled">
                          <rect x="9.25" y="8.25" width="2.25" height="7.5" rx="0.35" fill="currentColor"/>
                          <rect x="12.5" y="8.25" width="2.25" height="7.5" rx="0.35" fill="currentColor"/>
                        </g>
                        <g class="np-pp-pause-waiting">
                          <rect x="9.25" y="8.25" width="2.25" height="7.5" rx="0.35" fill="currentColor"/>
                          <rect x="12.5" y="8.25" width="2.25" height="7.5" rx="0.35" fill="currentColor"/>
                        </g>
                      </g>
                    </svg>
                  </button>
                  <button type="button" class="hud-skip" tabindex="-1" :disabled="!block.player.hasTrack" title="Suivant" @click="next(block.player, $event)">
                    <svg viewBox="0 0 24 24"><path fill="currentColor" d="M16 6h2v12h-2V6zM6 18l8.5-6L6 6v12z"/></svg>
                  </button>
                  <input
                    class="hud-vol"
                    type="range"
                    tabindex="-1"
                    min="0"
                    max="100"
                    step="1"
                    :value="shownVol(block.player)"
                    :title="'Volume ' + shownVol(block.player) + '%'"
                    @click.stop
                    @pointerdown.stop="onVolDown(block.player, $event)"
                    @pointermove.stop="onVolMove(block.player, $event)"
                    @pointerup.stop="onVolUp"
                    @pointercancel.stop="onVolUp"
                    @mousedown.stop
                    @input="setVol(block.player, $event)"
                    @change="setVol(block.player, $event)"
                  />
                  <button
                    type="button"
                    class="hud-mode"
                    :class="{ on: block.player.shuffle > 0, albums: block.player.shuffle === 2 }"
                    tabindex="-1"
                    :title="shuffleTitle(block.player)"
                    @click="cycleShuffle(block.player, $event)"
                  >
                    <svg viewBox="0 0 24 24"><path fill="currentColor" d="M10.59 9.17 5.41 4 4 5.41l5.17 5.17 1.42-1.41zM14.5 4l2.04 2.04L4 18.59 5.41 20 17.96 7.46 20 9.5V4h-5.5zm.33 9.41-1.41 1.41 3.13 3.13L14.5 20H20v-5.5l-2.04 2.04-3.13-3.13z"/></svg>
                  </button>
                  <button
                    type="button"
                    class="hud-mode"
                    :class="{ on: block.player.repeat > 0, one: block.player.repeat === 1 }"
                    tabindex="-1"
                    :title="repeatTitle(block.player)"
                    @click="cycleRepeat(block.player, $event)"
                  >
                    <svg viewBox="0 0 24 24"><path fill="currentColor" d="M7 7h10v3l4-4-4-4v3H5v6h2V7zm10 10H7v-3l-4 4 4 4v-3h12v-6h-2v4z"/></svg>
                  </button>
                  <button type="button" class="hud-more" tabindex="-1" title="Plus" aria-label="Plus" @click="openMore(block.player, $event)">
                    <svg viewBox="0 0 24 24"><path fill="currentColor" d="M6 10a2 2 0 1 0 .001 4.001A2 2 0 0 0 6 10zm6 0a2 2 0 1 0 .001 4.001A2 2 0 0 0 12 10zm6 0a2 2 0 1 0 .001 4.001A2 2 0 0 0 18 10z"/></svg>
                  </button>
                </div>
                <button
                  v-if="block.player.synced"
                  type="button"
                  class="hud-chain"
                  title="Détacher"
                  aria-label="Détacher"
                  tabindex="-1"
                  @click.stop="unsyncPlayer(block.player)"
                  @pointerdown.stop
                  @pointerup.stop
                >
                  <svg viewBox="0 0 24 24"><path fill="currentColor" d="M3.9 12c0-1.71 1.39-3.1 3.1-3.1h4V7H7c-2.76 0-5 2.24-5 5s2.24 5 5 5h4v-1.9H7c-1.71 0-3.1-1.39-3.1-3.1zM8 13h8v-2H8v2zm9-6h-4v1.9h4c1.71 0 3.1 1.39 3.1 3.1s-1.39 3.1-3.1 3.1h-4V17h4c2.76 0 5-2.24 5-5s-2.24-5-5-5z"/></svg>
                </button>
              </article>
              <div
                v-if="!block.player.isgroup || block.slaves.length"
                class="hud-nest"
                :class="{ open: block.slaves.length }"
                :data-hud-nest="block.player.id"
              >
                <div v-for="s in block.slaves" :key="s.player.id" class="hud-block" :data-id="s.player.id">
                  <article
                    :id="'hud-' + s.player.id"
                    :data-hud-player="s.player.id"
                    class="hud-row is-slave"
                    :class="{
                      current: s.player.id === currentId,
                      off: !s.player.ison,
                      synced: s.player.synced,
                      'has-art': s.player.ison && !!s.player.cover,
                      'has-controls': s.player.ison,
                      swiping: swipingId === s.player.id,
                      'drag-src': dragId === s.player.id,
                      'drop-on': dropId === s.player.id,
                      'vol-active': volDragId === s.player.id,
                    }"
                    :style="leadStyle(s.player)"
                    @click="openPlayer(s.player)"
                    @contextmenu="onContext(s.player, $event)"
                    @pointerdown="onRowDown(s.player, $event)"
                    @pointermove="onRowMove(s.player, $event)"
                    @pointerup="onRowUp($event)"
                    @pointercancel="onRowUp($event)"
                    @mouseenter="onRowEnter($event)"
                  >
                    <div class="hud-face">
                      <div class="hud-icon" aria-hidden="true">
                        <img v-if="s.player.svg" class="hud-svg" :src="svgUrl(s.player.svg)" alt="" draggable="false" />
                        <span v-else class="hud-mi">{{ s.player.icon || "speaker" }}</span>
                      </div>
                      <div v-if="s.player.ison && s.player.cover" class="hud-cover">
                        <img
                          :src="s.player.cover"
                          alt=""
                          draggable="false"
                          @error="($event.target as HTMLImageElement).style.display = 'none'"
                        />
                      </div>
                      <div class="hud-copy">
                        <div class="hud-name-row">
                          <div class="hud-line hud-name"><span>{{ s.player.name }}</span></div>
                          <span v-if="s.player.ison && s.player.isplaying" class="hud-vu" aria-hidden="true"><i></i><i></i><i></i></span>
                          <span v-if="sleepSecs(s.player) > 0" class="hud-flag" title="Mise en veille">
                            <svg viewBox="0 0 24 24"><path fill="currentColor" d="M7 13c1.66 0 3-1.34 3-3S8.66 7 7 7 4 8.34 4 10s1.34 3 3 3zm12-6h-8v7H3V5H1v15h2v-3h18v3h2v-9c0-2.21-1.79-4-4-4z"/></svg>
                            {{ formatRemain(sleepSecs(s.player)) }}
                          </span>
                          <span v-if="s.player.alarmAt" class="hud-flag" title="Alarme">
                            <svg viewBox="0 0 24 24"><path fill="currentColor" d="M12 22c1.1 0 2-.9 2-2h-4a2 2 0 0 0 2 2zm6-6v-5c0-3.07-1.64-5.64-4.5-6.32V4c0-.83-.67-1.5-1.5-1.5s-1.5.67-1.5 1.5v.68C7.63 5.36 6 7.92 6 11v5l-2 2v1h16v-1l-2-2z"/></svg>
                            {{ alarmLabel(s.player) }}
                          </span>
                        </div>
                        <div v-if="s.player.ison" class="hud-line hud-track">
                          <span>{{ s.player.track }}<template v-if="s.player.artist"> · {{ s.player.artist }}</template></span>
                        </div>
                      </div>
                    </div>
                    <button
                      v-if="!s.player.ison"
                      type="button"
                      class="hud-power"
                      title="Allumer"
                      aria-label="Allumer"
                      tabindex="-1"
                      @click.stop="togglePower(s.player)"
                      @pointerdown.stop
                      @pointerup.stop
                    >
                      <svg viewBox="0 0 24 24"><path fill="currentColor" d="M13 3h-2v10h2V3zm4.83 2.17-1.42 1.42A6.92 6.92 0 0 1 19 12a7 7 0 1 1-14 0c0-2.22.9-4.23 2.59-5.41L6.17 5.17A8.94 8.94 0 0 0 3 12a9 9 0 1 0 18 0 8.94 8.94 0 0 0-3.17-6.83z"/></svg>
                    </button>
                    <div v-if="s.player.ison" class="hud-controls hud-controls-vol" @click.stop @pointerdown.stop @mousedown.stop>
                      <input
                        class="hud-vol"
                        type="range"
                        tabindex="-1"
                        min="0"
                        max="100"
                        step="1"
                        :value="shownVol(s.player)"
                        :title="'Volume ' + shownVol(s.player) + '%'"
                        @click.stop
                        @pointerdown.stop="onVolDown(s.player, $event)"
                        @pointermove.stop="onVolMove(s.player, $event)"
                        @pointerup.stop="onVolUp"
                        @pointercancel.stop="onVolUp"
                        @mousedown.stop
                        @input="setVol(s.player, $event)"
                        @change="setVol(s.player, $event)"
                      />
                    </div>
                    <button
                      v-if="s.player.synced"
                      type="button"
                      class="hud-chain"
                      title="Détacher"
                      aria-label="Détacher"
                      tabindex="-1"
                      @click.stop="unsyncPlayer(s.player)"
                      @pointerdown.stop
                      @pointerup.stop
                    >
                      <svg viewBox="0 0 24 24"><path fill="currentColor" d="M3.9 12c0-1.71 1.39-3.1 3.1-3.1h4V7H7c-2.76 0-5 2.24-5 5s2.24 5 5 5h4v-1.9H7c-1.71 0-3.1-1.39-3.1-3.1zM8 13h8v-2H8v2zm9-6h-4v1.9h4c1.71 0 3.1 1.39 3.1 3.1s-1.39 3.1-3.1 3.1h-4V17h4c2.76 0 5-2.24 5-5s-2.24-5-5-5z"/></svg>
                    </button>
                  </article>
                </div>
              </div>
            </div>
          </div>
        </template>
      </div>
    </div>

    <div
      v-if="menu.show && menu.player"
      class="hud-menu"
      :style="{ left: menu.x + 'px', top: menu.y + 'px' }"
      @click.stop
    >
      <template v-if="menu.pane === 'root'">
        <button type="button" @click="menuPrefs">
          <svg viewBox="0 0 24 24"><path fill="currentColor" d="M19.14 12.94c.04-.31.06-.63.06-.94s-.02-.63-.06-.94l2.03-1.58a.5.5 0 0 0 .12-.64l-1.92-3.32a.5.5 0 0 0-.6-.22l-2.39.96a7.07 7.07 0 0 0-1.63-.94l-.36-2.54A.5.5 0 0 0 13.9 2h-3.8a.5.5 0 0 0-.49.42l-.36 2.54c-.6.24-1.15.55-1.63.94l-2.39-.96a.5.5 0 0 0-.6.22L2.71 8.48a.5.5 0 0 0 .12.64l2.03 1.58c-.04.31-.06.63-.06.94s.02.63.06.94L2.83 14.16a.5.5 0 0 0-.12.64l1.92 3.32c.13.23.4.32.64.22l2.39-.96c.48.39 1.03.7 1.63.94l.36 2.54c.05.24.25.42.49.42h3.8c.24 0 .44-.18.49-.42l.36-2.54c.6-.24 1.15-.55 1.63-.94l2.39.96c.24.1.51.01.64-.22l1.92-3.32a.5.5 0 0 0-.12-.64l-2.03-1.58zM12 15.5A3.5 3.5 0 1 1 12 8.5a3.5 3.5 0 0 1 0 7z"/></svg>
          <span>Préférences</span>
        </button>
        <button type="button" @click="menuPower">
          <svg viewBox="0 0 24 24"><path fill="currentColor" d="M13 3h-2v10h2V3zm4.83 2.17-1.42 1.42A6.92 6.92 0 0 1 19 12a7 7 0 1 1-14 0c0-2.22.9-4.23 2.59-5.41L6.17 5.17A8.94 8.94 0 0 0 3 12a9 9 0 1 0 18 0 8.94 8.94 0 0 0-3.17-6.83z"/></svg>
          <span>{{ menu.player.ison ? 'Éteindre' : 'Allumer' }}</span>
        </button>
        <button type="button" @click="menu.pane = 'sleep'">
          <svg viewBox="0 0 24 24"><path fill="currentColor" d="M12.3 4.1a8 8 0 1 0 7.6 7.6 7 7 0 0 1-7.6-7.6z"/></svg>
          <span>Mise en veille</span>
        </button>
        <button type="button" @click="menu.pane = 'sync'">
          <svg viewBox="0 0 24 24"><path fill="currentColor" d="M7.5 7.5a4.5 4.5 0 0 1 6.36 0l.64.64 1.41-1.41-.64-.64A6.5 6.5 0 0 0 5 15h-2l3 4 3-4H7a4.5 4.5 0 0 1 .5-7.5zm9 9a4.5 4.5 0 0 1-6.36 0l-.64-.64-1.41 1.41.64.64A6.5 6.5 0 0 0 19 9h2l-3-4-3 4h2a4.5 4.5 0 0 1 .5 7.5z"/></svg>
          <span>Synchroniser</span>
        </button>
      </template>
      <template v-else-if="menu.pane === 'sleep'">
        <button type="button" class="hud-menu-back" @click="menu.pane = 'root'">
          <svg viewBox="0 0 24 24"><path fill="currentColor" d="M15.41 7.41 14 6l-6 6 6 6 1.41-1.41L10.83 12z"/></svg>
          <span>Mise en veille</span>
        </button>
        <button type="button" @click="menuSleep(900)"><span>15 minutes</span></button>
        <button type="button" @click="menuSleep(1800)"><span>30 minutes</span></button>
        <button type="button" @click="menuSleep(2700)"><span>45 minutes</span></button>
        <button type="button" @click="menuSleep(3600)"><span>1 heure</span></button>
        <button type="button" @click="menuSleep(0)"><span>Annuler la veille</span></button>
      </template>
      <template v-else>
        <button type="button" class="hud-menu-back" @click="menu.pane = 'root'">
          <svg viewBox="0 0 24 24"><path fill="currentColor" d="M15.41 7.41 14 6l-6 6 6 6 1.41-1.41L10.83 12z"/></svg>
          <span>Synchroniser</span>
        </button>
        <button
          v-for="o in displayPlayers.filter((x) => x.id !== menu.player?.id)"
          :key="o.id"
          type="button"
          @click="menuSyncTo(o)"
        >
          <span>{{ o.name }}</span>
        </button>
        <button v-if="menu.player.synced" type="button" @click="menuUnsync">
          <svg viewBox="0 0 24 24"><path fill="currentColor" d="M17 7h-4v2h4a3 3 0 0 1 1.8 5.4l1.4 1.4A5 5 0 0 0 17 7zM3.3 4.7 2 6l3.1 3.1A5 5 0 0 0 7 17h4v-2H7a3 3 0 0 1-.2-6l1.7 1.7 1.4-1.4zM14 13h-2.2l2 2H14v2h4.2l2 2 1.3-1.3z"/></svg>
          <span>Désynchroniser</span>
        </button>
      </template>
    </div>
    <div
      v-if="dragPlayer"
      class="hud-lift"
      :style="{ left: dragX + 'px', top: dragY + 'px' }"
    >
      <div class="hud-icon" aria-hidden="true">
        <img v-if="dragPlayer.svg" class="hud-svg" :src="svgUrl(dragPlayer.svg)" alt="" draggable="false" />
        <span v-else class="hud-mi">{{ dragPlayer.icon || "speaker" }}</span>
      </div>
      <span>{{ dragPlayer.name }}</span>
    </div>
    <div v-if="toast" class="hud-toast">{{ toast }}</div>
  </div>
</template>

<style>
html, body, #app {
  margin: 0;
  height: 100%;
  background: transparent;
  overflow: hidden;
  font-family: -apple-system, BlinkMacSystemFont, "SF Pro Text", "Helvetica Neue", sans-serif;
  color: #f5f5f7;
  user-select: none;
  -webkit-user-select: none;
  -webkit-touch-callout: none;
  cursor: default;
  color-scheme: dark light;
  --hud-accent: #0a84ff;
  --hud-radius: 10px;
  --hud-nest: 16px;
  --hud-fill: rgba(255, 255, 255, 0.08);
  --hud-line: rgba(255, 255, 255, 0.14);
  --hud-group: rgba(255, 255, 255, 0.05);
}
html.plat-win, html.plat-win body, html.plat-win #app {
  font-family: "Segoe UI Variable Text", "Segoe UI", sans-serif;
  --hud-accent: #4cc2ff;
  --hud-radius: 4px;
  --hud-nest: 12px;
}
/* Catalina's WKWebView turns backdrop-filter into an opaque black plate and
   hides the native vibrancy behind the page. */
html.os-catalina .hud-panel {
  background: rgba(28, 28, 30, 0.42);
  -webkit-backdrop-filter: none;
  backdrop-filter: none;
}
html.os-catalina .hud-menu {
  background: rgba(44, 44, 46, 0.96);
  -webkit-backdrop-filter: none;
  backdrop-filter: none;
}
html.plat-linux, html.plat-linux body, html.plat-linux #app {
  font-family: Cantarell, Ubuntu, "Noto Sans", sans-serif;
  --hud-accent: #3584e4;
  --hud-radius: 12px;
  --hud-nest: 14px;
}
.hud, .hud * {
  user-select: none;
  -webkit-user-select: none;
  -webkit-user-drag: none;
}
.hud img { -webkit-user-drag: none; }
.hud :focus { outline: none; box-shadow: none; }
.hud.kb :focus-visible {
  outline: 2px solid var(--hud-accent);
  outline-offset: -2px;
  border-radius: 6px;
}
.hud {
  height: 100%;
  display: flex;
  flex-direction: column;
  background: transparent;
  opacity: 1;
  transition: opacity 0.45s ease;
  padding: 0;
  box-sizing: border-box;
}
.hud-panel {
  position: relative;
  z-index: 1;
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
  border-radius: 0;
  overflow: hidden;
  background: rgba(12, 12, 14, 0.78);
  backdrop-filter: blur(40px) saturate(180%);
  -webkit-backdrop-filter: blur(40px) saturate(180%);
  border: 0.5px solid rgba(255, 255, 255, 0.16);
  border-top: 0;
  box-sizing: border-box;
}
html.hud-dragging,
html.hud-dragging * { cursor: grabbing; }
.hud-list {
  flex: 1;
  overflow: auto;
  scrollbar-width: none;
}
.hud-sep {
  padding: 8px 16px 4px;
  font-size: 11px;
  font-weight: 600;
  letter-spacing: 0.02em;
  color: rgba(235, 235, 245, 0.45);
  text-transform: uppercase;
}
.hud-list::-webkit-scrollbar { width: 0; height: 0; }
.hud-empty {
  padding: 28px 16px;
  text-align: center;
  color: rgba(235,235,245,0.5);
  font-size: 13px;
}
.hud-block.has-slaves {
  margin: 0 6px 4px;
  border-radius: 8px;
  background: var(--hud-group);
}
.hud-row {
  height: 40px;
  margin: 0 6px 4px;
  box-sizing: border-box;
  cursor: pointer;
  position: relative;
  border-radius: 8px;
  overflow: hidden;
  touch-action: none;
  --swipe: 0px;
  --hint-prev: 0;
  --hint-next: 0;
}
.hud-block.has-slaves > .hud-row { margin: 0; }
.hud-nest.open { padding-left: 16px; }
.hud-row.is-slave { margin: 0; width: 100%; }
.hud-row:hover { background: rgba(255,255,255,0.07); }
.hud-row.current { background: rgba(255,255,255,0.14); }
.hud-row::before,
.hud-row::after {
  content: "";
  position: absolute;
  top: 0;
  bottom: 0;
  width: 36px;
  z-index: 3;
  pointer-events: none;
}
.hud-row::before {
  left: 0;
  opacity: var(--hint-prev);
  background: linear-gradient(90deg, rgba(10, 132, 255, 0.5), transparent 85%);
}
.hud-row::after {
  right: 0;
  opacity: var(--hint-next);
  background: linear-gradient(270deg, rgba(10, 132, 255, 0.5), transparent 85%);
}
.hud-face {
  display: flex;
  align-items: center;
  height: 40px;
  transform: translateX(var(--swipe));
  transition: transform 0.2s cubic-bezier(0.2, 0.8, 0.2, 1);
}
.hud-row.swiping .hud-face,
.hud-row.no-swipe-anim .hud-face { transition: none; }
.hud-power {
  position: absolute;
  right: 8px;
  top: 50%;
  transform: translateY(-50%);
  z-index: 4;
  width: 28px;
  height: 28px;
  border: 0;
  border-radius: 7px;
  background: transparent;
  color: rgba(235, 235, 245, 0.92);
  display: grid;
  place-items: center;
  cursor: pointer;
  padding: 0;
}
.hud-power svg { width: 16px; height: 16px; display: block; }
.hud-power:hover { background: rgba(255, 255, 255, 0.14); color: #fff; }
.hud-nest:not(.open) {
  height: 0;
  margin: 0;
  overflow: hidden;
}
.hud-row.drag-src { opacity: 0.38; }
.hud-row.drop-on {
  background: rgba(10, 132, 255, 0.24);
  box-shadow: inset 0 0 0 1.5px var(--hud-accent);
}
.hud-lift {
  position: fixed;
  z-index: 40;
  pointer-events: none;
  display: flex;
  align-items: center;
  height: 36px;
  margin: 10px 0 0 12px;
  padding-right: 12px;
  border-radius: 8px;
  overflow: hidden;
  background: rgba(28, 28, 30, 0.94);
  box-shadow: 0 12px 28px rgba(0, 0, 0, 0.42), 0 0 0 0.5px rgba(255, 255, 255, 0.2);
}
.hud-lift .hud-icon { width: 36px; height: 36px; flex: 0 0 36px; }
.hud-lift span {
  max-width: 168px;
  padding-left: 8px;
  font-size: 13px;
  font-weight: 600;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
html.plat-mac .hud-nest.open { position: relative; }
html.plat-mac .hud-nest.open::before {
  content: "";
  position: absolute;
  left: 8px;
  top: 0;
  bottom: 4px;
  width: 1px;
  background: var(--hud-line);
  pointer-events: none;
}
html.plat-win .hud-block.has-slaves { box-shadow: inset 3px 0 0 var(--hud-accent); }
.hud-sort-ghost { opacity: 0.35; }
.hud-sort-fallback {
  border-radius: 8px;
  overflow: hidden;
  box-shadow: 0 10px 28px rgba(0, 0, 0, 0.35);
}
.hud-icon {
  width: 40px;
  height: 40px;
  flex: 0 0 40px;
  display: grid;
  place-items: center;
  color: rgba(235, 235, 245, 0.92);
}
.hud-mi {
  font-family: "Material Icons";
  font-weight: normal;
  font-style: normal;
  font-size: 22px;
  line-height: 1;
  letter-spacing: normal;
  text-transform: none;
  display: block;
  white-space: nowrap;
  word-wrap: normal;
  direction: ltr;
  -webkit-font-feature-settings: "liga";
  font-feature-settings: "liga";
  -webkit-font-smoothing: antialiased;
}
.hud-svg { width: 22px; height: 22px; display: block; object-fit: contain; }
.hud-chain {
  position: absolute;
  right: 4px;
  top: 50%;
  transform: translateY(-50%);
  z-index: 5;
  width: 26px;
  height: 26px;
  border: 0;
  border-radius: 6px;
  background: transparent;
  color: var(--hud-accent);
  display: grid;
  place-items: center;
  cursor: pointer;
  padding: 0;
}
.hud-chain svg { width: 16px; height: 16px; display: block; }
.hud-chain:hover { background: rgba(255, 255, 255, 0.14); }
.hud-row.off .hud-chain { right: 36px; }
.hud-row.synced .hud-controls { right: 30px; }
.hud-row.synced .hud-copy { padding-right: 32px; }
.hud-row.off.synced .hud-copy { padding-right: 70px; }
.hud-cover {
  width: 32px;
  height: 32px;
  flex: 0 0 32px;
  overflow: hidden;
  background: #2c2c2e;
  margin: 0 6px 0 0;
  border-radius: 4px;
}
.hud-cover img { width: 100%; height: 100%; object-fit: cover; display: block; }
.hud-cover.device {
  display: grid;
  place-items: center;
  color: rgba(235, 235, 245, 0.82);
}
.hud-cover.device svg { width: 26px; height: 26px; display: block; }
.hud-copy {
  min-width: 0;
  flex: 1;
  height: 40px;
  display: flex;
  flex-direction: column;
  justify-content: center;
  gap: 1px;
  padding: 0 10px;
  opacity: 1;
  transition: opacity 0.16s ease;
}
.hud-row.off .hud-copy { padding-right: 44px; }
.hud-name-row {
  display: flex;
  align-items: center;
  gap: 6px;
  min-width: 0;
}
.hud-line { overflow: hidden; white-space: nowrap; min-width: 0; flex: 1; }
.hud-copy > .hud-line { flex: 0 1 auto; }
.hud-line span { display: inline-block; white-space: nowrap; }
.hud-name { font-size: 13px; font-weight: 600; line-height: 16px; }
.hud-flag {
  display: inline-flex;
  align-items: center;
  gap: 3px;
  flex: 0 0 auto;
  font-size: 10px;
  font-variant-numeric: tabular-nums;
  color: rgba(235, 235, 245, 0.72);
  white-space: nowrap;
}
.hud-flag svg {
  width: 11px;
  height: 11px;
  flex: 0 0 11px;
  opacity: 0.9;
}
.hud-vu {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 2px;
  width: 14px;
  height: 16px;
  flex: 0 0 14px;
  color: var(--hud-accent);
}
.hud-vu i {
  display: block;
  width: 2.5px;
  height: 14px;
  border-radius: 1px;
  background: currentColor;
  transform-origin: center center;
  transform: scaleY(0.3);
  animation: hudVu 0.9s ease-in-out infinite;
}
.hud-vu i:nth-child(1) { animation-delay: -0.15s; }
.hud-vu i:nth-child(2) { animation-delay: -0.45s; }
.hud-vu i:nth-child(3) { animation-delay: -0.72s; }
@keyframes hudVu {
  0%, 100% { transform: scaleY(0.28); opacity: 0.55; }
  35% { transform: scaleY(0.72); opacity: 0.85; }
  65% { transform: scaleY(1); opacity: 1; }
}
.hud-track {
  font-size: 11px;
  line-height: 14px;
  color: rgba(235,235,245,0.55);
  margin: 0;
}
.hud-row:hover .hud-line.overflow span {
  animation: hud-marquee var(--dur, 8s) ease-in-out infinite;
}
@keyframes hud-marquee {
  0%, 12% { transform: translateX(0); }
  46%, 58% { transform: translateX(var(--shift, 0px)); }
  92%, 100% { transform: translateX(0); }
}
.hud-controls {
  position: absolute;
  left: var(--hud-lead, 40px);
  right: 0;
  top: 0;
  bottom: 0;
  z-index: 2;
  display: flex;
  align-items: center;
  gap: 0;
  padding: 0 4px 0 2px;
  opacity: 0;
  pointer-events: none;
  transition: opacity 0.16s ease;
}
.hud-row.has-controls:hover .hud-copy,
.hud-row.has-controls:focus-within .hud-copy { opacity: 0; }
.hud-row.has-controls:hover .hud-controls,
.hud-row.has-controls:focus-within .hud-controls {
  opacity: 1;
  pointer-events: auto;
}
.hud-row.swiping .hud-copy,
.hud-row.swiping.has-controls:hover .hud-copy { opacity: 1; }
.hud-row.swiping .hud-controls,
.hud-row.swiping.has-controls:hover .hud-controls {
  opacity: 0;
  pointer-events: none;
}
.hud-row.vol-active .hud-copy,
.hud-row.vol-active.has-controls:hover .hud-copy { opacity: 0; }
.hud-row.vol-active .hud-controls,
.hud-row.vol-active.has-controls .hud-controls {
  opacity: 1;
  pointer-events: auto;
}
.hud-skip {
  width: 28px; height: 28px; flex: 0 0 28px; border: 0; background: transparent; color: #f5f5f7;
  display: grid; place-items: center; cursor: pointer; padding: 0; opacity: 0.88;
}
.hud-skip:hover { opacity: 1; }
.hud-skip:disabled { opacity: 0.3; cursor: default; }
.hud-skip svg { width: 18px; height: 18px; }
.hud-pp {
  width: 32px; height: 32px; flex: 0 0 32px; border: 0; background: transparent; color: #f5f5f7;
  display: grid; place-items: center; cursor: pointer; padding: 0;
}
.hud-pp:hover .np-pp-icon { transform: scale(1.04); }
.np-pp-icon { width: 28px; height: 28px; display: block; overflow: visible; transition: transform 0.15s ease; }
.np-pp-layer {
  transform-origin: 12px 12px;
  transition: opacity 0.28s cubic-bezier(0.32, 0.72, 0, 1), transform 0.28s cubic-bezier(0.32, 0.72, 0, 1);
}
.np-pp-play-layer { opacity: 1; transform: scale(1) rotate(0deg); }
.np-pp-pause-layer { opacity: 0; transform: scale(0.82) rotate(-8deg); }
.hud-pp.pause .np-pp-play-layer { opacity: 0; transform: scale(0.82) rotate(8deg); }
.hud-pp.pause .np-pp-pause-layer { opacity: 1; transform: scale(1) rotate(0deg); }
.np-pp-pause-filled { opacity: 1; transition: opacity 0.18s ease; }
.np-pp-pause-waiting { opacity: 0; transition: opacity 0.18s ease; }
.np-pp-circle { fill: none; stroke: currentColor; stroke-width: 1.5; }
.np-pp-buffer-ring {
  opacity: 0; stroke-dasharray: 18 48; transform-origin: 12px 12px; transition: opacity 0.2s ease;
}
.hud-pp.waiting .np-pp-circle { opacity: 0.72; }
.hud-pp.waiting .np-pp-buffer-ring { opacity: 0.5; animation: np-pp-buffer-spin 1.35s linear infinite; }
.hud-pp.waiting .np-pp-pause-filled { opacity: 0; }
.hud-pp.waiting .np-pp-pause-waiting { opacity: 1; animation: np-pp-waiting-pulse 2.2s ease-in-out infinite; }
@keyframes np-pp-buffer-spin { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }
@keyframes np-pp-waiting-pulse { 0%, 100% { opacity: 0.82; } 50% { opacity: 1; } }
.hud .hud-vol {
  flex: 1;
  min-width: 48px;
  height: 28px;
  margin: 0 2px;
  background: transparent;
  cursor: pointer;
  touch-action: auto;
  -webkit-user-select: auto;
  user-select: auto;
  -webkit-user-drag: auto;
  -webkit-appearance: none;
  appearance: none;
}
.hud .hud-vol::-webkit-slider-runnable-track {
  height: 3px;
  background: rgba(235, 235, 245, 0.32);
  border-radius: 2px;
}
.hud .hud-vol::-webkit-slider-thumb {
  -webkit-appearance: none;
  appearance: none;
  width: 14px;
  height: 14px;
  margin-top: -5.5px;
  border-radius: 50%;
  background: #f5f5f7;
  box-shadow: 0 0.5px 2px rgba(0, 0, 0, 0.45);
  pointer-events: auto;
  -webkit-user-select: auto;
  user-select: auto;
}
.hud-mode, .hud-more {
  width: 26px; height: 26px; flex: 0 0 26px; border: 0; background: transparent;
  color: rgba(235, 235, 245, 0.5); display: grid; place-items: center; cursor: pointer; padding: 0;
  position: relative;
}
.hud-mode svg, .hud-more svg { width: 16px; height: 16px; display: block; }
.hud-mode.on { color: var(--hud-accent); }
.hud-mode.albums::after, .hud-mode.one::after {
  content: "1";
  position: absolute;
  right: 1px;
  bottom: 1px;
  font-size: 8px;
  font-weight: 700;
  line-height: 1;
}
.hud-mode.albums::after { content: "2"; }
.hud-mode:hover, .hud-more:hover { color: #fff; }
.hud-menu {
  position: fixed;
  z-index: 20;
  min-width: 196px;
  padding: 5px;
  border-radius: 8px;
  background: rgba(42, 42, 44, 0.94);
  backdrop-filter: blur(36px) saturate(180%);
  -webkit-backdrop-filter: blur(36px) saturate(180%);
  border: 0.5px solid rgba(255,255,255,0.18);
  box-shadow: 0 10px 40px rgba(0,0,0,0.45), 0 0 0 0.5px rgba(0,0,0,0.4);
}
.hud-menu button {
  display: flex;
  align-items: center;
  gap: 10px;
  width: 100%;
  text-align: left;
  border: 0;
  background: transparent;
  color: #f5f5f7;
  font: inherit;
  font-size: 13px;
  padding: 6px 10px;
  border-radius: 5px;
  cursor: pointer;
}
.hud-menu button svg {
  width: 16px;
  height: 16px;
  flex: 0 0 16px;
  opacity: 0.9;
}
.hud-menu button:hover { background: var(--hud-accent); color: #fff; }
html.plat-linux .hud-menu button:hover { background: var(--hud-fill); color: inherit; }
.hud-menu-back {
  font-weight: 600;
  opacity: 0.85;
  margin-bottom: 2px;
}
@media (prefers-color-scheme: light) {
  html, body, #app { color: #1d1d1f; }
  .hud-panel {
    background: rgba(246, 246, 248, 0.82);
    border-color: rgba(0, 0, 0, 0.12);
  }
  html.os-catalina .hud-panel {
    background: rgba(246, 246, 248, 0.5);
    -webkit-backdrop-filter: none;
    backdrop-filter: none;
  }
  html.os-catalina .hud-menu {
    background: rgba(246, 246, 248, 0.96);
    -webkit-backdrop-filter: none;
    backdrop-filter: none;
  }
  .hud-empty { color: rgba(60, 60, 67, 0.55); }
  .hud-sep { color: rgba(60, 60, 67, 0.45); }
  .hud-row:hover { background: rgba(0, 0, 0, 0.05); }
  .hud-row.current { background: rgba(0, 0, 0, 0.08); }
  html, body, #app {
    --hud-fill: rgba(0, 0, 0, 0.06);
    --hud-line: rgba(0, 0, 0, 0.12);
    --hud-group: rgba(0, 0, 0, 0.04);
  }
  html.plat-linux .hud-link:hover { background: rgba(0, 0, 0, 0.08); }
  .hud-cover { background: #e8e8ed; }
  .hud-cover.device { color: rgba(60, 60, 67, 0.78); }
  .hud-icon { color: rgba(28, 28, 30, 0.88); }
  .hud-chain:hover { background: rgba(0, 0, 0, 0.08); }
  .hud-mode, .hud-more { color: rgba(60, 60, 67, 0.5); }
  .hud-mode:hover, .hud-more:hover { color: #1d1d1f; }
  .hud-lift { background: rgba(246, 246, 248, 0.96); }
  .hud-row.drop-on { background: rgba(10, 132, 255, 0.16); }
  .hud-power { color: rgba(28, 28, 30, 0.88); }
  .hud-power:hover { background: rgba(0, 0, 0, 0.08); color: #1d1d1f; }
  .hud-track, .hud-vol-n, .hud-flag { color: rgba(60, 60, 67, 0.62); }
  .hud-skip, .hud-pp { color: #1d1d1f; }
  .hud .hud-vol::-webkit-slider-runnable-track { background: rgba(60, 60, 67, 0.28); }
  .hud .hud-vol::-webkit-slider-thumb { background: #1d1d1f; }
  .hud-menu {
    background: rgba(246, 246, 248, 0.94);
    border-color: rgba(0, 0, 0, 0.12);
  }
  .hud-menu button { color: #1d1d1f; }
  .hud-toast { background: rgba(255,255,255,0.88); color: #1d1d1f; }
}
.hud-toast {
  position: absolute;
  left: 50%;
  bottom: 16px;
  transform: translateX(-50%);
  z-index: 30;
  padding: 6px 12px;
  border-radius: 8px;
  background: rgba(0,0,0,0.72);
  font-size: 12px;
  pointer-events: none;
}
</style>
