// Claude Usage Analytics — front end. Vanilla JS, no build step.
// Load order (index.html): glossary.js, charts.js, table.js, app.js,
// session.js, learn.js. Later files use the helpers defined here.

const tauri = window.__TAURI__;
if (!tauri) document.documentElement.classList.add("web");
const invoke = tauri
  ? tauri.core.invoke
  : async (cmd, args) => {
      if (window.mockInvoke) return window.mockInvoke(cmd, args);
      throw new Error("not running inside Tauri");
    };
const listen = tauri
  ? tauri.event.listen
  : async (name, cb) => (window.mockListen ? window.mockListen(name, cb) : () => {});

const VIEWS = {
  overview: "Overview",
  model: "Models",
  project: "Projects",
  session: "Sessions",
  effort: "Effort",
  agent: "Main vs subagents",
  hour: "Time of day",
  insights: "Insights",
  compare: "Compare",
  settings: "Settings",
  learn: "How usage works",
};
// Singular column heading and glossary term per breakdown.
const DIM_COL = { model: "Model", project: "Project", session: "Session", effort: "Effort", agent: "Thread", hour: "Hour" };

// What "how much" means everywhere a total is shown. Raw tokens are mostly
// cheap cache reads, so cost is the default.
const UNITS = {
  cost: { name: "Cost", long: "API-equivalent cost", get: (t) => t.costUsd, fmt: (v) => fmtUsd(v) },
  weighted: { name: "Weighted tokens", long: "price-weighted tokens", get: (t) => t.weighted, fmt: fmtTok },
  tokens: { name: "Tokens", long: "tokens", get: (t) => t.total, fmt: fmtTok },
};
const U = () => UNITS[state.unit] || UNITS.cost;

function fmtUsd(x) {
  if (x == null || !isFinite(x)) return "—";
  const a = Math.abs(x);
  if (a >= 1000) return "$" + (x / 1000).toFixed(1) + "k";
  if (a >= 100) return "$" + x.toFixed(0);
  if (a >= 1) return "$" + x.toFixed(2);
  if (a === 0) return "$0";
  return "$" + x.toFixed(3);
}

const METRICS = {
  total: { name: "Total", get: (t) => U().get(t), fmt: (v) => U().fmt(v) },
  requests: { name: "Requests", get: (t) => t.requests, fmt: fmtInt },
  output: { name: "Output", get: (t) => t.output, fmt: fmtTok },
  context: { name: "Context / req", get: (t) => t.contextPerRequest, fmt: fmtTok },
};
// Token types for the split daily chart, bottom first.
const TOKEN_SERIES = [
  { key: "cacheRead", name: "Cache read", slot: 1 },
  { key: "cacheWrite", name: "Cache write", slot: 2 },
  { key: "input", name: "Input", slot: 3 },
  { key: "output", name: "Output", slot: 4 },
];

const state = Object.assign(
  { view: "overview", model: "", range: 14, weekdays: false, metric: "total", split: false, unit: "cost" },
  loadPrefs(),
  { models: [], busy: false, refreshedAt: null, error: null, session: null, filesSeen: null, progress: null },
);
if (!VIEWS[state.view]) state.view = "overview";

// ---------- helpers ----------
const $ = (s) => document.querySelector(s);

function el(tag, attrs = {}, ...children) {
  const n = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (k === "class") n.className = v;
    else if (k === "text") n.textContent = v;
    else if (k.startsWith("on")) n.addEventListener(k.slice(2), v);
    else if (v !== null && v !== undefined && v !== false) n.setAttribute(k, v);
  }
  for (const c of children.flat()) {
    if (c === null || c === undefined || c === false) continue;
    n.append(c.nodeType ? c : document.createTextNode(String(c)));
  }
  return n;
}

const term = (name, label) => Glossary.term(name, label);
const showTip = (e, t) => Charts.showTip(e, t);
const hideTip = () => Charts.hideTip();

function fmtTok(n) {
  if (n == null || Number.isNaN(n)) return "—";
  const a = Math.abs(n);
  if (a >= 1e9) return (n / 1e9).toFixed(2) + "B";
  if (a >= 1e6) return (n / 1e6).toFixed(1) + "M";
  if (a >= 1e3) return (n / 1e3).toFixed(1) + "k";
  return String(Math.round(n));
}

function fmtInt(n) {
  return Math.round(n || 0).toLocaleString("en-US");
}

function fmtPct(x) {
  if (x == null || !isFinite(x)) return "—";
  return (x * 100).toFixed(Math.abs(x) >= 0.1 ? 0 : 1) + "%";
}

function change(before, after) {
  return before > 0 ? ((after - before) / before) * 100 : null;
}

