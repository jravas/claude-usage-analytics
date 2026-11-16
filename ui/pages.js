// Insights, Compare and Settings pages. Uses the helpers in app.js.

// ---------- Insights ----------
const insightsCache = new Map();
function loadInsights(f) {
  const k = JSON.stringify(f);
  if (!insightsCache.has(k)) {
    const p = invoke("insights", { filter: f });
    p.catch(() => insightsCache.delete(k));
    insightsCache.set(k, p);
  }
  return insightsCache.get(k);
}
listen("index-updated", () => insightsCache.clear());

// Where an insight's "Show me" goes. Views the app does not have yet fall back
// to the session or project the insight is about.
function evidenceTarget(ev) {
  const f = ev.filter || {};
  if (f.session) return () => go("session", f.session);
  const direct = { overview: "overview", sessions: "session", project: "project", effort: "effort", subagents: "agent" }[ev.view];
  if (direct) return () => go(direct);
  if (f.project) return () => go("project");
  return null;
}

async function renderInsights(body) {
  const r = await loadInsights(filter());
  body.append(
    el(
      "p",
      { class: "lead" },
      `What to change, ranked by how many tokens it accounts for in the ${rangeText()}. `,
      el("span", { class: "muted", text: `${fmtInt(r.sessions)} sessions checked.` }),
    ),
  );
  if (r.change) {
    body.append(
      el(
        "section",
        { class: "panel" },
        el("div", { class: "panel-head" }, el("h2", { text: "Why usage moved" }), el("span", { class: "muted", text: r.change.periods?.label || "" })),
        el("p", { class: "why", text: r.change.explanation }),
      ),
    );
  }
  if (!r.insights.length) {
    body.append(emptyState("Nothing stands out", `No rule fired for the ${rangeText()}. Try a longer range.`));
  }
  for (const i of r.insights) {
    const target = evidenceTarget(i.evidence);
    body.append(
      el(
        "article",
        { class: `insight ${i.severity}` },
        el(
          "div",
          { class: "insight-head" },
          el("span", { class: `chip ${i.severity}`, text: i.severity }),
          el("h3", { text: i.title + (i.subjectLabel ? `: ${i.subjectLabel}` : "") }),
          el("span", { class: "insight-amount", title: `${fmtInt(i.amount)} ${i.unit}` }, `${fmtTok(i.amount)} ${i.unit === "tokens" ? "tokens" : i.unit}`, el("span", { class: "muted", text: ` · ${fmtPct(i.share)}` })),
        ),
        el("p", { text: i.finding }),
        el("p", { class: "action", text: i.action }),
        target ? el("button", { class: "linklike", text: "Show me", onclick: target }) : null,
      ),
    );
  }
  if (r.skipped && r.skipped.length) {
    body.append(el("p", { class: "note", text: "Not checked yet: " + r.skipped.map((s) => `${s.title} (${s.reason})`).join("; ") + "." }));
  }
}

// ---------- Compare ----------
const compareState = { paths: null, result: null, family: "", name: "", toast: "", preview: null };

async function runCompare(paths) {
  const res = await invoke("compare", { family: compareState.family || null, paths: paths || null });
  if (!res) return; // cancelled
  const [p, cmp] = res;
  compareState.paths = p;
  compareState.result = cmp;
  render();
}

