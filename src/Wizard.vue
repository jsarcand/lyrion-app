<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted } from "vue";
import { invoke } from "@tauri-apps/api/tauri";
import { listen } from "@tauri-apps/api/event";

type WizardStep = "splash" | "servers" | "identity" | "plugins" | "install" | "launch";

type FoundServer = {
  host: string;
  port: string;
  name?: string;
};

const props = defineProps<{ version: string; skipSplash?: boolean }>();
const emit = defineEmits<{
  (e: "connect", payload: { host: string; port: number }): void;
}>();

const step = ref<WizardStep>("splash");
const reverse = ref(false);
const splashMinElapsed = ref(false);
const discovering = ref(false);
const servers = ref<FoundServer[]>([]);
const discoverError = ref("");
const manualHost = ref("");
const manualPort = ref("9000");

const identity = ref({
  serverName: "Lyrion",
  libraryPath: "",
  language: "FR",
  channel: "latest",
});

const groups = ref({
  library: true,
  streaming: false,
  mix: true,
  endpoints: true,
});

const installBusy = ref(false);
const installError = ref("");
const installProgress = ref("");
const installPct = ref<number | null>(null);
const installDone = ref(false);
const installSteps = ref<Array<{ id: string; label: string; ok: boolean }>>([]);

const windowStep = computed(() => {
  const order: WizardStep[] = ["servers", "identity", "plugins", "install", "launch"];
  const i = order.indexOf(step.value === "splash" ? "servers" : step.value);
  return Math.max(0, i);
});

function isLocal(host: string) {
  const h = (host || "").toLowerCase();
  return h === "127.0.0.1" || h === "localhost" || h === "::1";
}

async function discoverServers(quiet = false) {
  if (!quiet) discovering.value = true;
  discoverError.value = "";
  try {
    const found = await invoke<Array<Record<string, string>>>("detect_lms_servers");
    servers.value = (found || []).map((s) => ({
      host: s.host || "",
      port: s.port || "9000",
      name: s.name || undefined,
    })).filter((s) => s.host);
  } catch (e: any) {
    discoverError.value = String(e?.message || e || "Discovery failed");
    servers.value = [];
  } finally {
    discovering.value = false;
  }
}

function go(next: WizardStep, back = false) {
  reverse.value = back;
  step.value = next;
}

function chooseServer(s: FoundServer) {
  emit("connect", { host: s.host, port: Number(s.port) || 9000 });
}

function chooseManual() {
  const host = manualHost.value.trim();
  if (!host) return;
  emit("connect", { host, port: Number(manualPort.value) || 9000 });
}

function startInstallPath() {
  go("identity");
}

async function pickLibrary() {
  try {
    const p = await invoke<string | null>("lms_pick_library_folder");
    if (p) identity.value.libraryPath = p;
  } catch (e) {
    console.log("folder pick", e);
  }
}

async function runInstall() {
  go("install");
  installBusy.value = true;
  installError.value = "";
  installDone.value = false;
  installProgress.value = "Starting…";
  installPct.value = 0;
  installSteps.value = [
    { id: "lms", label: "Lyrion Music Server", ok: false },
    { id: "material", label: "Material Skin", ok: false },
    { id: "bridges", label: "Local endpoints", ok: false },
    { id: "start", label: "Start server", ok: false },
  ];
  try {
    await invoke("lms_wizard_run", {
      opts: {
        serverName: identity.value.serverName,
        libraryPath: identity.value.libraryPath,
        language: identity.value.language,
        channel: identity.value.channel,
        groupLibrary: groups.value.library,
        groupStreaming: groups.value.streaming,
        groupMix: groups.value.mix,
        groupEndpoints: groups.value.endpoints,
      },
    });
    installDone.value = true;
    installPct.value = 100;
    installProgress.value = "Server is ready";
    installSteps.value = installSteps.value.map((s) => ({ ...s, ok: true }));
    go("launch");
    setTimeout(() => {
      emit("connect", { host: "127.0.0.1", port: 9000 });
    }, 700);
  } catch (e: any) {
    installError.value = String(e?.message || e || "Install failed");
    installBusy.value = false;
  } finally {
    installBusy.value = false;
  }
}

