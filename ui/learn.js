// "How usage works" (PLAN 4.I.1) with the user's own numbers, the glossary,
// and the first-run screen (4.I.3). Uses only existing commands: overview,
// breakdown by session and session detail of the heaviest recent session.

const INDEX_PATH = "~/Library/Application Support/digital.prototyp.claude-usage-analytics/index.sqlite";

async function renderLearn(body) {
  // This week, or last week early on a Monday.
  const o = await invoke("overview", { filter: filter() });
  const wk = o.thisWeek.requests >= 20 ? o.thisWeek : o.lastWeek;
  const wkName = wk === o.thisWeek ? "this week" : "last week";
  const top = await invoke("breakdown", { filter: { ...filter(), from: daysAgo(27), to: today() }, dim: "session", limit: 5 }).catch(() => []);
  // An example session with a real timeline: the heaviest of the last 4 weeks.
  let ex = null;
  for (const g of top.slice(0, 3)) {
    try {
      const d = await loadSession(g.key);
      if (d.timeline.length > 5) {
        ex = d;
        break;
      }
    } catch {
      /* transcript gone: try the next one */
    }
  }
  const has = wk.requests > 0;
  const exName = ex ? `“${ex.title || ex.id.slice(0, 8)}”` : "";
  const openEx = ex ? el("button", { class: "linklike", text: "Open this session", onclick: () => go("session", ex.id) }) : null;

  body.append(
    el(
      "div",
      { class: "learn" },
      el(
        "p",
        { class: "lead" },
        "Three ideas explain almost every number in this app. ",
        has ? `The figures below are yours, from ${wkName}${ex ? ` and from your heaviest recent session, ${exName}` : ""}.` : "Once you have usage, the figures below fill in with your own numbers.",
      ),

      // ---------- 1. context re-sent ----------
      el(
        "section",
        { class: "panel learn-sec" },
        el("h2", {}, el("span", { class: "num-badge", text: "1" }), "Every request re-sends the whole conversation"),
        el(
          "p",
          {},
          "The model keeps no memory between requests. Each time Claude Code calls it (after your prompt, and again after every tool result) it sends everything so far: the system prompt, tool definitions, your messages, the model's earlier replies and thinking, and every tool result. That is the ",
          term("Context"),
          ". It grows with each step, so late requests in a long session cost far more than early ones.",
        ),
        has
          ? el(
              "p",
              { class: "yours" },
              `${cap(wkName)}, your average request carried `,
              el("b", { text: fmtTok(wk.contextPerRequest) }),
              " tokens of context and produced ",
              el("b", { text: fmtTok(wk.outputPerRequest) }),
              " tokens of output: the model read ",
              el("b", { text: `${fmtInt(wk.contextPerRequest / Math.max(1, wk.outputPerRequest))}×` }),
              ` more than it wrote. Across ${fmtInt(wk.requests)} requests that is ${fmtTok(wk.total)} tokens.`,
            )
          : null,
        ex ? growthChart(ex) : illustration("growth"),
        ex
          ? el(
              "p",
              { class: "caption" },
              `${exName}: context grew from ${fmtTok(ex.timeline[0].context)} to a peak of ${fmtTok(ex.peakContext)} over ${fmtInt(ex.requests)} requests`,
              ex.compactions ? `, and was compacted ${ex.compactions} time${ex.compactions === 1 ? "" : "s"} when it neared the limit. ` : ". ",
              openEx,
            )
          : null,
        el("p", { class: "tip-line" }, "What helps: start a new session for a new task; ", el("code", { text: "/clear" }), " or ", el("code", { text: "/compact" }), " at a natural break; avoid pasting large logs or reading whole files when a range will do."),
      ),

      // ---------- 2. cache ----------
      el(
        "section",
        { class: "panel learn-sec" },
        el("h2", {}, el("span", { class: "num-badge", text: "2" }), "Cached tokens are cheap, not free, and they expire"),
        el(
          "p",
          {},
          "Re-sending the conversation would be slow and expensive, so the API keeps it in a prompt cache. New content is ",
          term("Cache write", "written to the cache"),
          " once, at a premium over plain input. On later requests the same content is ",
          term("Cache read", "read from the cache"),
          " at a small fraction of the input price, but it is still paid on every request. A cached entry lives for 5 minutes (or 1 hour for longer-lived writes) after it was last used. Come back after a longer break and the next request ",
          term("Cache rebuild", "writes everything again"),
          ".",
        ),
        has
          ? el(
              "p",
              { class: "yours" },
              `${cap(wkName)}: `,
              el("b", { text: fmtPct(wk.cacheHitRate) }),
              " of your context came from the cache. For every token written you read it back about ",
              el("b", { text: `${fmtInt(wk.cacheRead / Math.max(1, wk.cacheWrite))} times` }),
              ` (${fmtTok(wk.cacheWrite)} written, ${fmtTok(wk.cacheRead)} read).`,
              wk.cacheWrite1h ? ` ${fmtPct(wk.cacheWrite1h / Math.max(1, wk.cacheWrite))} of writes used the 1-hour lifetime.` : "",
            )
          : null,
        ex ? cacheChart(ex) : illustration("cache"),
        ex
          ? el(
              "p",
              { class: "caption" },
              ex.rebuilds
                ? `${exName} had ${ex.rebuilds} cache rebuild${ex.rebuilds === 1 ? "" : "s"} after idle, re-writing ${fmtTok(ex.rebuildTokens)} tokens. `
                : `${exName} had no cache rebuilds after idle. `,
              "Striped segments are cache writes; the solid part is read from the cache.",
            )
          : null,
        el("p", { class: "tip-line" }, "What helps: finish or compact before a long break; a resumed large session re-writes its whole context."),
      ),

      // ---------- 3. overhead ----------
      el(
        "section",
        { class: "panel learn-sec" },
        el("h2", {}, el("span", { class: "num-badge", text: "3" }), "Fixed overhead rides on every request"),
        el(
          "p",
          {},
          "Before any of your conversation, each request carries the system prompt and the definition of every tool and MCP server that is loaded, used or not. That ",
          term("Overhead"),
          " is the same on every request, so it matters most in short sessions and adds up in long ones.",
        ),
        ex && ex.overheadTokens
          ? [
              el(
                "p",
                { class: "yours" },
                `In ${exName}, `,
                el("b", { text: fmtTok(ex.overheadTokens) }),
                ` tokens of every request were system prompt and tool definitions${ex.toolsLoaded ? ` (${fmtInt(ex.toolsLoaded)} tools loaded)` : ""}: `,
                el("b", { text: fmtPct(ex.overheadTokens / Math.max(1, ex.avgContext)) }),
                ` of the average request, about ${fmtTok(ex.overheadTokens * ex.requests)} tokens over the session.`,
              ),
              Charts.shareBar(
                [
                  { name: "Overhead", value: ex.overheadTokens, slot: groupSlot("Overhead"), tip: "System prompt and tool definitions" },
                  { name: "Conversation", value: Math.max(0, ex.avgContext - ex.overheadTokens), slot: groupSlot("Model output"), tip: "Everything else in context" },
                ],
                { fmtShare: fmtPct, label: "Average request: overhead vs conversation", summary: `Average request in ${exName}: ${fmtTok(ex.overheadTokens)} overhead, ${fmtTok(ex.avgContext - ex.overheadTokens)} conversation.` },
              ),
            ]
          : el("p", { class: "muted", text: ex ? "This session predates request snapshots, so its overhead was not measured." : "Overhead is measured per session once you have one with request snapshots." }),
        el("p", { class: "tip-line" }, "What helps: disconnect MCP servers you do not use in a project; prefer deferred tool loading."),
      ),

      // ---------- what this app can't see ----------
      el(
        "section",
        { class: "panel learn-sec" },
        el("h2", { text: "What these numbers are, and are not" }),
        el(
          "ul",
          { class: "plain" },
          el("li", {}, "Token counts per request are exact, from the API usage Claude Code records. Splits of one step between several things are estimates."),
          el("li", {}, term("Total tokens"), " is dominated by cache reads, the cheapest tokens. Context per request and output are better guides to cost."),
          el("li", {}, "Dollar figures are Claude Code's own estimate at API prices, not your bill. Subscription plan limits are not in the local logs, so the app cannot show them."),
          el("li", {}, "Only Claude Code on this Mac. claude.ai chats, other machines and the API console are not included."),
        ),
        el("button", { class: "btn", text: "What the app reads and stores", onclick: () => showIntro() }),
      ),

      // ---------- glossary ----------
      el(
        "section",
        { class: "panel learn-sec" },
        el("h2", { text: "Glossary" }),
        el(
          "dl",
          { class: "glossary" },
          Object.entries(Glossary.TERMS)
            .filter(([k]) => !/ rate$| \/ req$/.test(k) || k === "Output / req")
            .sort(([a], [b]) => a.localeCompare(b))
            .flatMap(([k, v]) => [el("dt", { text: k }), el("dd", { text: v })]),
        ),
      ),
    ),
  );
}