function fmtChange(c) {
  return `${c > 0 ? "+" : c < 0 ? "−" : ""}${Math.abs(c).toFixed(0)}%`;
}

// Up/down is said in words and arrows as well as colour.
function delta(before, after, what = "") {
  const c = change(before, after);
  if (c === null) return null;
  const cls = Math.abs(c) < 2 ? "" : c > 0 ? "up" : "down";
  const arrow = Math.abs(c) < 2 ? "" : c > 0 ? "▲ " : "▼ ";
  return el("span", { class: `delta ${cls}`, title: `${fmtChange(c)} ${what}`.trim() }, el("span", { "aria-hidden": "true", text: arrow }), fmtChange(c));
}

function isoDay(d) {
  const p = (x) => String(x).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
}

function today() {
  return isoDay(new Date());
}

function daysAgo(n) {
  const d = new Date();
  d.setDate(d.getDate() - n);
  return isoDay(d);
}

function shortDay(day) {
  const [, m, d] = day.split("-");
  return `${Number(d)}.${Number(m)}.`;
}

function loadPrefs() {
  try {
    return JSON.parse(localStorage.getItem("prefs") || "{}");
  } catch {
    return {};
  }
}

function savePrefs() {
  try {
    const { view, model, range, weekdays, metric, split, unit } = state;
    localStorage.setItem("prefs", JSON.stringify({ view, model, range, weekdays, metric, split, unit }));
  } catch {
    /* storage unavailable: preferences just don't persist */
  }
}

function filter(extra = {}) {
  return {
    from: daysAgo(state.range - 1),
    to: today(),
    weekdaysOnly: state.weekdays,
    model: state.model || null,
    ...extra,
  };
}

function rangeText() {
  return `last ${state.range} days${state.weekdays ? ", weekdays" : ""}${state.model ? `, ${state.model}` : ""}`;
}

function emptyState(title, detail, ...actions) {
  return el("div", { class: "empty", role: "status" }, el("h2", { text: title }), detail ? el("p", { text: detail }) : null, actions.length ? el("div", { class: "empty-actions" }, actions) : null);
}

function linkButton(text, onclick) {
  return el("button", { class: "btn", text, onclick });
}

function go(view, session = null) {
  state.view = view;
  state.session = session;
  savePrefs();
  render();
  $("#body").scrollTop = 0;
}

