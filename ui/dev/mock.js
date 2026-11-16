// Synthetic mock data for the UI dev harness. Everything here is made up:
// project names, session titles and numbers come from a seeded generator, so
// this file is safe to commit. `scripts/make-mock.sh` writes the real-data
// equivalent to `mock.local.js`, which is git-ignored.
//
// Shape: `rows` are request aggregates per session, day, hour and agent, which
// `mock-invoke.js` filters and sums like the SQL queries do. Session details are
// generated on demand from the session id.

(function () {
  let seed = 7;
  const rnd = () => {
    seed = (seed * 16807) % 2147483647;
    return (seed - 1) / 2147483646;
  };
  const pick = (a) => a[Math.floor(rnd() * a.length)];
  const p2 = (x) => String(x).padStart(2, "0");
  const iso = (d) => `${d.getFullYear()}-${p2(d.getMonth() + 1)}-${p2(d.getDate())}`;

  const projects = [
    "/Users/dev/Projects/acme-web",
    "/Users/dev/Projects/billing-service",
    "/Users/dev/Projects/mobile-app",
    "/Users/dev/Projects/data-pipeline",
    "/Users/dev/Projects/design-system",
  ];
  const models = ["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5"];
  const efforts = ["medium", "medium", "medium", "high", "xhigh", null];
  const verbs = ["Fix", "Add", "Refactor", "Investigate", "Speed up", "Document", "Test", "Migrate", "Review", "Clean up"];
  const things = [
    "login redirect loop",
    "invoice PDF export",
    "flaky checkout test",
    "dark mode tokens",
    "nightly import job",
    "search ranking",
    "push notification badge",
    "rate limiter",
    "onboarding screens",
    "CSV upload parser",
    "cache invalidation",
    "chart tooltips",
  ];

  const now = new Date();
  const rows = [];
  const sessions = [];
  const DAYS = 120;
  for (let i = 0; i < 90; i++) {
    const start = new Date(now);
    start.setHours(0, 0, 0, 0);
    // Newer sessions are more common, and one is always running today.
    const back = i === 0 ? 0 : Math.floor(Math.pow(rnd(), 1.6) * DAYS);
    start.setDate(start.getDate() - back);
    const id = Array.from({ length: 4 }, () => Math.floor(rnd() * 0xffff).toString(16).padStart(4, "0")).join("") + "-mock-" + p2(i);
    const s = {
      id,
      title: `${pick(verbs)} ${pick(things)}`,
      project: pick(projects),
      model: rnd() < 0.7 ? models[0] : rnd() < 0.8 ? models[1] : models[2],
      effort: pick(efforts),
      entrypoint: rnd() < 0.8 ? "claude-desktop" : "cli",
      heavy: rnd() < 0.15,
    };
    sessions.push(s);
    const span = s.heavy ? 1 + Math.floor(rnd() * 3) : 1;
    for (let d = 0; d < span; d++) {
      const day = new Date(start);
      day.setDate(day.getDate() + d);
      if (day > now) break;
      const dow = day.getDay();
      if ((dow === 0 || dow === 6) && rnd() < 0.7) continue;
      const startHour = 8 + Math.floor(rnd() * 10);
      const hours = 1 + Math.floor(rnd() * (s.heavy ? 6 : 3));
      for (let h = startHour; h < Math.min(24, startHour + hours); h++) {
        if (d === span - 1 && iso(day) === iso(now) && h > now.getHours()) break;
        for (const sub of [false, true]) {
          if (sub && rnd() < 0.75) continue;
          const req = Math.round((sub ? 6 : 20) * (0.4 + rnd()) * (s.heavy ? 3 : 1));
          const ctx = (sub ? 40e3 : 90e3) * (0.5 + rnd()) * (s.heavy ? 3 : 1);
          const cacheWrite = Math.round(req * ctx * (0.03 + rnd() * 0.05));
          const input = Math.round(req * (3 + rnd() * 30));
          const cacheRead = Math.round(req * ctx - cacheWrite - input);
          const output = Math.round(req * (350 + rnd() * 900));
          const cw1h = rnd() < 0.3 ? Math.round(cacheWrite * 0.5) : 0;
          rows.push({
            session: s.id,
            project: s.project,
            model: sub && rnd() < 0.4 ? models[2] : s.model,
            effort: s.effort,
            entrypoint: s.entrypoint,
            subagent: sub,
            day: iso(day),
            dow,
            hour: h,
            ts: new Date(day.getFullYear(), day.getMonth(), day.getDate(), h, Math.floor(rnd() * 60)).getTime(),
            requests: req,
            input,
            output,
            thinking: Math.round(output * (0.2 + rnd() * 0.3)),
            cacheWrite,
            cacheWrite5m: cacheWrite - cw1h,
            cacheWrite1h: cw1h,
            cacheRead,
            webSearch: rnd() < 0.05 ? 1 : 0,
            webFetch: rnd() < 0.08 ? 2 : 0,
          });
        }
      }
    }
  }

  window.MOCK_DATA = {
    kind: "synthetic",
    generated: now.toISOString(),
    rows,
    sessions,
    refresh: { filesSeen: 412, filesRead: 3, newRequests: 12, errors: 0, ms: 41 },
  };
})();