function cap(s) {
  return s[0].toUpperCase() + s.slice(1);
}

function growthChart(ex) {
  const oh = ex.overheadTokens || 0;
  return Charts.stackedArea(
    ex.timeline.map((p) => ({
      values: { overhead: Math.min(oh, p.context), convo: Math.max(0, p.context - oh) },
      tip: `${fmtWhen(p.ts)}\ncontext ${fmtTok(p.context)}${oh ? `\noverhead ${fmtTok(Math.min(oh, p.context))}` : ""}`,
    })),
    oh
      ? [
          { key: "overhead", name: "System prompt and tools", slot: groupSlot("Overhead") },
          { key: "convo", name: "Conversation so far", slot: groupSlot("Model output") },
        ]
      : [{ key: "convo", name: "Context", slot: groupSlot("Model output") }],
    {
      fmt: fmtTok,
      height: 190,
      label: "Context per request over one session",
      summary: `Context per request across ${ex.timeline.length} points: starts at ${fmtTok(ex.timeline[0].context)}, peaks at ${fmtTok(ex.peakContext)}.`,
    },
  );
}

// Thirty consecutive requests around the first cache rebuild (or the start).
function cacheChart(ex) {
  const pts = ex.timeline;
  const rb = ex.events.find((e) => e.kind === "rebuild");
  let at = rb ? Math.max(0, pts.findIndex((p) => p.ts >= rb.ts) - 12) : 0;
  const win = pts.slice(at, at + 30);
  return Charts.stackedBar(
    win.map((p, i) => ({
      label: String(at + i + 1),
      values: { read: Math.max(0, p.context - p.cacheWrite), write: p.cacheWrite },
      tip: `Request ${at + i + 1} · ${fmtWhen(p.ts)}\nread ${fmtTok(p.context - p.cacheWrite)}\nwritten ${fmtTok(p.cacheWrite)}`,
    })),
    [
      { key: "read", name: "Read from cache", slot: 1 },
      { key: "write", name: "Written to cache", slot: 2 },
    ],
    {
      fmt: fmtTok,
      height: 170,
      label: "Cache reads and writes per request",
      summary: `Thirty consecutive requests${rb ? " around a cache rebuild" : " from the start of the session"}. Writes total ${fmtTok(win.reduce((a, p) => a + p.cacheWrite, 0))}; reads ${fmtTok(win.reduce((a, p) => a + p.context - p.cacheWrite, 0))}.`,
    },
  );
}