async function renderCompare(body) {
  // Share
  const nameInput = el("input", {
    type: "text",
    placeholder: "Your name (optional)",
    value: compareState.name,
    "data-fk": "share-name",
    oninput: (e) => (compareState.name = e.target.value),
  });
  const preview = compareState.preview ? el("details", { open: "" }, el("summary", { text: "What the file contains" }), el("pre", { class: "preview", text: compareState.preview })) : null;
  body.append(
    el(
      "section",
      { class: "panel" },
      el("div", { class: "panel-head" }, el("h2", { text: "Share your numbers" })),
      el("p", {
        class: "note top",
        text: "A summary file has your daily totals by model family and per-request averages for the last five weeks. It never contains paths, project names, session titles, branches, prompts or tool arguments.",
      }),
      el(
        "div",
        { class: "compare-bar" },
        nameInput,
        el("button", {
          class: "btn",
          text: "Preview",
          onclick: async () => {
            compareState.preview = await invoke("summary_preview", { name: compareState.name || null });
            render();
          },
        }),
        el("button", {
          class: "btn",
          text: "Export summary…",
          onclick: async () => {
            const p = await invoke("export_summary", { name: compareState.name || null });
            compareState.toast = p ? `Saved to ${p}` : "";
            render();
          },
        }),
      ),
      compareState.toast ? el("div", { class: "toast", role: "status", text: compareState.toast }) : null,
      preview,
    ),
  );

  // Compare
  const fam = el(
    "select",
    {
      "aria-label": "Model family",
      onchange: (e) => {
        compareState.family = e.target.value;
        if (compareState.paths) runCompare(compareState.paths);
      },
    },
    [
      ["", "All models"],
      ["opus", "Opus"],
      ["sonnet", "Sonnet"],
      ["haiku", "Haiku"],
      ["fable", "Fable"],
    ].map(([v, t]) => el("option", { value: v, text: t, selected: v === compareState.family ? "" : null })),
  );
  const section = el(
    "section",
    { class: "panel" },
    el("div", { class: "panel-head" }, el("h2", { text: "Compare with others" })),
    el("p", { class: "note top", text: "Ask colleagues to export their summary from this page, then open the files here. You are included automatically." }),
    el("div", { class: "compare-bar" }, el("button", { class: "btn", text: compareState.result ? "Choose other files…" : "Choose summary files…", onclick: () => runCompare(null) }), fam),
  );
  body.append(section);
  const cmp = compareState.result;
  if (!cmp) return;
  const weeks = cmp.weeks;
  const pct = (c) => (c == null ? "" : `${c > 0 ? "+" : c < 0 ? "−" : ""}${Math.abs(c).toFixed(0)}%`);
  const metric = (label, get, fmt, ch) =>
    DataTable({
      id: `compare:${label}`,
      name: `compare-${label.toLowerCase().replace(/\W+/g, "-")}`,
      label,
      rows: cmp.people,
      rowKey: (p) => p.label,
      tools: false,
      columns: [
        { key: "who", label: "Person", left: true, value: (p) => p.label },
        ...weeks.map((w, i) => ({
          key: `w${i}`,
          label: `Week of ${shortDay(w.start)}`,
          value: (p) => get(p.cells[i]),
          text: (p) => fmt(get(p.cells[i])),
          render: (p) => {
            const c = p.cells[i];
            if (!c || !c.daysCovered) return el("span", { class: "muted", text: "—" });
            return el("span", {}, fmt(get(c)) + (c.partial ? "*" : ""), el("span", { class: "cell-change", text: pct(ch(c)) }));
          },
        })),
      ],
    });
  section.append(
    el("h3", { class: "sub-head", text: "Total tokens per week" }),
    metric("Total tokens per week", (c) => c?.totals.total, fmtTok, (c) => c.change.total),
    el("h3", { class: "sub-head", text: "Context per request" }),
    metric("Context per request", (c) => c?.totals.contextPerRequest, fmtTok, (c) => c.change.contextPerRequest),
    el("h3", { class: "sub-head", text: "Output per request" }),
    metric("Output per request", (c) => c?.totals.outputPerRequest, fmtInt, (c) => c.change.outputPerRequest),
    el("p", { class: "note", text: `Files: ${compareState.paths.join(", ")}. * week only partly covered; its change is against the same weekdays a week earlier.` }),
  );
}

