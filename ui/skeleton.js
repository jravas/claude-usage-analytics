// Placeholder layouts shown the moment a page opens, while its data loads.
// Each one has the shape of the page it stands in for, so nothing jumps when
// the real content arrives. Exposes `window.Skeleton.forView(view)`.

(function () {
  const h = (cls, style) => {
    const n = document.createElement("div");
    n.className = cls;
    if (style) n.setAttribute("style", style);
    return n;
  };
  const wrap = (...kids) => {
    const n = h("skeleton");
    n.setAttribute("role", "status");
    n.setAttribute("aria-label", "Loading");
    n.append(...kids.flat());
    return n;
  };
  const line = (w, cls = "") => h(`sk-line ${cls}`, `width:${w}`);
  const block = (height) => h("sk-block", `height:${height}px`);

  function cards(n = 4) {
    const row = h("cards");
    for (let i = 0; i < n; i++) {
      const c = h("card sk-card");
      c.append(line("40%", "sm"), line("62%", "lg"), line("80%", "sm"));
      row.append(c);
    }
    return row;
  }

  function panel(title, body) {
    const p = h("panel");
    p.append(line(title, "md"), ...[body].flat());
    return p;
  }

  function rows(n, cols = [38, 10, 10, 10, 12, 12]) {
    const t = h("sk-table");
    for (let i = 0; i < n; i++) {
      const r = h("sk-row");
      cols.forEach((w, j) => r.append(line(`${j === 0 ? w - (i % 3) * 6 : w}%`)));
      t.append(r);
    }
    return t;
  }

  function bars(n = 14, height = 200) {
    const b = h("sk-bars", `height:${height}px`);
    for (let i = 0; i < n; i++) b.append(h("sk-bar", `height:${25 + ((i * 37) % 70)}%`));
    return b;
  }

  const VIEWS = {
    overview: () => wrap(cards(), panel("18%", bars()), rows(8)),
    "session-detail": () =>
      wrap(
        h("detail-head", ""),
        line("12%", "sm"),
        line("38%", "xl"),
        line("52%", "sm"),
        cards(),
        panel("24%", [block(22), rows(6, [40, 14, 30, 8, 8])]),
        panel("20%", rows(6, [8, 14, 14, 8, 10, 10, 30])),
        panel("26%", block(210)),
      ),
    insights: () => {
      const list = [];
      for (let i = 0; i < 4; i++) {
        const c = h("card sk-card sk-insight");
        c.append(line("46%", "md"), line("90%"), line("72%"), line("58%", "sm"));
        list.push(c);
      }
      return wrap(panel("30%", line("80%")), list);
    },
    hour: () => wrap(line("50%"), panel("12%", bars(24, 200)), rows(8)),
    learn: () => wrap(panel("30%", [line("90%"), line("84%"), line("70%"), block(160)]), panel("26%", [line("88%"), line("64%")])),
    settings: () => wrap(panel("22%", rows(5, [60, 20])), panel("18%", rows(3, [60, 20]))),
  };

  function forView(view) {
    const make = VIEWS[view] || (() => wrap(line("46%"), rows(12)));
    return make();
  }

  window.Skeleton = { forView, rows, cards };
})();
