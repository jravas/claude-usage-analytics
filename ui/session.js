// Session detail: what filled the context, how it grew, what to look at.
// Uses the helpers in app.js. Table state (sort, search, "show all") is keyed
// by session id, so nothing carries over from one session to the next.

const GROUP_ORDER = Glossary.GROUP_ORDER;
const GROUP_HELP = Glossary.GROUPS;
const groupSlot = Glossary.groupSlot;

// Event kinds: shape + colour + name, never colour alone.
const EVENT_KINDS = {
  compaction: { name: "Compaction", shape: "triangle" },
  rebuild: { name: "Cache rebuilt", shape: "diamond" },
  subagent: { name: "Subagent started", shape: "circle" },
  error: { name: "API error", shape: "cross" },
};

function fmtDuration(ms) {
  const m = Math.round(ms / 60000);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  return h < 48 ? `${h}h ${m % 60}m` : `${Math.round(h / 24)}d`;
}

function fmtWhen(ts) {
  return new Date(ts).toLocaleString("en-GB", { day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" });
}

function fmtRange(a, b) {
  const da = new Date(a),
    db = new Date(b);
  const sameDay = isoDay(da) === isoDay(db);
  const day = (d) => d.toLocaleDateString("en-GB", { weekday: "short", day: "numeric", month: "short", year: d.getFullYear() === new Date().getFullYear() ? undefined : "numeric" });
  const time = (d) => d.toLocaleTimeString("en-GB", { hour: "2-digit", minute: "2-digit" });
  return sameDay ? `${day(da)}, ${time(da)}–${time(db)}` : `${day(da)} ${time(da)} → ${day(db)} ${time(db)}`;
}

function stat(name, title, big, sub) {
  return el("div", { class: "card" }, el("h3", {}, term(name, title)), el("div", { class: "big", text: big }), sub ? el("div", { class: "sub", text: sub }) : null);
}

// Session analysis re-reads the transcript, so keep the last result until the
// index changes instead of re-running it on every redraw.
const sessionCache = new Map();
function invalidateSession() {
  sessionCache.clear();
}
function loadSession(id) {
  if (!sessionCache.has(id)) {
    const p = invoke("session_cached", { id });
    p.catch(() => sessionCache.delete(id));
    sessionCache.set(id, p);
  }
  return sessionCache.get(id);
}

async function renderSession(body, id) {
  const d = await loadSession(id);
  const back = () => go("session");

  // ---------- header ----------
  const range = d.firstTs && d.lastTs ? fmtRange(d.firstTs, d.lastTs) : "no timestamps";
  const span = d.firstTs && d.lastTs ? fmtDuration(d.lastTs - d.firstTs) : null;
  $("#scope").textContent = `${range}${span ? ` · ${span}` : ""} · whole session, filters do not apply`;
  body.append(
    el(
      "div",
      { class: "detail-head" },
      el("button", { class: "back linklike", text: "‹ Sessions", onclick: back, title: "Back to sessions (Esc)" }),
      el("h2", { text: d.title || d.id.slice(0, 8) }),
      el(
        "div",
        { class: "meta" },
        d.project ? el("span", { title: d.project, text: d.project.split("/").filter(Boolean).pop() }) : null,
        el("span", { text: range }),
        span ? el("span", { text: `${span} from first to last request` }) : null,
        el(
          "span",
          {
            class: `badge ${d.mode === "exact" ? "exact" : "estimated"}`,
            title:
              d.mode === "exact"
                ? "Every request's context was rebuilt from the request snapshots Claude Code recorded, so the split is exact."
                : d.exactRequests
                  ? `${fmtInt(d.exactRequests)} of ${fmtInt(d.requests)} requests were rebuilt from request snapshots; the rest are estimated from transcript order and size.`
                  : "This transcript predates request snapshots, so the split is estimated from transcript order and the size of each item.",
          },
          d.mode === "exact" ? "Exact" : d.exactRequests ? `Exact for ${fmtPct(d.exactRequests / d.requests)}` : "Estimated",
        ),
      ),
    ),
  );

  // ---------- stats ----------
  const c = d.cost;
  const ratio = d.prompts ? d.requests / d.prompts : null;
  body.append(
    el(
      "div",
      { class: "cards" },
      stat(
        "Cost",
        "Cost",
        fmtUsd(d.totals.costUsd),
        c
          ? `Claude Code's own estimate ${fmtUsd(c.usd)}` + (c.byModel.length > 1 ? ` (${c.byModel.map(([m, v]) => `${m.replace("claude-", "")} ${fmtUsd(v)}`).join(", ")})` : "")
          : `${fmtTok(d.totals.total)} tokens with subagents`,
      ),
      stat("Requests", "Requests", fmtInt(d.requests), d.prompts ? `${fmtInt(d.prompts)} prompts · ${ratio.toFixed(1)} requests each` : null),
      stat("Context per request", "Context per request", fmtTok(d.avgContext), `peak ${fmtTok(d.peakContext)} · ${d.compactions} compaction${d.compactions === 1 ? "" : "s"}`),
      stat("Output", "Output", fmtTok(d.output), `${fmtTok(d.totals.total)} tokens in all${c ? ` · +${fmtInt(c.linesAdded)} / −${fmtInt(c.linesRemoved)} lines` : ""}`),
    ),
  );

  // ---------- warnings ----------
  if (d.warnings.length) {
    body.append(
      el(
        "section",
        { class: "panel warnings", "aria-label": "Findings" },
        d.warnings.map((w) =>
          el(
            "div",
            { class: `warning ${w.level}` },
            el("div", { class: "w-title" }, el("span", { class: "w-icon", "aria-hidden": "true", text: w.level === "warn" ? "!" : "i" }), el("span", { class: "sr-only", text: w.level === "warn" ? "Warning: " : "Note: " }), w.title),
            el("div", { class: "w-detail", text: w.detail }),
          ),
        ),
      ),
    );
  }

  // ---------- what filled the context ----------
  const groups = [...d.groups].sort((a, b) => GROUP_ORDER.indexOf(a.group) - GROUP_ORDER.indexOf(b.group));
  const biggest = [...groups].sort((a, b) => b.share - a.share);
  const totalComp = d.composition.reduce((a, s) => a + s.tokens, 0) || 1;
  body.append(
    el(
      "section",
      { class: "panel" },
      el("div", { class: "panel-head" }, el("h2", { text: "What filled the context" })),
      el("p", {
        class: "note top",
        text: "Each request re-sends the whole conversation. This splits those tokens by what put them there, counted for every request they stayed in context.",
      }),
      Charts.shareBar(
        groups.map((g) => ({ name: g.group, value: g.tokens, slot: groupSlot(g.group), help: GROUP_HELP[g.group], tip: `${fmtTok(g.tokens)} context tokens\n${GROUP_HELP[g.group] || ""}` })),
        {
          fmtShare: fmtPct,
          label: "What filled the context, by group",
          summary: "Share of context tokens by group: " + biggest.map((g) => `${g.group} ${fmtPct(g.share)}`).join(", ") + ".",
        },
      ),
      DataTable({
        id: `slices:${d.id}`,
        name: `session-${d.id.slice(0, 8)}-context`,
        label: "What filled the context, item by item",
        rows: d.composition,
        rowKey: (s) => s.group + "/" + s.label,
        limit: 14,
        sort: { key: "tokens", dir: "desc" },
        cls: "slices",
        columns: [
          { key: "label", label: "Item", left: true, render: (s) => el("span", { class: "cell-label" }, Charts.swatch(groupSlot(s.group)), " ", s.label) },
          { key: "group", label: "Group", left: true, width: "120px", cls: "muted" },
          {
            key: "bar",
            label: "",
            width: "28%",
            value: (s) => s.tokens,
            text: () => "",
            render: (s) => el("div", { class: "hbar", "aria-hidden": "true" }, el("div", { style: `width:${((s.tokens / (d.composition[0]?.tokens || 1)) * 100).toFixed(1)}%;background:${Charts.cssColor(groupSlot(s.group))}` })),
          },
          { key: "tokens", label: "Carried", width: "84px", text: (s) => fmtTok(s.tokens) },
          { key: "share", label: "Share", width: "72px", text: (s) => fmtPct(s.share) },
        ],
      }),
    ),
  );


  // ---------- prompts ----------
  if (d.turns && d.turns.length) {
    const TOOLS_SHOWN = 2;
    const shortTool = (n) => (n.startsWith("mcp__") ? n.split("__").slice(1).join(" › ").replace(/^[0-9a-f-]{32,}/, "connector") : n);
    const heavy = [...d.turns].sort((a, b) => b.contextAdded - a.contextAdded)[0];
    body.append(
      el(
        "section",
        { class: "panel" },
        el("div", { class: "panel-head" }, el("h2", {}, term("Prompts", `Prompts (${d.turns.length})`)), el("span", { class: "muted", text: "what each message you sent set off" })),
        el("p", {
          class: "note top",
          text: `Each prompt can run many requests. The heaviest, #${heavy.index + 1} at ${fmtWhen(heavy.ts)}, ran ${fmtInt(heavy.requests)} requests and added ${fmtTok(heavy.contextAdded)} to the context. Prompt text is not shown.`,
        }),
        DataTable({
          id: `turns:${d.id}`,
          name: `session-${d.id.slice(0, 8)}-prompts`,
          label: "Prompts in this session",
          rows: d.turns,
          rowKey: (t) => String(t.index),
          limit: 12,
          sort: { key: "added", dir: "desc" },
          columns: [
            { key: "n", label: "#", width: "44px", value: (t) => t.index + 1, text: (t) => String(t.index + 1) },
            { key: "ts", label: "Time", left: true, width: "104px", value: (t) => t.ts, text: (t) => fmtWhen(t.ts) },
            { key: "origin", label: "Started by", left: true, width: "92px", value: (t) => t.origin, text: (t) => ({ human: "You", task_notification: "Task notification", peer: "Another session", scheduled: "Schedule", sdk: "SDK", host_synthetic: "Claude Code" })[t.origin] || t.origin },
            { key: "requests", label: "Requests", width: "76px", value: (t) => t.requests, text: (t) => fmtInt(t.requests) },
            { key: "added", label: "Added", term: "Added", width: "76px", value: (t) => t.contextAdded, text: (t) => fmtTok(t.contextAdded) },
            { key: "output", label: "Output", width: "72px", value: (t) => t.output, text: (t) => fmtTok(t.output) },
            {
              key: "tools",
              label: "Tools",
              left: true,
              width: "34%",
              value: (t) => t.tools.reduce((a, x) => a + x.calls, 0),
              text: (t) => [...t.tools].sort((a, b) => b.calls - a.calls).map((x) => `${shortTool(x.name)}×${x.calls}`).join(", "),
              render: (t) => {
                const top = [...t.tools].sort((a, b) => b.calls - a.calls);
                const more = top.length - TOOLS_SHOWN;
                return el(
                  "span",
                  { class: "cell-label tools-cell", title: top.map((x) => `${shortTool(x.name)} ×${x.calls}`).join("\n") },
                  top.slice(0, TOOLS_SHOWN).map((x) => `${shortTool(x.name)} ×${x.calls}`).join(" · ") || "—",
                  more > 0 ? el("small", { text: `+${more} more` }) : null,
                );
              },
            },
            { key: "flags", label: "", left: true, width: "116px", value: (t) => t.flags.join(","), text: (t) => t.flags.join(", "), render: (t) => el("span", {}, t.flags.map((f) => el("span", { class: "chip", text: f.replace(/_/g, " ") }))) },
          ],
        }),
      ),
    );
  }

  // ---------- largest single items ----------
  if (d.topPieces && d.topPieces.length) {
    body.append(
      el(
        "section",
        { class: "panel" },
        el("div", { class: "panel-head" }, el("h2", { text: "Largest single items" }), el("span", { class: "muted", text: "ranked by context carried: size × requests it stayed for" })),
        DataTable({
          id: `pieces:${d.id}`,
          name: `session-${d.id.slice(0, 8)}-items`,
          label: "The single items that carried the most context",
          rows: d.topPieces,
          rowKey: (p) => `${p.label}/${p.ts}/${p.tokens}`,
          limit: 10,
          columns: [
            {
              key: "label",
              label: "Item",
              left: true,
              value: (p) => p.label,
              text: (p) => `${p.label}${p.tool ? ` (${p.tool})` : ""}`,
              render: (p) => el("span", { class: "cell-label" }, el("span", {}, Charts.swatch(groupSlot(p.group)), " ", p.label), el("small", { text: [p.group, p.tool && p.tool !== p.label ? p.tool : null, p.exact ? "exact" : "estimated"].filter(Boolean).join(" · ") })),
            },
            { key: "ts", label: "Added", width: "120px", value: (p) => p.ts, text: (p) => fmtWhen(p.ts) },
            { key: "tokens", label: "Size", width: "80px", value: (p) => p.tokens, text: (p) => fmtTok(p.tokens) },
            { key: "requests", label: "Requests", width: "84px", value: (p) => p.requests, text: (p) => fmtInt(p.requests) },
            { key: "carried", label: "Carried", term: "Carried", width: "84px", value: (p) => p.carriedTokens, text: (p) => fmtTok(p.carriedTokens) },
          ],
        }),
      ),
    );
  }

  // ---------- context over time ----------
  if (d.timeline.length > 1) {
    const pts = d.timeline;
    const evs = d.events
      .map((ev) => {
        let i = pts.findIndex((p) => p.ts >= ev.ts);
        if (i < 0) i = pts.length - 1;
        const k = EVENT_KINDS[ev.kind] || { name: ev.kind, shape: "circle" };
        return { index: i, shape: k.shape, cls: `ev-${ev.kind}`, tip: `${k.name} · ${fmtWhen(ev.ts)}\n${ev.label}${ev.tokens ? `\n${fmtTok(ev.tokens)} tokens` : ""}` };
      })
      .sort((a, b) => a.index - b.index);
    const kinds = [...new Set(d.events.map((e) => e.kind))];
    const peakAt = pts.reduce((a, p, i) => (p.context > pts[a].context ? i : a), 0);
    const counts = kinds.map((k) => `${d.events.filter((e) => e.kind === k).length} ${(EVENT_KINDS[k]?.name || k).toLowerCase()}`).join(", ");
    body.append(
      el(
        "section",
        { class: "panel" },
        el("div", { class: "panel-head" }, el("h2", { text: "Context over the session" }), el("span", { class: "muted", text: `${pts.length < d.requests ? "sampled" : "every"} request, oldest left` })),
        Charts.stackedArea(
          pts.map((p) => ({
            values: { read: Math.max(0, p.context - p.cacheWrite), write: p.cacheWrite },
            tip: `${fmtWhen(p.ts)}\ncontext ${fmtTok(p.context)}\ncache write ${fmtTok(p.cacheWrite)} · output ${fmtTok(p.output)}`,
          })),
          [
            { key: "read", name: "Read from cache", slot: 1 },
            { key: "write", name: "Written to cache", slot: 2 },
          ],
          {
            fmt: fmtTok,
            height: 210,
            events: evs,
            eventLegend: kinds.map((k) => ({ name: EVENT_KINDS[k]?.name || k, shape: EVENT_KINDS[k]?.shape || "circle", cls: `ev-${k}` })),
            label: "Context size per request over the session",
            summary: `Context per request from ${fmtTok(pts[0].context)} at the start to a peak of ${fmtTok(pts[peakAt].context)} (request ${peakAt + 1} of ${pts.length}), ending at ${fmtTok(pts[pts.length - 1].context)}.${counts ? " Events: " + counts + "." : ""} Use arrow keys to step through events.`,
          },
        ),
      ),
    );
  }

  // ---------- tools ----------
  const tools = d.tools.filter((t) => t.calls || t.carriedTokens);
  if (tools.length) {
    body.append(
      el(
        "section",
        { class: "panel" },
        el("div", { class: "panel-head" }, el("h2", { text: "Tools and MCP servers" })),
        DataTable({
          id: `tools:${d.id}`,
          name: `session-${d.id.slice(0, 8)}-tools`,
          label: "Tools and MCP servers in this session",
          rows: tools,
          rowKey: (t) => t.name,
          limit: 12,
          columns: [
            {
              key: "name",
              label: "Tool or server",
              left: true,
              text: (t) => `${t.name} (${t.group === "MCP" ? "MCP server" : "built-in tool"})`,
              render: (t) => el("span", { class: "cell-label" }, el("span", {}, Charts.swatch(groupSlot(t.group)), " ", t.name), el("small", { text: t.group === "MCP" ? "MCP server" : "Built-in tool" })),
            },
            { key: "calls", label: "Calls", text: (t) => fmtInt(t.calls) },
            { key: "resultTokens", label: "Added", text: (t) => fmtTok(t.resultTokens) },
            { key: "largestResult", label: "Largest", text: (t) => fmtTok(t.largestResult) },
            { key: "carriedTokens", label: "Carried", text: (t) => fmtTok(t.carriedTokens) },
            { key: "share", label: "Share", value: (t) => t.carriedTokens / totalComp, text: (t) => fmtPct(t.carriedTokens / totalComp) },
          ],
        }),
      ),
    );
  }


  // ---------- fixed overhead ----------
  const o = d.overhead;
  if (o && o.total > 0) {
    const rows = [
      { name: "System prompt", kind: "System", tokens: o.systemTokens, calls: null, carried: null, detail: `${o.systemBlocks.length} blocks` },
      { name: "Built-in tools", kind: "Built-in", tokens: o.builtinTokens, calls: null, carried: null, detail: `${o.builtin.length} definitions, largest ${o.builtin[0] ? `${o.builtin[0].name} ${fmtTok(o.builtin[0].tokens)}` : "—"}` },
      ...o.servers.map((sv) => ({ name: sv.name, kind: "MCP server", tokens: sv.tokens, calls: sv.calls, carried: sv.carriedTokens, detail: `${sv.tools.length} tool${sv.tools.length === 1 ? "" : "s"}` })),
    ];
    const unused = o.servers.filter((sv) => !sv.calls);
    const unusedTok = unused.reduce((a, sv) => a + sv.tokens, 0);
    body.append(
      el(
        "section",
        { class: "panel" },
        el("div", { class: "panel-head" }, el("h2", { text: "Sent with every request" }), el("span", { class: "muted", text: `${fmtTok(o.total)} per request · ${fmtInt(o.toolsLoaded)} tools loaded` })),
        el("p", {
          class: "note top",
          text:
            "The system prompt and every tool definition ride along with each request, whether or not the tool is used. " +
            (unused.length ? `${unused.length} MCP server${unused.length === 1 ? " was" : "s were"} loaded but never called here: ${fmtTok(unusedTok)} on every request.` : "Every loaded MCP server was called at least once."),
        }),
        DataTable({
          id: `overhead:${d.id}`,
          name: `session-${d.id.slice(0, 8)}-overhead`,
          label: "System prompt and tool definitions per request",
          rows,
          rowKey: (r) => r.kind + r.name,
          limit: 10,
          sort: { key: "tokens", dir: "desc" },
          rowClass: (r) => (r.kind === "MCP server" && !r.calls ? "unused" : ""),
          columns: [
            { key: "name", label: "Part", left: true, value: (r) => r.name, text: (r) => `${r.name} (${r.detail})`, render: (r) => el("span", { class: "cell-label" }, r.name, el("small", { text: `${r.kind} · ${r.detail}` })) },
            { key: "tokens", label: "Per request", width: "96px", value: (r) => r.tokens, text: (r) => fmtTok(r.tokens) },
            { key: "calls", label: "Calls", width: "72px", value: (r) => r.calls, text: (r) => (r.calls == null ? "—" : r.calls ? fmtInt(r.calls) : "never") },
            { key: "carried", label: "Carried", width: "84px", value: (r) => r.carried, text: (r) => (r.carried == null ? "—" : fmtTok(r.carried)) },
          ],
        }),
      ),
    );
  }

  // ---------- subagents ----------
  if (d.subagents.length) {
    body.append(
      el(
        "section",
        { class: "panel" },
        el("div", { class: "panel-head" }, el("h2", {}, term("Subagent", `Subagents (${d.subagents.length})`))),
        DataTable({
          id: `subagents:${d.id}`,
          name: `session-${d.id.slice(0, 8)}-subagents`,
          label: "Subagents in this session",
          rows: d.subagents,
          rowKey: (a) => a.id,
          limit: 12,
          columns: [
            {
              key: "task",
              label: "Task",
              left: true,
              value: (a) => a.description || a.id,
              text: (a) => `${a.description || a.id} (${a.agentType || "subagent"})`,
              render: (a) => el("span", { class: "cell-label" }, a.description || a.id, el("small", { text: a.agentType || "subagent" })),
            },
            { key: "requests", label: "Requests", text: (a) => fmtInt(a.requests) },
            { key: "output", label: "Output", text: (a) => fmtTok(a.output) },
            { key: "tokens", label: "Tokens", term: "Total tokens", text: (a) => fmtTok(a.tokens) },
            { key: "firstTs", label: "Started", value: (a) => a.firstTs, text: (a) => (a.firstTs ? fmtWhen(a.firstTs) : "—") },
          ],
        }),
      ),
    );
  }

  body.append(
    el("p", {
      class: "note",
      text:
        "Context sizes come from the API usage and are exact. The split between things added at the same step is estimated from their size, so single rows are approximate. " +
        (d.hasSnapshots ? "" : "This session predates request snapshots, so its starting context is not split further. ") +
        "Message text is read only to measure it and is never stored.",
    }),
  );
}