// ---------- views ----------
async function renderOverview(body) {
  const f = filter();
  const [o, days] = await Promise.all([invoke("overview", { filter: f }), invoke("daily", { filter: f })]);
  const anyInRange = days.some((d) => d.totals.requests);
  if (!anyInRange && !o.thisWeek.requests && !o.lastWeek.requests) {
    body.append(noData());
    return;
  }
  const wd = new Date().toLocaleDateString("en-US", { weekday: "short" });
  const t = o.thisWeek,
    l = o.lastWeekToDate;
  const u = U();
  const spark = () =>
    Charts.sparkline(
      days.slice(-14).map((d) => u.get(d.totals)),
      { label: `Daily ${u.name.toLowerCase()} over the last ${Math.min(14, days.length)} days`, width: 84 },
    );

  const cards = el(
    "div",
    { class: "cards" },
    el(
      "div",
      { class: "card" },
      el("h3", {}, term(u.name, "Today")),
      el("div", { class: "big" }, u.fmt(u.get(o.todayTotals)), delta(u.get(o.sameDayLastWeek), u.get(o.todayTotals), `vs last ${wd} at the same time`)),
      el("div", { class: "sub", text: `${fmtInt(o.todayTotals.requests)} requests · last ${wd} by ${o.asOf}: ${u.fmt(u.get(o.sameDayLastWeek))}` }),
    ),
    el(
      "div",
      { class: "card" },
      el("h3", {}, term(u.name, "This week so far")),
      el("div", { class: "big" }, u.fmt(u.get(t)), delta(u.get(l), u.get(t), "vs last week to the same time")),
      el("div", { class: "sub", text: `${fmtInt(t.requests)} requests · last week to ${wd} ${o.asOf}: ${u.fmt(u.get(l))}` }),
    ),
    el(
      "div",
      { class: "card" },
      el("h3", {}, term(u.name, "Last week")),
      el("div", { class: "big", text: u.fmt(u.get(o.lastWeek)) }),
      el("div", { class: "sub", text: `${fmtInt(o.lastWeek.requests)} requests · ${fmtTok(o.lastWeek.total)} tokens` }),
      el("div", { class: "card-spark", title: `${u.name} per day, last 14 days` }, spark()),
    ),
    el(
      "div",
      { class: "card" },
      el("h3", { text: "Per request vs last week" }),
      el(
        "div",
        { class: "kv" },
        el("span", { class: "k" }, term("Output / req", "Output")),
        el("span", { text: fmtTok(t.outputPerRequest) }),
        delta(o.lastWeek.outputPerRequest, t.outputPerRequest, "vs last week") || el("span"),
        el("span", { class: "k" }, term("Context / req", "Context")),
        el("span", { text: fmtTok(t.contextPerRequest) }),
        delta(o.lastWeek.contextPerRequest, t.contextPerRequest, "vs last week") || el("span"),
        el("span", { class: "k" }, term("Cost", "Cost")),
        el("span", { text: fmtUsd(t.costPerRequest) }),
        delta(o.lastWeek.costPerRequest, t.costPerRequest, "vs last week") || el("span"),
        el("span", { class: "k" }, term("Cache hit")),
        el("span", { text: fmtPct(t.cacheHitRate) }),
        el("span", { class: "muted", title: "Last week", text: fmtPct(o.lastWeek.cacheHitRate) }),
      ),
    ),
  );
  body.append(cards);

  const why = whyChanged(l, t, wd, o.asOf);
  if (why) body.append(why);

  // ---------- by day ----------
  const m = METRICS[state.metric] || METRICS.total;
  const seg = el(
    "div",
    { class: "seg", role: "radiogroup", "aria-label": "Daily metric" },
    Object.entries(METRICS).map(([k, v]) =>
      el("button", {
        class: k === state.metric ? "on" : "",
        role: "radio",
        "aria-checked": String(k === state.metric),
        "data-fk": `metric:${k}`,
        text: k === "total" ? U().name : v.name,
        onclick: () => {
          state.metric = k;
          savePrefs();
          render();
        },
      }),
    ),
  );
  const canSplit = state.metric === "total" && state.unit === "tokens";
  const split = canSplit
    ? el(
        "label",
        { class: "toggle" },
        el("input", {
          type: "checkbox",
          "data-fk": "split",
          checked: state.split ? "" : null,
          onchange: (e) => {
            state.split = e.target.checked;
            savePrefs();
            render();
          },
        }),
        "By token type",
      )
    : null;
  const todayIso = today();
  const label = (d) => `${d.weekday.slice(0, 2)} ${shortDay(d.day)}`;
  const peak = days.reduce((a, d) => (m.get(d.totals) > m.get(a.totals) ? d : a), days[0]);
  const sumAll = days.reduce((a, d) => a + d.totals.total, 0);
  const allU = days.reduce((a, d) => a + (u.get(d.totals) || 0), 0);
  const summary = `${m.name === "Total" ? u.name : m.name} per day, ${rangeText()}. Highest on ${peak.weekday} ${peak.day} at ${m.fmt(m.get(peak.totals))}. ${days.filter((d) => d.totals.requests).length} of ${days.length} days had usage. ${u.fmt(allU)} in all (${fmtTok(sumAll)} tokens).`;
  const chart =
    canSplit && state.split
      ? Charts.stackedBar(
          days.map((d) => ({
            label: label(d),
            values: Object.fromEntries(TOKEN_SERIES.map((s) => [s.key, d.totals[s.key]])),
            tip: dayTip(d),
          })),
          TOKEN_SERIES,
          { fmt: fmtTok, label: "Tokens per day by type", summary: summary + ` Cache reads are ${fmtPct(days.reduce((a, d) => a + d.totals.cacheRead, 0) / (sumAll || 1))} of all tokens.` },
        )
      : Charts.bar(
          days.map((d) => ({
            label: label(d),
            value: m.get(d.totals),
            cls: [d.day === todayIso ? "today" : "", d.weekday === "Sat" || d.weekday === "Sun" ? "weekend" : ""].join(" "),
            tip: dayTip(d),
          })),
          { fmt: m.fmt, label: `${m.name === "Total" ? u.name : m.name} per day`, summary },
        );
  body.append(el("section", { class: "panel" }, el("div", { class: "panel-head" }, el("h2", { text: "By day" }), split, seg), chart));

  const ok = (d, v) => (d.totals.requests ? v : null);
  body.append(
    DataTable({
      id: "daily",
      name: `daily-${state.range}d`,
      label: "Usage by day",
      rows: [...days].reverse(),
      rowKey: (d) => d.day,
      rowClass: (d) => `${d.totals.requests ? "" : "zero"} ${d.day === todayIso ? "today" : ""}`,
      columns: [
        { key: "day", label: "Day", left: true, value: (d) => d.day, text: (d) => `${d.weekday} ${shortDay(d.day)}` },
        { key: "requests", label: "Requests", value: (d) => d.totals.requests, text: (d) => fmtInt(d.totals.requests) },
        { key: "cost", label: "Cost", value: (d) => d.totals.costUsd, text: (d) => fmtUsd(d.totals.costUsd) },
        { key: "output", label: "Output", value: (d) => d.totals.output, text: (d) => fmtTok(d.totals.output) },
        { key: "cacheWrite", label: "Cache write", value: (d) => d.totals.cacheWrite, text: (d) => fmtTok(d.totals.cacheWrite) },
        { key: "cacheRead", label: "Cache read", value: (d) => d.totals.cacheRead, text: (d) => fmtTok(d.totals.cacheRead) },
        { key: "total", label: "Total", term: "Total tokens", value: (d) => d.totals.total, text: (d) => fmtTok(d.totals.total) },
        { key: "opr", label: "Output / req", value: (d) => ok(d, d.totals.outputPerRequest), text: (d) => (d.totals.requests ? fmtInt(d.totals.outputPerRequest) : "—") },
        { key: "cpr", label: "Context / req", value: (d) => ok(d, d.totals.contextPerRequest), text: (d) => (d.totals.requests ? fmtTok(d.totals.contextPerRequest) : "—") },
        { key: "hit", label: "Cache hit", value: (d) => ok(d, d.totals.cacheHitRate), text: (d) => (d.totals.requests ? fmtPct(d.totals.cacheHitRate) : "—") },
      ],
    }),
    el(
      "p",
      { class: "note" },
      "Days are in local time. Each API response counts once. Context is what the model read per request: input plus cache writes plus cache reads. " +
        "Only Claude Code sessions on this Mac are included; claude.ai chats and plan-limit percentages are not in the local logs. ",
      el("button", { class: "linklike", text: "How usage works", onclick: () => go("learn") }),
    ),
  );
}

