import "./polyfills";
import { createApp } from "vue";
import { appWindow } from "@tauri-apps/api/window";

/** Release shell. Same port as `shell_proxy::SHELL_PORT`. */
const SHELL_ORIGIN = "http://localhost:14321";

async function boot() {
  if (appWindow.label === "players") {
    const { default: PlayersHud } = await import("./PlayersHud.vue");
    createApp(PlayersHud).mount("#app");
    return;
  }

  // Packaged app origin is tauri://localhost. Material in a cross-origin iframe
  // stays blank (WKWebView reports 0×0, and the webview default is white).
  // The release shell proxies LMS on localhost so the iframe is same-origin.
  // The hostname must be localhost: Tauri's remote IPC allowlist ignores raw IPs.
  if (
    import.meta.env.PROD &&
    (location.protocol === "tauri:" || location.hostname === "tauri.localhost")
  ) {
    location.replace(SHELL_ORIGIN + "/");
    return;
  }

  const { default: App } = await import("./App.vue");
  const { createVuetify } = await import("vuetify");
  const components = await import("vuetify/components");
  const directives = await import("vuetify/directives");
  await import("vuetify/styles");

  const vuetify = createVuetify({
    components,
    directives,
    theme: {
      defaultTheme: "dark",
    },
  });

  createApp(App).use(vuetify).mount("#app");
}

void boot();