// Schematic charts for an empty index, clearly marked as examples.
function illustration(kind) {
  const n = 24;
  if (kind === "growth") {
    const pts = Array.from({ length: n }, (_, i) => ({ values: { overhead: 30, convo: 6 + i * 7 }, tip: `Request ${i + 1} (example)` }));
    return el(
      "div",
      { class: "example" },
      Charts.stackedArea(pts, [
        { key: "overhead", name: "System prompt and tools", slot: groupSlot("Overhead") },
        { key: "convo", name: "Conversation so far", slot: groupSlot("Model output") },
      ], { fmt: (v) => "", height: 150, label: "Example: context grows with every request", summary: "Example only: context grows with every request while overhead stays constant." }),
      el("span", { class: "example-tag", text: "Example" }),
    );
  }
  const items = Array.from({ length: n }, (_, i) => {
    const write = i === 0 || i === 14 ? 40 + i * 2 : 3;
    return { label: String(i + 1), values: { read: i === 0 || i === 14 ? 0 : 40 + i * 2, write }, tip: i === 14 ? "After a long break: everything written again (example)" : `Request ${i + 1} (example)` };
  });
  return el(
    "div",
    { class: "example" },
    Charts.stackedBar(items, [
      { key: "read", name: "Read from cache", slot: 1 },
      { key: "write", name: "Written to cache", slot: 2 },
    ], { fmt: () => "", height: 140, label: "Example: write once, read many times, re-write after expiry", summary: "Example only: the first request writes the cache, later ones read it, and after a long break everything is written again." }),
    el("span", { class: "example-tag", text: "Example" }),
  );
}