// Week to date vs last week to the same time, split into: more or fewer
// requests, heavier or lighter context per request, more or less output per
// request. total = requests × (context/req + output/req), so the three parts
// add up exactly.
function whyChanged(l, t, wd, asOf) {
  if (!l.requests || !t.requests) return null;
  const u = U();
  const cReq = change(l.requests, t.requests);
  const cCtx = change(l.contextPerRequest, t.contextPerRequest);
  const verb = (c, up, down, same) => (Math.abs(c) < 5 ? same : c > 0 ? up : down);
  const sign = (v) => `${v >= 0 ? "+" : "−"}${u.fmt(Math.abs(v))}`;
  let steps, sentence;
  if (state.unit === "tokens") {
    // total = requests × (context/req + output/req), so the parts add up exactly.
    const p0 = l.contextPerRequest + l.outputPerRequest;
    const eReq = (t.requests - l.requests) * p0;
    const eCtx = t.requests * (t.contextPerRequest - l.contextPerRequest);
    const eOut = t.requests * (t.outputPerRequest - l.outputPerRequest);
    sentence =
      `Requests ${fmtChange(cReq)}, context per request ${fmtChange(cCtx)}: ` +
      verb(cReq, "you made more requests", "you made fewer requests", "about as many requests") +
      "; " +
      verb(cCtx, "each one carried more context", "each one carried less context", "each one was about the same size") +
      ".";
    steps = [
      { label: "Requests", sub: fmtChange(cReq), value: eReq, tip: `More or fewer requests, at last week's size\n${sign(eReq)}` },
      { label: "Context / req", sub: fmtChange(cCtx), value: eCtx, tip: `Heavier or lighter context per request\n${sign(eCtx)}` },
      { label: "Output / req", sub: fmtChange(change(l.outputPerRequest, t.outputPerRequest) ?? 0), value: eOut, tip: `More or less output per request\n${sign(eOut)}` },
    ];
  } else {
    // amount = requests × amount per request.
    const per0 = u.get(l) / l.requests;
    const per1 = u.get(t) / t.requests;
    const eReq = (t.requests - l.requests) * per0;
    const ePer = t.requests * (per1 - per0);
    const cPer = change(per0, per1);
    sentence =
      `Requests ${fmtChange(cReq)}, ${u.name.toLowerCase()} per request ${fmtChange(cPer)} (context per request ${fmtChange(cCtx)}): ` +
      verb(cReq, "you made more requests", "you made fewer requests", "about as many requests") +
      "; " +
      verb(cPer, "each one cost more", "each one cost less", "each one cost about the same") +
      ".";
    steps = [
      { label: "Requests", sub: fmtChange(cReq), value: eReq, tip: `More or fewer requests, at last week's ${u.name.toLowerCase()} per request\n${sign(eReq)}` },
      { label: "Per request", sub: fmtChange(cPer), value: ePer, tip: `Each request heavier or lighter (context, output, cache)\n${sign(ePer)}` },
    ];
  }
  const cTot = change(u.get(l), u.get(t));
  steps = [
    { label: "Last week", sub: `to ${wd} ${asOf}`, value: u.get(l), total: true },
    ...steps,
    { label: "This week", sub: fmtChange(cTot ?? 0), value: u.get(t), total: true },
  ];
  return el(
    "section",
    { class: "panel" },
    el("div", { class: "panel-head" }, el("h2", { text: "Why did it change?" }), el("span", { class: "muted", text: `this week so far vs last week to the same time, in ${u.long}` })),
    el("p", { class: "why" }, sentence),
    Charts.waterfall(steps, { fmt: u.fmt, height: 190, label: "Change from last week to this week", summary: sentence + ` Total ${u.fmt(u.get(l))} to ${u.fmt(u.get(t))}.` }),
  );
}