let discoverTimer: ReturnType<typeof setInterval> | null = null;
const unlisteners: Array<() => void> = [];

onMounted(async () => {
  try {
    const n = await invoke<string>("lms_machine_name");
    if (n) identity.value.serverName = n;
  } catch { /* keep Lyrion */ }

  const lang = (navigator.language || "en").slice(0, 2).toUpperCase();
  identity.value.language = ["EN", "FR", "DE", "NL", "IT", "ES", "DA", "SV", "CS", "HU"].includes(lang)
    ? lang
    : "EN";

  try {
    const unlisten = await listen("lms-install-progress", (event) => {
      const p = event.payload as { stage?: string; percent?: number | null; message?: string };
      if (p?.message) installProgress.value = p.message;
      if (p?.percent != null) installPct.value = p.percent;
      if (p?.stage) {
        installSteps.value = installSteps.value.map((s) =>
          s.id === p.stage || (p.stage === "ready" && s.id === "start")
            ? { ...s, ok: p.percent === 100 || p.stage === "ready" }
            : s
        );
      }
    });
    unlisteners.push(unlisten);
    unlisteners.push(
      await listen("lms-local-ready", () => {
        void discoverServers();
      })
    );
  } catch (e) {
    console.warn("wizard events", e);
  }

  if (props.skipSplash) {
    splashMinElapsed.value = true;
    go("servers");
    void discoverServers();
  } else {
    const minSplash = new Promise<void>((r) => setTimeout(r, 1400));
    // Catalina's shell sometimes never answers IPC. Don't leave the splash up.
    const disc = Promise.race([
      discoverServers(),
      new Promise<void>((r) => setTimeout(r, 2500)),
    ]);
    await Promise.all([minSplash, disc]);
    splashMinElapsed.value = true;
    go("servers");
  }
  discoverTimer = setInterval(() => {
    if (step.value === "servers") void discoverServers(true);
  }, 4000);
});

onUnmounted(() => {
  if (discoverTimer) clearInterval(discoverTimer);
  unlisteners.forEach((u) => u());
});
</script>