// ---------- Settings ----------
// Menu bar title modes: value (as in settings.rs MenuBar), label, hint.
const MENU_BAR_MODES = [
  ["usual", "Today compared with a usual day", "For example “1.4× usual”: today's weighted tokens against your median day of the same kind (weekday or weekend) over the last 4 weeks, up to the same time of day. Shows today's amount until there is enough history."],
  ["context", "Active session's context size", "Tokens the latest request of the session you are working in sent. Empty when no session was active in the last 30 minutes."],
  ["weighted", "Today's weighted tokens", "Tokens counted by price, so cache reads count little."],
  ["tokens", "Today's raw tokens", "Mostly cache reads."],
  ["cost", "Today's API-equivalent cost", "What the API would charge. Useful if you pay per token; not what a subscription bills."],
  ["icon", "Icon only", "No text."],
];
async function renderSettings(body) {
  const [s, prices] = await Promise.all([invoke("get_settings"), invoke("price_info").catch(() => null)]);
  const save = async (patch) => {
    try {
      await invoke("set_settings", { settings: { ...s, ...patch } });
      Object.assign(s, patch);
    } catch (e) {
      alert(String(e));
    }
    render();
  };
  const check = (key, label, hint, disabled = false) => [
    el("label", { for: `set-${key}` }, label, hint ? el("span", { class: "hint", text: hint }) : null),
    el("input", { type: "checkbox", id: `set-${key}`, "data-fk": `set-${key}`, checked: s[key] ? "" : null, disabled: disabled ? "" : null, onchange: (e) => save({ [key]: e.target.checked }) }),
  ];
  const num = (key, label, hint, step = 1, disabled = false) => [
    el("label", { for: `set-${key}` }, label, hint ? el("span", { class: "hint", text: hint }) : null),
    el("input", { type: "number", id: `set-${key}`, "data-fk": `set-${key}`, step: String(step), value: String(s[key]), disabled: disabled ? "" : null, onchange: (e) => save({ [key]: Number(e.target.value) }) }),
  ];
  const off = !s.notificationsEnabled;
  body.append(
    el(
      "section",
      { class: "panel" },
      el("div", { class: "panel-head" }, el("h2", { text: "Notifications" })),
      el("p", { class: "note top", text: "Off by default. Each one says what it cost and what to do about it, and repeats at most once per session in the cooldown." }),
      el(
        "div",
        { class: "form-grid" },
        check("notificationsEnabled", "Show notifications"),
        check("notifyContext", "Context is getting large", "When a session's latest request passes the threshold below.", off),
        num("contextThresholdPct", "Context threshold (%)", "Share of the context window.", 5, off),
        num("contextWindowTokens", "Context window (tokens)", "1,000,000 for the 1M models.", 1000, off),
        check("notifyCacheRewrite", "Cache rewritten after a pause", "A resumed session wrote its whole context to the cache again.", off),
        num("cacheRewriteMinTokens", "Minimum rewrite (tokens)", null, 10000, off),
        check("notifyDailySpike", "Unusually heavy day", "Today above a multiple of your median day over the last 4 weeks.", off),
        num("dailySpikeMultiple", "Daily multiple", null, 0.5, off),
        num("notifyCooldownMinutes", "Cooldown (minutes)", null, 5, off),
      ),
    ),
    el(
      "section",
      { class: "panel" },
      el("div", { class: "panel-head" }, el("h2", { text: "Menu bar" })),
      el("p", {
        class: "note top",
        text: "Plan-limit usage is not in local data, so the menu bar never shows it or guesses it. By default it compares today with your own usual day instead.",
      }),
      el(
        "div",
        { class: "form-grid" },
        el("label", { for: "set-menuBar" }, "Show next to the icon", el("span", { class: "hint", text: MENU_BAR_MODES.find(([v]) => v === s.menuBar)?.[2] || "" })),
        el(
          "select",
          { id: "set-menuBar", "data-fk": "set-menuBar", onchange: (e) => save({ menuBar: e.target.value }) },
          MENU_BAR_MODES.map(([v, t]) => el("option", { value: v, text: t, selected: v === s.menuBar ? "" : null })),
        ),
        check("launchAtLogin", "Open at login", "Starts in the menu bar with the window hidden."),
      ),
    ),
    el(
      "section",
      { class: "panel" },
      el("div", { class: "panel-head" }, el("h2", { text: "Prices" })),
      el("p", {
        class: "note top",
        text: prices
          ? `Costs use Anthropic's published API prices from ${prices.source}, retrieved ${prices.retrieved}.${prices.overridden ? " Your own prices.json override is in use." : " To use your own prices, put a prices.json in the app's data folder."} They show what the tokens would cost on the API, not what a subscription bills.`
          : "The price table could not be read.",
      }),
    ),
  );
}