function dayTip(d) {
  const t = d.totals;
  return [
    `${d.weekday} ${d.day}`,
    `${fmtInt(t.requests)} requests`,
    `total ${fmtTok(t.total)}`,
    `output ${fmtTok(t.output)} (thinking ${fmtTok(t.thinking)})`,
    `cache write ${fmtTok(t.cacheWrite)} · read ${fmtTok(t.cacheRead)}`,
  ].join("\n");
}

function noData() {
  if (state.filesSeen === 0) return noTranscripts();
  if (state.progress) return indexing();
  return emptyState(
    "No usage in this range",
    `Nothing matched the ${rangeText()}.`,
    state.range < 90
      ? linkButton("Show the last 90 days", () => {
          state.range = 90;
          $("#range").value = "90";
          savePrefs();
          render();
        })
      : null,
    state.model || state.weekdays
      ? linkButton("Clear filters", () => {
          state.model = "";
          state.weekdays = false;
          $("#model").value = "";
          $("#weekdays").checked = false;
          savePrefs();
          render();
        })
      : null,
  );
}

function noTranscripts() {
  return emptyState(
    "No Claude Code transcripts found",
    "The app reads the transcripts Claude Code writes to ~/.claude/projects (or $CLAUDE_CONFIG_DIR/projects). Run a Claude Code session on this Mac, then refresh.",
    linkButton("Refresh", refresh),
    linkButton("What this app reads", () => showIntro()),
  );
}

function indexing() {
  const p = state.progress;
  return emptyState("Reading your transcripts…", p ? `${fmtInt(p.done)} of ${fmtInt(p.total)} files. Totals appear as soon as this finishes.` : "Totals appear as soon as this finishes.");
}

// ---------- breakdowns ----------
const sparkCache = new Map();
async function sparkFor(dim, key) {
  const f = filter(dim === "project" ? { project: key } : { session: key });
  const k = JSON.stringify([dim, key, f, state.unit]);
  if (!sparkCache.has(k)) sparkCache.set(k, invoke("daily", { filter: f }).then((ds) => ds.map((d) => U().get(d.totals))));
  return sparkCache.get(k);
}