<template>
  <div class="wiz">
    <transition name="splash">
      <div v-if="step === 'splash'" class="wiz-splash" aria-hidden="false">
        <img class="wiz-logo" src="/images/lyrion-logo.png" alt="" />
        <div class="wiz-wordmark">Lyrion</div>
        <div class="wiz-tag">25 years of the Squeezebox</div>
        <div class="wiz-pulse" />
      </div>
    </transition>

    <div v-show="step !== 'splash'" class="wiz-body">
      <header class="wiz-top">
        <img class="wiz-logo-sm" src="/images/lyrion-logo.png" alt="" />
        <div>
          <div class="wiz-title">Lyrion</div>
          <div class="wiz-sub">v{{ props.version }}</div>
        </div>
      </header>

      <v-window :model-value="windowStep" :reverse="reverse" class="wiz-window" :touch="false">
        <!-- Servers -->
        <v-window-item>
          <section class="wiz-page">
            <h1 class="wiz-h">Choose a server</h1>
            <p class="wiz-p">Lyrion Music Server on your network, or install one on this computer.</p>

            <div v-if="discovering" class="wiz-discover">
              <v-progress-linear indeterminate color="primary" height="2" />
              <span>Looking for servers…</span>
            </div>

            <ul v-else class="wiz-list">
              <li v-for="s in servers" :key="s.host + ':' + s.port">
                <button type="button" class="wiz-server" @click="chooseServer(s)">
                  <div class="wiz-server-name">{{ s.name || s.host }}</div>
                  <div class="wiz-server-meta">
                    {{ s.host }}:{{ s.port }}
                    <span v-if="isLocal(s.host)" class="wiz-badge">This Mac</span>
                  </div>
                </button>
              </li>
              <li v-if="!servers.length && !discoverError" class="wiz-empty">
                No server found on the network.
              </li>
              <li v-if="discoverError" class="wiz-empty">{{ discoverError }}</li>
            </ul>

            <div class="wiz-manual">
              <v-text-field
                v-model="manualHost"
                label="Host"
                placeholder="192.168.1.4"
                variant="outlined"
                density="comfortable"
                hide-details
              />
              <v-text-field
                v-model="manualPort"
                label="Port"
                variant="outlined"
                density="comfortable"
                hide-details
                style="max-width: 110px"
              />
              <v-btn variant="tonal" :disabled="!manualHost.trim()" @click="chooseManual">Connect</v-btn>
            </div>

            <div class="wiz-actions">
              <v-btn variant="text" :loading="discovering" @click="discoverServers">Refresh</v-btn>
              <v-btn color="primary" variant="flat" @click="startInstallPath">
                Install on this Mac
              </v-btn>
            </div>
          </section>
        </v-window-item>

        <!-- Identity -->
        <v-window-item>
          <section class="wiz-page">
            <h1 class="wiz-h">This server</h1>
            <p class="wiz-p">Name, library, and language for the copy of Lyrion Music Server on this computer.</p>

            <v-text-field
              v-model="identity.serverName"
              label="Server name"
              variant="outlined"
              density="comfortable"
              class="mb-3"
            />
            <div class="wiz-lib">
              <v-text-field
                v-model="identity.libraryPath"
                label="Music library folder"
                placeholder="Choose a folder…"
                variant="outlined"
                density="comfortable"
                hide-details
              />
              <v-btn variant="tonal" class="mt-1" @click="pickLibrary">Browse</v-btn>
            </div>
            <v-select
              class="mt-4"
              v-model="identity.language"
              label="Language"
              :items="['FR', 'EN', 'DE', 'NL', 'IT', 'ES']"
              variant="outlined"
              density="comfortable"
            />
            <v-select
              v-model="identity.channel"
              label="LMS release"
              :items="[
                { title: 'Latest release', value: 'latest' },
                { title: 'Stable nightly', value: 'stable' },
                { title: 'Development', value: 'dev' },
              ]"
              variant="outlined"
              density="comfortable"
            />

            <div class="wiz-actions">
              <v-btn variant="text" @click="go('servers', true)">Back</v-btn>
              <v-btn color="primary" variant="flat" @click="go('plugins')">Continue</v-btn>
            </div>
          </section>
        </v-window-item>

        <!-- Plugins -->
        <v-window-item>
          <section class="wiz-page">
            <h1 class="wiz-h">Plugins</h1>
            <p class="wiz-p">Material Skin is always included. Groups install together.</p>

            <div class="wiz-group locked">
              <div>
                <div class="wiz-group-title">Interface</div>
                <div class="wiz-group-desc">Material Skin (Craig Drummond)</div>
              </div>
              <v-switch :model-value="true" disabled color="primary" hide-details />
            </div>
            <div class="wiz-group">
              <div>
                <div class="wiz-group-title">Library</div>
                <div class="wiz-group-desc">Artist info, ratings, listening stats</div>
              </div>
              <v-switch v-model="groups.library" color="primary" hide-details />
            </div>
            <div class="wiz-group">
              <div>
                <div class="wiz-group-title">Streaming</div>
                <div class="wiz-group-desc">Spotify, Qobuz, YouTube — sign in later</div>
              </div>
              <v-switch v-model="groups.streaming" color="primary" hide-details />
            </div>
            <div class="wiz-group">
              <div>
                <div class="wiz-group-title">Mix</div>
                <div class="wiz-group-desc">Don’t Stop The Music, Random Mix</div>
              </div>
              <v-switch v-model="groups.mix" color="primary" hide-details />
            </div>
            <div class="wiz-group">
              <div>
                <div class="wiz-group-title">Local endpoints</div>
                <div class="wiz-group-desc">UPnP, Chromecast, AirPlay + grouped players. One binary for this Mac.</div>
              </div>
              <v-switch v-model="groups.endpoints" color="primary" hide-details />
            </div>

            <div class="wiz-actions">
              <v-btn variant="text" @click="go('identity', true)">Back</v-btn>
              <v-btn color="primary" variant="flat" @click="runInstall">Install</v-btn>
            </div>
          </section>
        </v-window-item>

        <!-- Install progress -->
        <v-window-item>
          <section class="wiz-page">
            <h1 class="wiz-h">Installing</h1>
            <p class="wiz-p">{{ installProgress || "Working…" }}</p>
            <v-progress-linear
              :model-value="installPct ?? 0"
              :indeterminate="installBusy && installPct == null"
              color="primary"
              height="4"
              class="mb-5"
            />
            <ul class="wiz-steps">
              <li v-for="s in installSteps" :key="s.id" :class="{ ok: s.ok }">
                {{ s.label }}
              </li>
            </ul>
            <p v-if="installError" class="wiz-err">{{ installError }}</p>
            <div class="wiz-actions">
              <v-btn variant="text" :disabled="installBusy" @click="go('plugins', true)">Back</v-btn>
              <v-btn v-if="installError" color="primary" variant="flat" @click="runInstall">Retry</v-btn>
            </div>
          </section>
        </v-window-item>

        <!-- Launch -->
        <v-window-item>
          <section class="wiz-page wiz-page--center">
            <img class="wiz-logo" src="/images/lyrion-logo.png" alt="" />
            <h1 class="wiz-h">Ready</h1>
            <p class="wiz-p">Your server is running on this Mac.</p>
            <v-btn color="primary" variant="flat" size="large" @click="emit('connect', { host: '127.0.0.1', port: 9000 })">
              Launch
            </v-btn>
          </section>
        </v-window-item>
      </v-window>
    </div>
  </div>
