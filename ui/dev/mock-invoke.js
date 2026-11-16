// `window.mockInvoke` and `window.mockListen` for the dev harness. They answer
// the same commands as the Tauri app (`refresh`, `overview`, `daily`,
// `breakdown`, `models`, `session`) from `window.MOCK_DATA`, which is either the
// synthetic generator (`mock.js`, has `rows`) or real data captured by
// `scripts/make-mock.sh` (`mock.local.js`, has `responses`).
//
// `?state=` in the page URL simulates edge cases:
//   empty          the index has no requests in any range
//   no-transcripts refresh finds no transcript files at all
//   indexing       refresh is slow and emits `refresh-progress`
//   error          every query fails

(function () {
  const params = new URLSearchParams(location.search);
  const STATE = params.get("state") || "normal";
  // ?slow=MS delays the slow commands, to see the loading skeletons.
  const SLOW = Number(params.get("slow") || 0);
  const D = window.MOCK_DATA;
  const listeners = {};

  window.mockListen = (name, cb) => {
    (listeners[name] ||= []).push(cb);
    return () => (listeners[name] = listeners[name].filter((f) => f !== cb));
  };
  window.mockEmit = (name, payload) => (listeners[name] || []).forEach((cb) => cb({ event: name, payload }));

  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const p2 = (x) => String(x).padStart(2, "0");
  const iso = (d) => `${d.getFullYear()}-${p2(d.getMonth() + 1)}-${p2(d.getDate())}`;
  const WD = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
  // While "indexing", queries see an empty index until refresh finishes.
  let indexing = STATE === "indexing";
  const isEmpty = () => STATE === "empty" || STATE === "no-transcripts" || indexing;

  function totals(rows) {
    const t = { requests: 0, input: 0, output: 0, thinking: 0, cacheWrite: 0, cacheWrite5m: 0, cacheWrite1h: 0, cacheRead: 0, webSearch: 0, webFetch: 0 };
    for (const r of rows) for (const k in t) t[k] += r[k] || 0;
    const ctx = t.input + t.cacheWrite + t.cacheRead;
    t.total = ctx + t.output;
    t.outputPerRequest = t.requests ? t.output / t.requests : 0;
    t.contextPerRequest = t.requests ? ctx / t.requests : 0;
    t.cacheHitRate = ctx ? t.cacheRead / ctx : 0;
    return t;
  }

  function where(f = {}) {
    return (r) =>
      (!f.from || r.day >= f.from) &&
      (!f.to || r.day <= f.to) &&
      (!f.weekdaysOnly || (r.dow !== 0 && r.dow !== 6)) &&
      (!f.model || r.model.includes(f.model)) &&
      (!f.project || r.project === f.project) &&
      (!f.session || r.session === f.session) &&
      (f.subagent == null || r.subagent === f.subagent) &&
      (f.beforeTs == null || r.ts < f.beforeTs);
  }

  function days(from, to) {
    const out = [];
    const d = new Date(from + "T00:00:00");
    const end = new Date(to + "T00:00:00");
    while (d <= end) {
      out.push(new Date(d));
      d.setDate(d.getDate() + 1);
    }
    return out;
  }

  // ---------- synthetic ----------
  const syn = {
    models() {
      const n = {};
      for (const r of D.rows) n[r.model] = (n[r.model] || 0) + r.requests;
      return Object.keys(n).sort((a, b) => n[b] - n[a]);
    },
    daily({ filter: f }) {
      const rows = D.rows.filter(where(f));
      return days(f.from, f.to)
        .filter((d) => !f.weekdaysOnly || (d.getDay() !== 0 && d.getDay() !== 6))
        .map((d) => {
          const day = iso(d);
          return { day, weekday: WD[d.getDay()], totals: totals(rows.filter((r) => r.day === day)) };
        });
    },
    overview({ filter: f }) {
      const now = new Date();
      const today = iso(now);
      const mon = new Date(now);
      mon.setDate(mon.getDate() - ((now.getDay() + 6) % 7));
      const lastMon = new Date(mon);
      lastMon.setDate(lastMon.getDate() - 7);
      const weekAgo = new Date(now);
      weekAgo.setDate(weekAgo.getDate() - 7);
      const base = { ...f, from: null, to: null };
      const sel = (from, to, before) => totals(D.rows.filter(where({ ...base, from, to, beforeTs: before })));
      const lastSun = new Date(mon);
      lastSun.setDate(lastSun.getDate() - 1);
      return {
        today,
        asOf: `${p2(now.getHours())}:${p2(now.getMinutes())}`,
        todayTotals: sel(today, today),
        sameDayLastWeek: sel(iso(weekAgo), iso(weekAgo), weekAgo.getTime()),
        thisWeek: sel(iso(mon), today),
        lastWeekToDate: sel(iso(lastMon), iso(weekAgo), weekAgo.getTime()),
        lastWeek: sel(iso(lastMon), iso(lastSun)),
        weekStart: iso(mon),
      };
    },
    breakdown({ filter: f, dim, limit = 200 }) {
      const rows = D.rows.filter(where(f));
      const all = totals(rows).total || 1;
      const keyOf = {
        model: (r) => r.model,
        project: (r) => r.project,
        session: (r) => r.session,
        effort: (r) => r.effort || "unknown",
        agent: (r) => (r.subagent ? "subagent" : "main"),
        hour: (r) => p2(r.hour),
        weekday: (r) => String(r.dow),
        entrypoint: (r) => r.entrypoint || "unknown",
      }[dim];
      const by = new Map();
      for (const r of rows) {
        const k = keyOf(r);
        if (!by.has(k)) by.set(k, []);
        by.get(k).push(r);
      }
      const ses = new Map(D.sessions.map((s) => [s.id, s]));
      const base = (p) => p.split("/").filter(Boolean).pop();
      let out = [...by.entries()].map(([key, rs]) => {
        const t = totals(rs);
        let label = key,
          detail = null;
        if (dim === "project") [label, detail] = [base(key), key];
        if (dim === "session") [label, detail] = [ses.get(key)?.title || key.slice(0, 8), base(rs[0].project)];
        if (dim === "hour") label = `${key}:00`;
        if (dim === "weekday") label = WD[Number(key)];
        return { key, label, detail, totals: t, share: t.total / all };
      });
      if (dim === "hour" || dim === "weekday") out.sort((a, b) => (a.key < b.key ? -1 : 1));
      else out.sort((a, b) => b.totals.total - a.totals.total);
      return out.slice(0, limit);
    },
    session({ id }) {
      const s = D.sessions.find((x) => x.id === id);
      if (!s) throw new Error(`session ${id} not found`);
      return richer(synthSession(s, D.rows.filter((r) => r.session === id)), s);
    },
    insights() {
      return SYN_INSIGHTS;
    },
  };

  // Turns, largest items and fixed overhead for synthetic sessions, so every
  // section of the session page has something to show. All made up.
  function richer(d, s) {
    let seed = [...s.id].reduce((a, c) => (a * 17 + c.charCodeAt(0)) % 2147483647, 5);
    const r = () => ((seed = (seed * 16807) % 2147483647) - 1) / 2147483646;
    const tools = ["Bash", "Read", "Edit", "Grep", "Write", "WebSearch", "mcp__browser__navigate", "mcp__github__get_pull_request"];
    const t0 = d.firstTs || Date.now() - 3 * 3600e3;
    const n = Math.max(6, Math.min(40, Math.round((d.prompts || 20) * 0.8)));
    d.turns = d.turns && d.turns.length ? d.turns : Array.from({ length: n }, (_, i) => {
      const req = 1 + Math.floor(r() * 30);
      const used = tools.filter(() => r() < 0.35).map((name) => ({ name, calls: 1 + Math.floor(r() * 12) }));
      return {
        index: i, ts: t0 + i * 6 * 60e3, origin: r() < 0.9 ? "human" : "task_notification", requests: req,
        contextTokens: Math.round(40e3 + r() * 300e3), contextAdded: Math.round(req * (500 + r() * 6000)),
        output: Math.round(req * (300 + r() * 1500)), tools: used.length ? used : [{ name: "Read", calls: 2 }],
        flags: r() < 0.08 ? ["compacted"] : r() < 0.08 ? ["rebuild"] : [],
      };
    });
    d.topPieces = d.topPieces && d.topPieces.length ? d.topPieces : Array.from({ length: 12 }, (_, i) => {
      const tool = ["Read", "Bash", "mcp__browser__navigate", "Grep"][i % 4];
      const tokens = Math.round(2e3 + r() * 22e3);
      const requests = Math.round(20 + r() * 300);
      return { label: tool.startsWith("mcp__") ? "browser" : tool, group: tool.startsWith("mcp__") ? "MCP" : "Tools", tool, tokens, requests, carriedTokens: tokens * requests, exact: true, ts: t0 + i * 9 * 60e3 };
    }).sort((a, b) => b.carriedTokens - a.carriedTokens);
    if (!d.overhead) {
      const servers = [
        { name: "browser", tokens: 6200, calls: 14 },
        { name: "github", tokens: 4100, calls: 3 },
        { name: "calendar", tokens: 2300, calls: 0 },
        { name: "notes", tokens: 900, calls: 0 },
      ].map((x) => ({ ...x, carriedTokens: x.tokens * (d.requests || 100), tools: [{ name: x.name, tokens: x.tokens }] }));
      const builtin = [{ name: "Bash", tokens: 4200 }, { name: "Edit", tokens: 2100 }];
      d.overhead = { total: 9500 + 28000 + 13500, systemTokens: 9500, systemBlocks: [200, 1300, 8000], builtinTokens: 28000, builtin, mcpTokens: 13500, servers, toolsLoaded: 38 };
    }
    d.mode = "exact";
    d.exactRequests = d.requests;
    return d;
  }

  const SYN_INSIGHTS = {
    sessions: 90, analyzed: 90, unit: "tokens",
    change: { explanation: "Requests +18%, context per request −6%: you made more requests; each one was about the same size.", periods: { label: "last 7 days vs the 7 before" } },
    insights: [
      { severity: "high", title: "Long sessions dominate", amount: 412e6, share: 0.46, unit: "tokens", finding: "9 of 90 sessions ran past 300 requests or a compaction and used 46% of all tokens; the largest, \u201cMigrate nightly import job\u201d, alone used 61M.", action: "Start a new session for a new task; use /clear or /compact at a natural break.", evidence: { view: "sessions", filter: {} } },
      { severity: "high", title: "Heavy tool definitions", amount: 98e6, share: 0.11, unit: "tokens", finding: "Every request carries 51k of system prompt and tool definitions (38 tools); 2 MCP servers were loaded in most sessions but never called.", action: "Disconnect MCP servers you rarely use, or enable them only in the projects that need them.", evidence: { view: "overhead", filter: {} } },
      { severity: "medium", title: "Large CLAUDE.md / instructions", subjectLabel: "billing-service", amount: 31e6, share: 0.035, unit: "tokens", finding: "In billing-service, CLAUDE.md and other instruction files averaged 18k of every request's context.", action: "Shorten or split the file; move rarely needed parts into skills.", evidence: { view: "project", filter: { project: "/Users/dev/Projects/billing-service" } } },
      { severity: "medium", title: "Idle cache rebuilds", amount: 14e6, share: 0.016, unit: "tokens", finding: "11 times a session resumed after the cache expired and wrote its whole context again (14M tokens).", action: "Finish or compact before long breaks; a resumed large session rewrites its whole context.", evidence: { view: "sessions", filter: {} } },
      { severity: "low", title: "One tool dominates", subjectLabel: "acme-web: Bash", amount: 9e6, share: 0.01, unit: "tokens", finding: "In acme-web, Bash results were 24% of the context, mostly verbose test output.", action: "Run tests in quiet mode and filter output before it reaches the model.", evidence: { view: "project", filter: { project: "/Users/dev/Projects/acme-web" } } },
    ],
    skipped: [],
  };

  function synthSession(s, rows) {
    let seed = [...s.id].reduce((a, c) => (a * 31 + c.charCodeAt(0)) % 2147483647, 11);
    const rnd = () => {
      seed = (seed * 16807) % 2147483647;
      return (seed - 1) / 2147483646;
    };
    const main = rows.filter((r) => !r.subagent);
    const requests = main.reduce((a, r) => a + r.requests, 0) || 1;
    const first = Math.min(...rows.map((r) => r.ts));
    const last = Math.max(...rows.map((r) => r.ts)) + 40 * 60e3;
    const n = Math.min(requests, 600);
    const overhead = 38e3 + rnd() * 25e3;
    const timeline = [];
    const events = [];
    let ctx = overhead + 4e3;
    const limit = 900e3;
    for (let i = 0; i < n; i++) {
      const ts = first + ((last - first) * i) / n;
      ctx += 1e3 + rnd() * (s.heavy ? 9e3 : 3e3);
      let cacheWrite = Math.round(2e3 + rnd() * 6e3);
      if (ctx > limit) {
        events.push({ ts, kind: "compaction", label: "Auto-compacted", tokens: Math.round(ctx) });
        ctx = overhead + 30e3;
        cacheWrite = Math.round(ctx);
      }
      if (i > 5 && rnd() < 0.006) {
        events.push({ ts, kind: "rebuild", label: "Cache rebuilt after 47 min idle", tokens: Math.round(ctx) });
        cacheWrite = Math.round(ctx);
      }
      if (rnd() < 0.008) events.push({ ts, kind: "subagent", label: "Explore: find usages of the parser", tokens: 0 });
      if (rnd() < 0.003) events.push({ ts, kind: "error", label: "API error 529 (overloaded), retried", tokens: 0 });
      timeline.push({ ts: Math.round(ts), context: Math.round(ctx), cacheWrite, output: Math.round(300 + rnd() * 1500) });
    }
    const avg = timeline.reduce((a, p) => a + p.context, 0) / timeline.length;
    const carried = avg * requests;
    const groupShares = {
      "Model output": 0.3 + rnd() * 0.15,
      Tools: 0.15 + rnd() * 0.15,
      MCP: rnd() * 0.12,
      Injected: 0.05 + rnd() * 0.1,
      You: 0.02 + rnd() * 0.04,
      Compaction: events.some((e) => e.kind === "compaction") ? 0.03 + rnd() * 0.04 : 0,
      Overhead: overhead / avg,
      Other: rnd() * 0.02,
    };
    const sum = Object.values(groupShares).reduce((a, b) => a + b, 0);
    const groups = Object.entries(groupShares)
      .filter(([, v]) => v > 0)
      .map(([group, v]) => ({ label: group, group, share: v / sum, tokens: (v / sum) * carried }));
    const parts = {
      "Model output": ["Earlier thinking", "Earlier text and tool calls"],
      Tools: ["Bash", "Read", "Edit", "Grep", "Write", "WebFetch"],
      MCP: ["Browser screenshots", "GitHub", "Linear"],
      Injected: ["Reminders", "Skill list", "Tool lists added by Claude Code", "Project instructions (CLAUDE.md)", "File snapshots"],
      You: ["Your prompts", "Pasted images"],
      Compaction: ["Compaction summaries"],
      Overhead: ["System prompt", "Built-in tool definitions", "MCP tool definitions"],
      Other: ["Unattributed"],
    };
    const composition = [];
    for (const g of groups) {
      const names = parts[g.group];
      let w = names.map(() => 0.2 + rnd());
      const ws = w.reduce((a, b) => a + b, 0);
      names.forEach((label, i) => composition.push({ label, group: g.group, share: (g.share * w[i]) / ws, tokens: (g.tokens * w[i]) / ws }));
    }
    composition.sort((a, b) => b.tokens - a.tokens);
    const tools = composition
      .filter((c) => c.group === "Tools" || c.group === "MCP")
      .map((c) => ({
        name: c.label,
        group: c.group,
        calls: Math.round(5 + rnd() * 120),
        resultTokens: c.tokens / (requests * 0.4),
        carriedTokens: c.tokens,
        largestResult: 2e3 + rnd() * 30e3,
      }));
    for (const extra of ["Glob", "TodoWrite", "AskUserQuestion", "NotebookEdit", "Skill", "Task", "Monitor", "ToolSearch", "WebSearch"]) {
      tools.push({ name: extra, group: "Tools", calls: Math.round(1 + rnd() * 8), resultTokens: 200 + rnd() * 900, carriedTokens: 1e4 + rnd() * 8e4, largestResult: 300 + rnd() * 1500 });
    }
    const t = totals(rows);
    const output = main.reduce((a, r) => a + r.output, 0);
    const subs = rows.filter((r) => r.subagent);
    const subagents = subs.map((r, i) => ({
      id: `agent-${p2(i)}${s.id.slice(0, 6)}`,
      agentType: i % 3 === 0 ? "general-purpose" : "Explore",
      description: ["Find usages of the parser", "Check test coverage", "Summarize the API changes", "Look for dead code"][i % 4],
      requests: r.requests,
      tokens: r.cacheRead + r.cacheWrite + r.input + r.output,
      output: r.output,
      firstTs: r.ts,
    }));
    const compactions = events.filter((e) => e.kind === "compaction").length;
    const rebuilds = events.filter((e) => e.kind === "rebuild");
    const warnings = [];
    if (compactions) warnings.push({ level: "warn", title: `Compacted ${compactions} time${compactions > 1 ? "s" : ""}`, detail: "The context filled up. Starting a new session for a new task keeps each request smaller." });
    if (rebuilds.length) warnings.push({ level: "warn", title: `${rebuilds.length} cache rebuild${rebuilds.length > 1 ? "s" : ""} after idle`, detail: "Coming back after the cache expired re-sent the whole context at the cache-write price." });
    warnings.push({ level: "info", title: "Earlier thinking is a large share", detail: "Thinking from earlier turns is kept in the conversation. A lower effort level produces less of it." });
    const usd = (t.cacheRead * 0.5 + t.cacheWrite * 6 + t.input * 5 + t.output * 25) / 1e6;
    return {
      id: s.id,
      title: s.title,
      project: s.project,
      firstTs: first,
      lastTs: last,
      totals: t,
      requests,
      prompts: Math.max(1, Math.round(requests / (3 + rnd() * 4))),
      peakContext: Math.max(...timeline.map((p) => p.context)),
      avgContext: avg,
      output,
      thinking: Math.round(output * 0.3),
      compactions,
      rebuilds: rebuilds.length,
      rebuildTokens: rebuilds.reduce((a, e) => a + e.tokens, 0),
      hookErrors: 0,
      apiErrors: events.filter((e) => e.kind === "error").length,
      overheadTokens: overhead,
      toolsLoaded: 40 + Math.round(rnd() * 40),
      hasSnapshots: !s.id.endsWith("3"),
      composition,
      groups,
      timeline,
      events,
      tools,
      subagents,
      warnings,
      cost: rnd() < 0.8 ? { usd, byModel: [[s.model, usd * 0.94], ["claude-haiku-5-5", usd * 0.06]], linesAdded: Math.round(rnd() * 900), linesRemoved: Math.round(rnd() * 400), apiMs: 1, toolMs: 1 } : null,
    };
  }

  // ---------- local (captured from the CLI) ----------
  function rangeKey(f) {
    const n = Math.round((new Date(f.to) - new Date(f.from)) / 864e5) + 1;
    const have = Object.keys(D.responses.breakdown).map(Number);
    return String(have.reduce((a, b) => (Math.abs(b - n) < Math.abs(a - n) ? b : a), have[0]));
  }
  const loc = {
    models: () => D.responses.models,
    overview: () => D.responses.overview,
    daily: ({ filter: f }) =>
      D.responses.daily.filter((d) => d.day >= f.from && d.day <= f.to && (!f.weekdaysOnly || (d.weekday !== "Sat" && d.weekday !== "Sun"))),
    breakdown: ({ filter: f, dim, limit = 200 }) => {
      if (f.project || f.session) return [];
      return (D.responses.breakdown[rangeKey(f)][dim] || []).slice(0, limit);
    },
    session: ({ id }) => {
      const s = D.responses.sessions[id];
      if (!s) throw new Error(`session ${id} is not in mock.local.js; raise --sessions in make-mock.sh`);
      return s;
    },
  };

  // `daily` with a project/session/model filter has no captured answer locally:
  // return zeros so sparklines show flat instead of wrong.
  function localDaily(args) {
    const f = args.filter;
    const rows = loc.daily(args);
    if (!f.project && !f.session && !f.model) return rows;
    const z = totals([]);
    return rows.map((d) => ({ ...d, totals: z }));
  }

  window.mockInvoke = async (cmd, args = {}) => {
    await sleep(STATE === "indexing" && cmd === "refresh" ? 0 : 15);
    if (STATE === "error" && cmd !== "refresh") throw new Error("mock: database is locked");
    if (cmd === "refresh") {
      if (STATE === "indexing") {
        const total = 600;
        for (let done = 0; done <= total; done += 40) {
          window.mockEmit("refresh-progress", { done: Math.min(done, total), total });
          await sleep(120);
        }
        indexing = false;
        return { filesSeen: total, filesRead: total, newRequests: 5000, errors: 0, ms: 2100 };
      }
      if (STATE === "no-transcripts") return { filesSeen: 0, filesRead: 0, newRequests: 0, errors: 0, ms: 3 };
      return D.refresh || { filesSeen: 1, filesRead: 0, newRequests: 0, errors: 0, ms: 1 };
    }
    if (isEmpty()) {
      if (cmd === "models") return [];
      if (cmd === "breakdown") return [];
      if (cmd === "daily") return syn.daily.call(null, { filter: { ...args.filter, model: "\u0000none" } });
      if (cmd === "overview") return syn.overview({ filter: { model: "\u0000none" } });
    }
    // Commands added after the harness: answered from captured extras, or
    // with fixed stand-ins.
    if (cmd === "session_cached") cmd = "session";
    if (SLOW && ["session", "insights", "breakdown", "overview"].includes(cmd)) await sleep(SLOW);
    const extra = (D.extra || {})[cmd];
    if (extra !== undefined) return withCost(JSON.parse(JSON.stringify(extra)));
    if (cmd in STUBS) return withCost(STUBS[cmd](args));
    const impl = D.rows ? syn : loc;
    if (D.rows && !impl[cmd]) throw new Error(`mock: unknown command ${cmd}`);
    if (!D.rows && cmd === "daily") return withCost(localDaily(args));
    if (!impl[cmd]) throw new Error(`mock: unknown command ${cmd}`);
    return withCost(JSON.parse(JSON.stringify(impl[cmd](args))));
  };

  const SETTINGS = {
    notificationsEnabled: false, notifyContext: true, contextThresholdPct: 70, contextWindowTokens: 1000000,
    notifyCacheRewrite: true, cacheRewriteMinTokens: 100000, notifyDailySpike: true, dailySpikeMultiple: 2,
    notifyCooldownMinutes: 60, launchAtLogin: false, menuBar: "usual",
  };
  const STUBS = {
    get_settings: () => ({ ...SETTINGS }),
    set_settings: ({ settings }) => Object.assign(SETTINGS, settings),
    price_info: () => ({ source: "mock prices", retrieved: "n/a", overridden: false }),
    insights: () => (D.rows ? SYN_INSIGHTS : { sessions: 0, analyzed: 0, insights: [], skipped: [], change: null }),
    summary_preview: () => '{ "schema": "claude-usage-summary/1", "mock": true }',
    export_summary: () => null,
    compare: () => null,
    change: () => null,
  };

  // Older captures and the synthetic generator have no cost fields: add a
  // stand-in so cost views render. These are NOT real prices.
  function withCost(v) {
    if (Array.isArray(v)) v.forEach(withCost);
    else if (v && typeof v === "object") {
      if ("cacheRead" in v && "total" in v && !("costUsd" in v)) {
        v.costUsd = ((v.input || 0) * 5 + (v.output || 0) * 25 + (v.cacheWrite || 0) * 6.25 + (v.cacheRead || 0) * 0.5) / 1e6;
        v.weighted = v.costUsd * 2e5;
        v.unpriced = 0;
        v.costPerRequest = v.requests ? v.costUsd / v.requests : 0;
      }
      Object.values(v).forEach(withCost);
    }
    return v;
  }
})();