async function renderBreakdown(body, dim) {
  const groups = await invoke("breakdown", { filter: filter(), dim, limit: dim === "session" ? 100 : 200 });
  if (!groups.length) {
    body.append(noData());
    return;
  }
  // Shares follow the selected unit: cost shares differ a lot from token shares.
  const uu = U();
  const sumU = groups.reduce((a, g) => a + (uu.get(g.totals) || 0), 0) || 1;
  for (const g of groups) g.share = (uu.get(g.totals) || 0) / sumU;
  if (state.unit !== "tokens" && dim !== "hour") groups.sort((a, b) => (uu.get(b.totals) || 0) - (uu.get(a.totals) || 0));
  if (dim === "hour") await renderTime(body, groups);
  const top = groups[0];
  body.append(
    el(
      "p",
      { class: "lead" },
      dim === "hour" ? `${groups.length} of 24 hours had usage in the ${rangeText()}. ` : `${groups.length} ${{ model: "models", project: "projects", session: "sessions", effort: "effort levels", agent: "threads" }[dim]} in the ${rangeText()}. `,
      dim === "hour" ? `Busiest hour: ${top && [...groups].sort((a, b) => uu.get(b.totals) - uu.get(a.totals))[0].label}.` : `Largest: ${top.label}, ${fmtPct(top.share)} of all ${uu.long}.`,
    ),
  );
  // Sparklines need a per-key daily query; the core can filter by project and session.
  const withSpark = dim === "project" || dim === "session";
  const SPARK_ROWS = 30;
  const sparkRank = new Map(groups.slice(0, SPARK_ROWS).map((g, i) => [g.key, i]));
  const cols = [
    {
      key: "label",
      label: DIM_COL[dim],
      term: DIM_COL[dim] === "Thread" ? "Main vs subagents" : DIM_COL[dim],
      left: true,
      value: (g) => (dim === "hour" ? g.key : g.label),
      text: (g) => (g.detail && g.detail !== g.label ? `${g.label} (${g.detail})` : g.label),
      render: (g) => el("span", { class: "cell-label", title: g.detail || g.key }, g.label, g.detail && g.detail !== g.label ? el("small", { text: g.detail }) : null),
    },
    { key: "requests", label: "Requests", value: (g) => g.totals.requests, text: (g) => fmtInt(g.totals.requests) },
    { key: "cost", label: "Cost", value: (g) => g.totals.costUsd, text: (g) => fmtUsd(g.totals.costUsd) },
    { key: "output", label: "Output", value: (g) => g.totals.output, text: (g) => fmtTok(g.totals.output) },
    { key: "cacheRead", label: "Cache read", value: (g) => g.totals.cacheRead, text: (g) => fmtTok(g.totals.cacheRead) },
    { key: "total", label: "Total", term: "Total tokens", value: (g) => g.totals.total, text: (g) => fmtTok(g.totals.total) },
    {
      key: "share",
      label: "Share",
      term: `Share of ${uu.long}`,
      width: "120px",
      value: (g) => g.share,
      text: (g) => fmtPct(g.share),
      render: (g) =>
        el("div", { class: "share" }, el("span", { text: fmtPct(g.share) }), el("div", { class: "track", "aria-hidden": "true" }, el("div", { class: "fill", style: `width:${(g.share * 100).toFixed(1)}%` }))),
    },
    { key: "opr", label: "Output / req", value: (g) => g.totals.outputPerRequest, text: (g) => fmtInt(g.totals.outputPerRequest) },
    { key: "cpr", label: "Context / req", value: (g) => g.totals.contextPerRequest, text: (g) => fmtTok(g.totals.contextPerRequest) },
  ];
  if (withSpark) {
    cols.push({
      key: "trend",
      label: "Trend",
      width: "96px",
      value: () => null,
      text: () => "",
      render: (g) => {
        const span = el("span", { class: "spark-cell", "aria-hidden": "true" });
        if (sparkRank.has(g.key)) sparkFor(dim, g.key).then((v) => span.replaceChildren(Charts.sparkline(v, { width: 80, label: `${g.label}, tokens per day` })));
        return span;
      },
    });
  }
  body.append(
    DataTable({
      id: `breakdown:${dim}`,
      name: `${dim}-${state.range}d`,
      label: `Usage by ${DIM_COL[dim].toLowerCase()}`,
      rows: groups,
      rowKey: (g) => g.key,
      columns: cols,
      onOpen: dim === "session" ? (g) => go("session", g.key) : null,
    }),
  );
  if (withSpark) body.append(el("p", { class: "note", text: `Trend: tokens per day over the ${rangeText()}, for the top ${SPARK_ROWS} rows.` }));
}

async function renderTime(body, groups) {
  const byHour = new Map(groups.map((g) => [g.key, g]));
  const hours = [];
  for (let h = 0; h < 24; h++) {
    const k = String(h).padStart(2, "0");
    const g = byHour.get(k);
    hours.push({ label: k, value: g ? g.totals.total : 0, tip: `${k}:00–${k}:59\n${fmtTok(g ? g.totals.total : 0)} tokens · ${fmtInt(g ? g.totals.requests : 0)} requests` });
  }
  const busiest = [...hours].sort((a, b) => b.value - a.value)[0];
  const quiet = hours.filter((h) => !h.value).length;
  // Calendar: the last 12 full weeks plus this one, independent of the range.
  const weeks = 13;
  const start = new Date();
  start.setDate(start.getDate() - ((start.getDay() + 6) % 7) - (weeks - 1) * 7);
  const [wdGroups, cal] = await Promise.all([
    invoke("breakdown", { filter: filter(), dim: "weekday", limit: 7 }).catch(() => []),
    invoke("daily", { filter: { ...filter(), from: isoDay(start), to: today() } }),
  ]);
  const WD = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
  const wdItems = [1, 2, 3, 4, 5, 6, 0].map((i) => {
    const g = wdGroups.find((x) => x.key === String(i));
    return { label: WD[i], value: g ? g.totals.total : 0, cls: i === 0 || i === 6 ? "weekend" : "", tip: `${WD[i]}\n${fmtTok(g ? g.totals.total : 0)} tokens · ${fmtInt(g ? g.totals.requests : 0)} requests` };
  });
  const byDay = new Map(cal.map((d) => [d.day, d]));
  const cells = [];
  const colLabels = [];
  for (let w = 0; w < weeks; w++) {
    const monday = new Date(start);
    monday.setDate(monday.getDate() + w * 7);
    colLabels.push(monday.getDate() <= 7 ? monday.toLocaleDateString("en-US", { month: "short" }) : "");
    for (let r = 0; r < 7; r++) {
      const day = new Date(monday);
      day.setDate(day.getDate() + r);
      const iso = isoDay(day);
      const d = byDay.get(iso);
      const future = iso > today();
      cells.push({
        row: r,
        col: w,
        value: d ? d.totals.total : 0,
        empty: future,
        tip: future ? "" : `${WD[day.getDay()]} ${iso}\n${fmtTok(d ? d.totals.total : 0)} tokens · ${fmtInt(d ? d.totals.requests : 0)} requests`,
      });
    }
  }
  const activeDays = cal.filter((d) => d.totals.requests).length;
  body.append(
    el(
      "section",
      { class: "panel" },
      el("div", { class: "panel-head" }, el("h2", { text: "By hour" }), el("span", { class: "muted", text: "local time" })),
      Charts.bar(hours, { fmt: fmtTok, height: 200, label: "Tokens by hour of day", summary: `Tokens by hour, ${rangeText()}. Busiest at ${busiest.label}:00 with ${fmtTok(busiest.value)}. ${quiet} hours had no usage.` }),
    ),
    el(
      "div",
      { class: "two-col" },
      el(
        "section",
        { class: "panel" },
        el("div", { class: "panel-head" }, el("h2", { text: "By weekday" })),
        Charts.bar(wdItems, { fmt: fmtTok, height: 170, label: "Tokens by weekday", summary: `Tokens by weekday, ${rangeText()}. ` + wdItems.map((w) => `${w.label} ${fmtTok(w.value)}`).join(", ") + "." }),
      ),
      el(
        "section",
        { class: "panel" },
        el("div", { class: "panel-head" }, el("h2", { text: "Last 13 weeks" }), el("span", { class: "muted", text: "tokens per day" })),
        Charts.heatmap(cells, {
          rows: ["Mon", "", "Wed", "", "Fri", "", "Sun"],
          cols: colLabels,
          maxCell: 18,
          label: "Calendar of daily tokens, last 13 weeks",
          summary: `Daily tokens over the last 13 weeks: ${activeDays} days with usage.`,
        }),
      ),
    ),
  );
}

