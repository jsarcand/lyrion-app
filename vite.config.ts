import fs from "node:fs";
import http from "node:http";
import os from "node:os";
import path from "node:path";
import { defineConfig, type Plugin } from "vite";
import vue from "@vitejs/plugin-vue";
import vuetify from "vite-plugin-vuetify";

type LmsTarget = { hostname: string; port: number };

let lastLms: LmsTarget = { hostname: "127.0.0.1", port: 9000 };

function parseLmsSearch(search: string): LmsTarget | null {
  try {
    const q = new URLSearchParams(search.startsWith("?") ? search.slice(1) : search);
    const h = (q.get("lmsHost") || q.get("lms") || "").trim();
    if (!h) return null;
    return { hostname: decodeURIComponent(h), port: Number(q.get("lmsPort")) || 9000 };
  } catch {
    return null;
  }
}

function lmsFromDisk(): LmsTarget {
  try {
    const p = path.join(os.homedir(), "Library/Application Support/org.lyrion.app/config.json");
    const cfg = JSON.parse(fs.readFileSync(p, "utf8"));
    const hostname = String(cfg?.server?.host || "127.0.0.1").trim() || "127.0.0.1";
    const port = Number(cfg?.server?.port) || 9000;
    return { hostname, port };
  } catch {
    return { hostname: "127.0.0.1", port: 9000 };
  }
}

/** Prefer the iframe's lmsHost query (and Referer for /jsonrpc /cometd), then disk. */
function lmsTarget(req?: { url?: string; headers?: http.IncomingHttpHeaders }): LmsTarget {
  if (req) {
    const url = req.url || "";
    const qi = url.indexOf("?");
    if (qi >= 0) {
      const fromQ = parseLmsSearch(url.slice(qi));
      if (fromQ) {
        lastLms = fromQ;
        return lastLms;
      }
    }
    const ref = String(req.headers?.referer || "");
    try {
      const fromRef = parseLmsSearch(new URL(ref).search);
      if (fromRef) {
        lastLms = fromRef;
        return lastLms;
      }
    } catch {
      /* ignore */
    }
  }
  lastLms = lmsFromDisk();
  return lastLms;
}

function isLmsPath(url: string): boolean {
  const u = (url.split("?")[0] || "");
  return (
    u === "/jsonrpc.js" ||
    u.startsWith("/material") ||
    u.startsWith("/html") ||
    u.startsWith("/plugins") ||
    u.startsWith("/music") ||
    u.startsWith("/imageproxy") ||
    u.startsWith("/cometd") ||
    u.startsWith("/status") ||
    u.startsWith("/repo") ||
    u.startsWith("/update") ||
    u.startsWith("/slim")
  );
}

const HOP_BY_HOP = new Set([
  "connection",
  "keep-alive",
  "proxy-authenticate",
  "proxy-authorization",
  "proxy-connection",
  "te",
  "trailer",
  "transfer-encoding",
  "upgrade",
  "accept-encoding",
]);

/** Cap concurrent LMS sockets. Node's global Agent is unbounded and floods pCP. */
const lmsAgent = new http.Agent({ keepAlive: false, maxSockets: 6, timeout: 15000 });

function copyLmsHeaders(src: http.IncomingHttpHeaders, hostname: string, port: number): http.OutgoingHttpHeaders {
  const headers: http.OutgoingHttpHeaders = { host: `${hostname}:${port}`, connection: "close" };
  for (const [k, v] of Object.entries(src)) {
    if (v == null || HOP_BY_HOP.has(k.toLowerCase())) continue;
    headers[k] = v;
  }
  return headers;
}

function retryable(err: NodeJS.ErrnoException): boolean {
  const c = err.code || "";
  const m = String(err.message || "");
  return (
    c === "ECONNRESET" ||
    c === "ECONNREFUSED" ||
    c === "EPIPE" ||
    c === "ETIMEDOUT" ||
    c === "EHOSTUNREACH" ||
    c === "ENETUNREACH" ||
    m.includes("hang up") ||
    m.includes("timeout")
  );
}