// ---------- first run ----------
function maybeShowIntro() {
  let seen = false;
  try {
    seen = localStorage.getItem("introSeen") === "1";
  } catch {
    seen = true; // no storage: don't nag on every launch
  }
  if (!seen) showIntro(true);
}

function showIntro(first = false) {
  if (document.querySelector(".intro")) return;
  const prev = document.activeElement;
  const close = () => {
    try {
      localStorage.setItem("introSeen", "1");
    } catch {}
    overlay.remove();
    if (prev && prev.focus) prev.focus();
  };
  const ok = el("button", { class: "btn primary", text: first ? "Start" : "Close", onclick: close });
  const dialog = el(
    "div",
    { class: "intro-card", role: "dialog", "aria-modal": "true", "aria-labelledby": "intro-title" },
    el("h1", { id: "intro-title", text: first ? "Where your Claude Code tokens go" : "What this app reads and stores" }),
    first ? el("p", { class: "lead", text: "This app reads the transcripts Claude Code keeps on this Mac and shows how many tokens each day, project, session and tool used, and why." }) : null,
    el(
      "div",
      { class: "intro-grid" },
      el("div", {}, el("h3", { text: "What it reads" }), el("p", {}, "Transcripts in ", el("code", { text: "~/.claude/projects" }), " (or ", el("code", { text: "$CLAUDE_CONFIG_DIR" }), "). Read-only: nothing is changed or deleted.")),
      el(
        "div",
        {},
        el("h3", { text: "What it stores" }),
        el("p", { text: "Per request: token counts, model, effort, time, project folder and session title. Never message text, prompts, code, tool inputs or tool results. The session page re-reads one transcript to measure sizes and keeps no text." }),
      ),
      el("div", {}, el("h3", { text: "Where the index lives" }), el("p", {}, el("code", { class: "path", text: INDEX_PATH }), ". Delete it any time; it is rebuilt from the transcripts.")),
      el("div", {}, el("h3", { text: "Network" }), el("p", { text: "None. The app works offline and sends nothing anywhere." })),
    ),
    el("h3", { text: "Limits" }),
    el(
      "ul",
      { class: "plain limits" },
      el("li", {}, el("b", { text: "This Mac only. " }), "Other computers have their own transcripts."),
      el("li", {}, el("b", { text: "Claude Code only. " }), "claude.ai chats and direct API use are not in these files."),
      el("li", {}, el("b", { text: "No plan-limit data. " }), "Subscription usage percentages are not stored locally, so the app cannot show how close you are to a limit. Dollar figures are API-price estimates, not your bill."),
    ),
    el("div", { class: "intro-actions" }, el("button", { class: "btn", text: "How usage works", onclick: () => (close(), go("learn")) }), ok),
  );
  const overlay = el("div", { class: "intro", onkeydown: (e) => {
    if (e.key === "Escape") {
      e.preventDefault();
      close();
    } else if (e.key === "Tab") {
      // Keep focus inside the dialog.
      const f = [...dialog.querySelectorAll("button")];
      if (e.shiftKey && document.activeElement === f[0]) (f[f.length - 1].focus(), e.preventDefault());
      else if (!e.shiftKey && document.activeElement === f[f.length - 1]) (f[0].focus(), e.preventDefault());
    }
  } }, dialog);
  document.body.append(overlay);
  ok.focus();
}