// ---------- render ----------
let renderSeq = 0;
let shownKey = null;
async function render() {
  const seq = ++renderSeq;
  const detail = state.view === "session" && state.session;
  // Opening a different page shows its skeleton at once; a redraw of the same
  // page (refresh, filter change) keeps the old content, dimmed, until the
  // new one is ready.
  const key = detail ? `session:${state.session}` : state.view;
  const host0 = $("#body");
  if (key !== shownKey) {
    host0.replaceChildren(Skeleton.forView(detail ? "session-detail" : state.view));
    host0.scrollTop = 0;
  } else {
    host0.classList.add("loading");
  }
  host0.setAttribute("aria-busy", "true");
  $("#title").textContent = detail ? "Session" : VIEWS[state.view] || "Overview";
  $(".filters").hidden = !!detail || ["settings", "compare", "learn"].includes(state.view);
  $("#scope").hidden = !detail;
  document.querySelectorAll(".nav-item").forEach((a) => {
    const on = a.dataset.view === state.view;
    a.classList.toggle("active", on);
    if (on) a.setAttribute("aria-current", "page");
    else a.removeAttribute("aria-current");
  });
  const body = el("div");
  try {
    if (detail) await renderSession(body, state.session);
    else if (state.view === "learn") await renderLearn(body);
    else if (state.view === "insights") await renderInsights(body);
    else if (state.view === "compare") await renderCompare(body);
    else if (state.view === "settings") await renderSettings(body);
    else if (state.filesSeen === 0) body.append(noTranscripts());
    else if (state.view === "overview") await renderOverview(body);
    else await renderBreakdown(body, state.view);
  } catch (e) {
    body.replaceChildren(emptyState("Could not load usage", String(e), linkButton("Try again", render)));
  }
  if (seq !== renderSeq) return;
  const host = $("#body");
  host.classList.remove("loading");
  host.removeAttribute("aria-busy");
  const sameKey = key === shownKey;
  shownKey = key;
  const scroll = host.scrollTop;
  // Keep keyboard focus (and the caret in a search box) across re-renders.
  const active = document.activeElement;
  const fk = active && host.contains(active) ? active.closest("[data-fk]")?.dataset.fk : null;
  const caret = fk && active.selectionStart != null ? [active.selectionStart, active.selectionEnd] : null;
  host.replaceChildren(body);
  host.scrollTop = sameKey ? scroll : 0;
  if (fk) {
    const again = host.querySelector(`[data-fk="${CSS.escape(fk)}"]`);
    if (again) {
      again.focus({ preventScroll: true });
      if (caret && again.setSelectionRange) again.setSelectionRange(...caret);
    }
  }
}