</template>

<style scoped>
.wiz {
  position: absolute;
  inset: 0;
  z-index: 30;
  background: #1c1c1e;
  color: #f5f5f7;
  overflow: hidden;
  font-family: -apple-system, BlinkMacSystemFont, "SF Pro Text", system-ui, sans-serif;
}

.wiz-splash {
  position: absolute;
  inset: 0;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  background: #1c1c1e;
  z-index: 2;
}

.wiz-logo {
  width: 88px;
  height: 88px;
  border-radius: 20px;
  animation: wiz-in 1.1s cubic-bezier(0.22, 1, 0.36, 1) both;
}

.wiz-wordmark {
  margin-top: 18px;
  font-size: 2rem;
  font-weight: 700;
  letter-spacing: -0.04em;
  animation: wiz-in 1.1s 0.08s cubic-bezier(0.22, 1, 0.36, 1) both;
}

.wiz-tag {
  margin-top: 6px;
  font-size: 0.92rem;
  color: rgba(235, 235, 245, 0.55);
  letter-spacing: 0.02em;
  animation: wiz-in 1.1s 0.16s cubic-bezier(0.22, 1, 0.36, 1) both;
}

.wiz-pulse {
  width: 36px;
  height: 2px;
  margin-top: 28px;
  border-radius: 2px;
  background: rgba(10, 132, 255, 0.85);
  animation: wiz-bar 1.4s ease-in-out infinite;
}

@keyframes wiz-in {
  from { opacity: 0; transform: scale(0.96) translateY(8px); }
  to { opacity: 1; transform: scale(1) translateY(0); }
}
@keyframes wiz-bar {
  0%, 100% { transform: scaleX(0.4); opacity: 0.4; }
  50% { transform: scaleX(1); opacity: 1; }
}

.splash-leave-active {
  transition: opacity 0.45s cubic-bezier(0.22, 1, 0.36, 1);
}
.splash-leave-to { opacity: 0; }

