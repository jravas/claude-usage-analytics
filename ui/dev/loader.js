// Dev harness loader: picks mock data, then loads the real ui/index.html (its
// markup, stylesheets and scripts, in order) so the preview never drifts from
// the app. Query parameters:
//   data=local|synthetic   default: local if mock.local.js exists
//   theme=light|dark       force a theme (default: follow the system)
//   state=…                see mock-invoke.js (empty, no-transcripts, indexing, error)
//   intro=1                show the first-run screen again

(async function () {
  const params = new URLSearchParams(location.search);

  // Static servers send no cache headers; bust the cache so edits show on reload.
  const v = `?v=${Date.now()}`;
  function load(src) {
    return new Promise((ok, fail) => {
      const s = document.createElement("script");
      s.src = src + v;
      s.onload = ok;
      s.onerror = () => fail(new Error(`could not load ${src}`));
      document.head.append(s);
    });
  }

  let data = params.get("data") || "local";
  if (data === "local") {
    try {
      await load("dev/mock.local.js");
    } catch {
      data = "synthetic";
    }
  }
  if (data === "synthetic") await load("dev/mock.js");
  await load("dev/mock-invoke.js");

  // Screenshot mode: `?shot=overview|session|insights|...` opens that view with
  // no first-run screen and no dev bar. Use with `data=synthetic` only.
  const shot = params.get("shot");
  if (shot) {
    try {
      localStorage.setItem("introSeen", "1");
      const prefs = JSON.parse(localStorage.getItem("prefs") || "{}");
      prefs.view = shot === "session-detail" ? "session" : shot;
      prefs.range = Number(params.get("range") || 28);
      prefs.unit = params.get("unit") || "cost";
      localStorage.setItem("prefs", JSON.stringify(prefs));
    } catch {}
  }
  const theme = params.get("theme");
  if (theme) document.documentElement.dataset.theme = theme;
  if (params.get("intro") === "1") {
    try {
      localStorage.removeItem("introSeen");
    } catch {}
  }

  const doc = new DOMParser().parseFromString(await (await fetch("index.html", { cache: "no-store" })).text(), "text/html");
  for (const l of doc.querySelectorAll('head link[rel="stylesheet"]')) {
    document.head.append(Object.assign(document.createElement("link"), { rel: "stylesheet", href: l.getAttribute("href") + v }));
  }
  const scripts = [...doc.body.querySelectorAll("script[src]")].map((s) => s.getAttribute("src"));
  doc.body.querySelectorAll("script").forEach((s) => s.remove());
  for (const a of doc.body.attributes) document.body.setAttribute(a.name, a.value);
  document.body.prepend(...[...doc.body.childNodes].map((n) => document.importNode(n, true)));
  window.__devLoading = true;
  for (const src of scripts) await load(src);
  window.__devLoading = false;
  window.dispatchEvent(new Event("dev-ready"));
  if (shot === "session-detail") {
    // Open the session with the highest cost in the range.
    setTimeout(async () => {
      const g = await window.mockInvoke("breakdown", { filter: filter(), dim: "session", limit: 50 });
      g.sort((a, b) => b.totals.costUsd - a.totals.costUsd);
      if (g[0]) go("session", g[0].key);
    }, 300);
  }
  if (shot) return;

  // ---------- dev bar ----------
  const bar = document.createElement("div");
  bar.className = "devbar";
  bar.setAttribute("aria-label", "Dev harness controls");
  const set = (k, v) => {
    const p = new URLSearchParams(location.search);
    if (v) p.set(k, v);
    else p.delete(k);
    location.search = p.toString();
  };
  const select = (label, key, options, value) => {
    const s = document.createElement("select");
    s.title = label;
    s.setAttribute("aria-label", label);
    for (const [v, t] of options) s.append(new Option(t, v, false, v === value));
    s.onchange = () => set(key, s.value);
    return s;
  };
  const btn = (text, fn) => Object.assign(document.createElement("button"), { textContent: text, onclick: fn });
  bar.append(
    Object.assign(document.createElement("b"), { textContent: "DEV" }),
    select("Mock data", "data", [["local", "Local data"], ["synthetic", "Synthetic data"]], data),
    select("Theme", "theme", [["", "System theme"], ["light", "Light"], ["dark", "Dark"]], theme || ""),
    select(
      "State",
      "state",
      [["", "Normal"], ["empty", "Empty index"], ["no-transcripts", "No transcripts"], ["indexing", "Indexing"], ["error", "Query error"]],
      params.get("state") || "",
    ),
    btn("index-updated", () => window.mockEmit("index-updated", { newRequests: 3 })),
    btn("First run", () => set("intro", "1")),
    btn("Hide", () => bar.remove()),
  );
  document.body.append(bar);
})().catch((e) => {
  document.body.textContent = `Dev harness failed: ${e.message}`;
});