// ---------- refresh ----------
async function refresh() {
  if (state.busy) return;
  state.busy = true;
  $("#refresh").disabled = true;
  try {
    const s = await invoke("refresh");
    const firstResult = state.filesSeen === null;
    const hadFiles = state.filesSeen;
    state.filesSeen = s.filesSeen;
    state.refreshedAt = new Date();
    $("#refreshed").textContent = `Updated ${state.refreshedAt.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}`;
    $("#refreshed").title = `${s.filesSeen} transcripts, ${s.filesRead} read, ${s.newRequests} new responses, ${s.ms} ms`;
    await loadModels();
    if (s.newRequests > 0) {
      sparkCache.clear();
      invalidateSession();
    }
    if (s.newRequests > 0 || firstResult || !hadFiles !== !s.filesSeen || !$("#body").childElementCount) await render();
  } catch (e) {
    $("#refreshed").textContent = "Refresh failed";
    $("#refreshed").title = String(e);
    if (!$("#body").childElementCount) await render();
  } finally {
    state.busy = false;
    state.progress = null;
    $("#refresh").disabled = false;
    $("#progress").hidden = true;
  }
}

// The index changed (file watcher in the core). Re-read models and redraw;
// debounced so a burst of events redraws once.
let updateTimer = null;
function onIndexUpdated() {
  clearTimeout(updateTimer);
  updateTimer = setTimeout(async () => {
    sparkCache.clear();
    invalidateSession();
    try {
      await loadModels();
    } catch {
      /* keep the old list */
    }
    state.refreshedAt = new Date();
    $("#refreshed").textContent = `Updated ${state.refreshedAt.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}`;
    render();
  }, 250);
}

async function loadModels() {
  const models = await invoke("models");
  if (JSON.stringify(models) === JSON.stringify(state.models)) return;
  state.models = models;
  const sel = $("#model");
  const families = [...new Set(models.map((m) => (m.match(/(opus|sonnet|haiku|fable)/) || [])[1]).filter(Boolean))];
  sel.replaceChildren(
    el("option", { value: "", text: "All models" }),
    families.map((f) => el("option", { value: f, text: `All ${f[0].toUpperCase()}${f.slice(1)}` })),
    models.map((m) => el("option", { value: m, text: m })),
  );
  sel.value = state.model;
  if (sel.value !== state.model) {
    state.model = "";
    sel.value = "";
  }
}

listen("refresh-progress", (e) => {
  const { done, total } = e.payload;
  if (total < 40) return;
  state.progress = done >= total ? null : { done, total };
  const p = $("#progress");
  p.hidden = done >= total;
  p.querySelector(".bar").style.width = `${(done / total) * 100}%`;
  p.querySelector("span").textContent = `Reading transcripts ${done} / ${total}`;
  const empty = $("#body .empty h2");
  if (empty && /Reading|No usage/.test(empty.textContent) && state.progress) $("#body").replaceChildren(indexing());
});
listen("index-updated", onIndexUpdated);

// ---------- wiring ----------
const nav = $("#nav");
nav.addEventListener("click", (e) => {
  const a = e.target.closest(".nav-item");
  if (!a || !a.dataset.view) return;
  go(a.dataset.view);
});
// Arrow keys move between nav items; Enter/Space activate (they are buttons).
nav.addEventListener("keydown", (e) => {
  if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(e.key)) return;
  const items = [...nav.querySelectorAll(".nav-item")];
  const i = items.indexOf(document.activeElement);
  const next = e.key === "Home" ? 0 : e.key === "End" ? items.length - 1 : (i + (e.key === "ArrowDown" ? 1 : -1) + items.length) % items.length;
  items[next].focus();
  e.preventDefault();
});
$("#refresh").addEventListener("click", refresh);
$("#about").addEventListener("click", () => showIntro());
for (const [id, key, parse] of [
  ["#model", "model", (v) => v],
  ["#range", "range", Number],
  ["#unit", "unit", (v) => v],
]) {
  $(id).value = String(state[key]);
  $(id).addEventListener("change", (e) => {
    state[key] = parse(e.target.value);
    savePrefs();
    render();
  });
}
$("#weekdays").checked = state.weekdays;
$("#weekdays").addEventListener("change", (e) => {
  state.weekdays = e.target.checked;
  savePrefs();
  render();
});
document.addEventListener("keydown", (e) => {
  // Escape on a session page goes back to the list.
  if (e.key === "Escape" && state.session && !e.defaultPrevented && !document.querySelector(".intro")) go("session");
});
window.addEventListener("focus", refresh);
// Fallback when the index-updated event is not available or missed.
setInterval(refresh, 60_000);

function start() {
  maybeShowIntro();
  render();
  refresh();
}
if (document.readyState === "loading") window.addEventListener("DOMContentLoaded", start);
else if (window.__devLoading) window.addEventListener("dev-ready", start, { once: true }); // dev harness loads scripts one by one
else start();