.wiz-body {
  height: 100%;
  display: flex;
  flex-direction: column;
  padding: 52px 28px 24px;
  box-sizing: border-box;
}

.wiz-top {
  display: flex;
  align-items: center;
  gap: 10px;
  margin-bottom: 18px;
}
.wiz-logo-sm {
  width: 32px;
  height: 32px;
  border-radius: 8px;
}
.wiz-title { font-weight: 700; letter-spacing: -0.02em; }
.wiz-sub { font-size: 0.75rem; color: rgba(235, 235, 245, 0.45); }

.wiz-window { flex: 1; overflow: hidden; }
.wiz-page {
  height: 100%;
  display: flex;
  flex-direction: column;
  overflow: auto;
}
.wiz-page--center {
  align-items: center;
  justify-content: center;
  text-align: center;
}

.wiz-h {
  font-size: 1.45rem;
  font-weight: 700;
  letter-spacing: -0.03em;
  margin: 0 0 6px;
}
.wiz-p {
  margin: 0 0 18px;
  color: rgba(235, 235, 245, 0.55);
  font-size: 0.9rem;
  line-height: 1.4;
}

.wiz-discover {
  display: flex;
  flex-direction: column;
  gap: 8px;
  color: rgba(235, 235, 245, 0.5);
  font-size: 0.8rem;
  margin-bottom: 12px;
}

.wiz-list {
  list-style: none;
  margin: 0 0 16px;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 8px;
  flex: 1;
}
.wiz-empty {
  color: rgba(235, 235, 245, 0.45);
  font-size: 0.9rem;
  padding: 12px 0;
}
.wiz-server {
  width: 100%;
  text-align: left;
  background: rgba(44, 44, 46, 0.7);
  border: 1px solid rgba(255, 255, 255, 0.08);
  border-radius: 12px;
  padding: 12px 14px;
  color: inherit;
  cursor: pointer;
}
.wiz-server:hover { background: rgba(58, 58, 60, 0.9); }
.wiz-server-name { font-weight: 600; }
.wiz-server-meta { font-size: 0.78rem; color: rgba(235, 235, 245, 0.5); margin-top: 2px; }
.wiz-badge {
  display: inline-block;
  margin-left: 8px;
  padding: 1px 7px;
  border-radius: 999px;
  font-size: 0.68rem;
  font-weight: 600;
  background: rgba(10, 132, 255, 0.22);
  color: #64d2ff;
}

.wiz-manual {
  display: flex;
  gap: 8px;
  align-items: center;
  margin-bottom: 12px;
}
.wiz-lib { display: flex; flex-direction: column; gap: 8px; }

.wiz-group {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 10px 12px;
  border-radius: 12px;
  background: rgba(44, 44, 46, 0.55);
  border: 1px solid rgba(255, 255, 255, 0.06);
  margin-bottom: 8px;
}
.wiz-group.locked { opacity: 0.85; }
.wiz-group-title { font-weight: 600; font-size: 0.92rem; }
.wiz-group-desc { font-size: 0.75rem; color: rgba(235, 235, 245, 0.5); }

.wiz-actions {
  margin-top: auto;
  display: flex;
  justify-content: space-between;
  gap: 8px;
  padding-top: 16px;
}

.wiz-steps { list-style: none; padding: 0; margin: 0 0 16px; }
.wiz-steps li {
  padding: 6px 0;
  color: rgba(235, 235, 245, 0.45);
  font-size: 0.9rem;
}
.wiz-steps li.ok { color: #34c759; }
.wiz-err { color: #ff6b6b; font-size: 0.85rem; }

:deep(.v-window-item) {
  height: 100%;
}
:deep(.v-window-x-transition-enter-active),
:deep(.v-window-x-transition-leave-active),
:deep(.v-window-x-reverse-transition-enter-active),
:deep(.v-window-x-reverse-transition-leave-active) {
  transition-duration: 0.3s !important;
  transition-timing-function: cubic-bezier(0.22, 1, 0.36, 1) !important;
}
</style>