function readBody(req: http.IncomingMessage, limit = 2_000_000): Promise<Buffer> {
  return new Promise((resolve, reject) => {
    const chunks: Buffer[] = [];
    let n = 0;
    req.on("data", (c: Buffer) => {
      n += c.length;
      if (n > limit) {
        req.destroy();
        reject(new Error("body too large"));
        return;
      }
      chunks.push(c);
    });
    req.on("end", () => resolve(Buffer.concat(chunks)));
    req.on("error", reject);
  });
}

function proxyOnce(
  opts: {
    hostname: string;
    port: number;
    path: string;
    method?: string;
    headers: http.OutgoingHttpHeaders;
    timeout: number;
    agent: http.Agent | boolean;
    body?: Buffer;
    streamIn?: http.IncomingMessage;
  },
  res: http.ServerResponse
): Promise<void> {
  return new Promise((resolve, reject) => {
    const p = http.request(
      {
        hostname: opts.hostname,
        port: opts.port,
        path: opts.path,
        method: opts.method,
        headers: opts.headers,
        timeout: opts.timeout,
        agent: opts.agent,
        family: 4,
      },
      (pr) => {
        const out = { ...pr.headers };
        delete out["content-encoding"];
        delete out.connection;
        delete out["keep-alive"];
        res.writeHead(pr.statusCode || 502, out);
        pr.pipe(res);
        pr.on("end", () => resolve());
        pr.on("error", reject);
      }
    );
    p.on("timeout", () => p.destroy(Object.assign(new Error("timeout"), { code: "ETIMEDOUT" })));
    p.on("error", reject);
    if (opts.streamIn) opts.streamIn.pipe(p);
    else p.end(opts.body && opts.body.length ? opts.body : undefined);
  });
}

function failProxy(res: http.ServerResponse, hostname: string, port: number, err: NodeJS.ErrnoException) {
  console.warn("LMS proxy", hostname + ":" + port, err.message);
  if (!res.headersSent) {
    res.writeHead(502, { "content-type": "text/plain; charset=utf-8" });
    res.end("LMS proxy error: " + hostname + ":" + port + " " + err.message);
  }
}

/** Same-origin LMS proxy so the Material iframe is not cross-origin to Vue.
 * WKWebView RecycleScroller (Accueil) never lays out in a cross-origin iframe. */
function lmsProxyPlugin(): Plugin {
  return {
    name: "lms-proxy",
    configureServer(server) {
      server.middlewares.use((req, res, next) => {
        const url = req.url || "";
        if (!isLmsPath(url)) return next();
        const { hostname, port } = lmsTarget(req);
        const stream = url.startsWith("/cometd");
        const headers = copyLmsHeaders(req.headers, hostname, port);
        if (stream) {
          headers.connection = "keep-alive";
          void proxyOnce(
            { hostname, port, path: url, method: req.method, headers, timeout: 120000, agent: false, streamIn: req },
            res
          ).catch((err) => failProxy(res, hostname, port, err));
          return;
        }
        void (async () => {
          const body = await readBody(req);
          if (body.length) headers["content-length"] = body.length;
          else delete headers["content-length"];
          let last: NodeJS.ErrnoException | null = null;
          for (let attempt = 0; attempt < 2; attempt++) {
            try {
              await proxyOnce(
                { hostname, port, path: url, method: req.method, headers, timeout: 15000, agent: lmsAgent, body },
                res
              );
              return;
            } catch (e) {
              last = e as NodeJS.ErrnoException;
              if (attempt === 0 && !res.headersSent && retryable(last)) {
                await new Promise((r) => setTimeout(r, 200));
                continue;
              }
              break;
            }
          }
          failProxy(res, hostname, port, last || new Error("proxy failed"));
        })().catch((err) => failProxy(res, hostname, port, err));
      });
    },
  };
}

// https://vitejs.dev/config/
export default defineConfig(async () => ({
  plugins: [
    vue(),
    vuetify({autoImport: true}),
    lmsProxyPlugin(),
  ],

  // Catalina WKWebView is Safari 13. The default Vite target emits syntax it rejects,
  // so the document stays on the black shell and the wizard never mounts.
  build: {
    target: "safari13",
  },

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    // Bind IPv4 loopback. Vite's default `localhost` is [::1] only on recent
    // Node, and WKWebView often resolves localhost to 127.0.0.1 — blank window.
    host: "127.0.0.1",
    port: 1420,
    strictPort: true,
    watch: {
      // 3. tell vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
    },
  },
}));
