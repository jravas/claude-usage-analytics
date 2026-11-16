// One glossary for every metric name in the UI. `Glossary.term("Cache read")`
// returns the label with a dotted underline that explains it on hover and on
// keyboard focus. Group definitions (what filled the context) live here too.
//
// Plain language, no internal names. When the core grows a shared glossary
// (PLAN 4.I.2), load it from there and keep this as the fallback.

(function () {
  const TERMS = {
    Cost:
      "What these requests would cost at Anthropic's published API prices (dated price table in Settings). A subscription bills differently; this is a way to weigh tokens by what they cost, not your bill.",
    "Weighted tokens":
      "Tokens weighted by price, in units of one input token. Output and cache writes count more, cache reads count much less. Useful on a subscription where dollars don't apply.",
    Tokens: "Raw token counts: input + output + cache writes + cache reads. Dominated by cheap cache reads.",
    "Share of API-equivalent cost": "This row's part of the cost of all requests in the selected range.",
    "Share of price-weighted tokens": "This row's part of the price-weighted tokens in the selected range.",
    "Share of tokens": "This row's part of all tokens in the selected range.",
    "Claude Code estimate": "The running cost estimate Claude Code itself writes into the transcript, including background models (titles, web search) that have no rows here.",
        Requests: "API calls Claude Code made to the model. One prompt from you usually triggers several: one per tool call round.",
    Prompts: "Messages you sent. Each starts a turn that can run many requests.",
    "Total tokens":
      "Input + output + cache writes + cache reads, summed over every request. Dominated by cache reads, which are the cheapest tokens, so it overstates cost.",
    Input: "Tokens sent to the model that were neither written to nor read from the cache. Usually tiny.",
    Output: "Tokens the model wrote: text, tool calls and thinking. The most expensive kind per token.",
    Thinking: "Output the model spent reasoning before answering. Higher effort levels produce more of it.",
    "Cache write":
      "Context stored in the prompt cache so later requests can reuse it. Charged at a premium over plain input. Happens for new content and again whenever the cache has expired.",
    "Cache read":
      "Context served from the prompt cache. Much cheaper than input, but paid again on every request for everything still in context.",
    "Cache hit":
      "Share of the context that came from the cache. High is good: the rest was written fresh at the higher price.",
    "Cache hit rate":
      "Share of the context that came from the cache. High is good: the rest was written fresh at the higher price.",
    Context:
      "What the model read for one request: the whole conversation so far, plus the system prompt and tool definitions. Re-sent on every request.",
    "Context / req":
      "Average context per request: input + cache writes + cache reads, divided by requests. The best single measure of how heavy each request is.",
    "Context per request":
      "Average context per request: input + cache writes + cache reads, divided by requests. The best single measure of how heavy each request is.",
    "Output / req": "Average output tokens per request.",
    "Peak context": "The largest context of any single request in the session. Near the model's limit, Claude Code compacts.",
    Share: "This row's part of all tokens in the selected range.",
    Calls: "How many times the model called this tool.",
    Added: "Estimated tokens this tool's results added to the context when they first arrived.",
    Largest: "Its largest single result, estimated.",
    Carried:
      "Context tokens this item accounted for over the whole session: its size times the number of requests it stayed in context. This is what a big early result really costs.",
    Compaction:
      "When the context nears the limit, Claude Code replaces the conversation with a summary. The next request is small again, but the summary is written to the cache fresh.",
    "Cache rebuild":
      "The cache expires after a few minutes idle (5 minutes, or 1 hour for longer-lived writes). The next request re-writes the whole context at the cache-write price.",
    Overhead:
      "The system prompt and the definitions of every tool and MCP server loaded. Sent with every request, whether or not the tools are used.",
    Subagent: "A separate model conversation started by the main one (for example Explore). Its tokens are counted with the session.",
    "Main vs subagents": "Requests from the main conversation against requests from subagents it started.",
    Effort: "The reasoning effort setting for the request. Higher effort means more thinking output.",
    Model: "The model that answered. A request can be routed to a different model than the one you picked.",
    Project: "The folder the session ran in. Worktrees count toward their repository.",
    Session: "One Claude Code conversation, from its first prompt to its last request.",
    "Claude Code estimate":
      "Claude Code's own running cost estimate at API prices, recorded in the transcript. Not your bill; subscriptions are priced differently.",
    "Time of day": "Local hour the request was made.",
  };

  const GROUPS = {
    "Model output": "The model's earlier thinking, text and tool calls, re-sent with every request",
    Tools: "Results of built-in tools such as Bash and Read",
    MCP: "Results from MCP servers and connectors",
    Injected: "Added by Claude Code: reminders, tool and skill lists, instructions, file snapshots",
    You: "Your prompts and pasted images",
    Compaction: "Summaries written when the context was compacted",
    Overhead: "System prompt and tool definitions, sent with every request",
    Other: "Growth that could not be matched to anything in the transcript",
  };
  // Fixed order and colour slot per group, so a group keeps its colour and
  // pattern everywhere (charts.js slots 1–8; slot 8 is the neutral one).
  const GROUP_ORDER = ["Model output", "Tools", "MCP", "Injected", "You", "Compaction", "Overhead", "Other"];
  const groupSlot = (g) => {
    const i = GROUP_ORDER.indexOf(g);
    return i < 0 ? 8 : i + 1;
  };

  function define(name) {
    return TERMS[name] || TERMS[name.replace(/s$/, "")] || null;
  }

  // A label that explains itself. Falls back to plain text for unknown names.
  function term(name, label = name) {
    const def = define(name);
    const span = document.createElement("span");
    span.textContent = label;
    if (!def) return span;
    span.className = "term";
    span.tabIndex = 0;
    span.setAttribute("aria-description", def);
    const show = () => window.Charts.showTipAt(span, `${name}\n${def}`);
    span.addEventListener("mouseenter", show);
    span.addEventListener("focus", show);
    span.addEventListener("mouseleave", window.Charts.hideTip);
    span.addEventListener("blur", window.Charts.hideTip);
    // A click inside a sortable header must not also sort.
    span.addEventListener("click", (e) => e.stopPropagation());
    return span;
  }

  window.Glossary = { TERMS, GROUPS, GROUP_ORDER, groupSlot, define, term };
})();
