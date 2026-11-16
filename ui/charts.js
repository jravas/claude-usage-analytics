// Charts: hand-written SVG, no dependencies. Exposes `window.Charts`.
//
//   Charts.bar(items, opts)              one value per column
//   Charts.stackedBar(items, series, o)  columns split into series
//   Charts.shareBar(parts, opts)         one 100% horizontal bar with direct labels
//   Charts.stackedArea(points, series, o) areas over a sequence, with event markers
//   Charts.heatmap(cells, opts)          grid of cells on a one-hue scale
//   Charts.waterfall(steps, opts)        start, signed changes, end
//   Charts.sparkline(values, opts)       tiny trend line for a table row or card
//   Charts.legend(series), Charts.swatch(slot)
//
// Shared pieces: theme (series slots → CSS variables + patterns so identity
// never relies on hue alone), y axis with nice ticks, one tooltip that works
// for mouse and keyboard, and a text summary on every chart for screen readers.
// Every chart is a <figure> that redraws on resize; arrow keys move between
// marks when it has focus.

(function () {
  const NS = "http://www.w3.org/2000/svg";

  function svg(tag, attrs = {}, ...children) {
    const n = document.createElementNS(NS, tag);
    for (const [k, v] of Object.entries(attrs)) {
      if (k === "text") n.textContent = v;
      else if (k.startsWith("on")) n.addEventListener(k.slice(2), v);
      else if (v !== null && v !== undefined && v !== false) n.setAttribute(k, v);
    }
    for (const c of children.flat()) if (c) n.append(c);
    return n;
  }

  function h(tag, attrs = {}, ...children) {
    const n = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs)) {
      if (k === "class") n.className = v;
      else if (k === "text") n.textContent = v;
      else if (k.startsWith("on")) n.addEventListener(k.slice(2), v);
      else if (v !== null && v !== undefined && v !== false) n.setAttribute(k, v);
    }
    for (const c of children.flat()) if (c !== null && c !== undefined && c !== false) n.append(c.nodeType ? c : document.createTextNode(String(c)));
    return n;
  }

  // ---------- theme ----------
  // Eight categorical slots (CSS --c1…--c8, light and dark steps in styles.css),
  // each with its own fill pattern. Slot 1 is solid.
  const PATTERNS = [
    null,
    "M0,8 L8,0", // 45°
    "dots",
    "M0,0 L8,8", // 135°
    "M0,4 L8,4", // horizontal
    "M0,8 L8,0 M0,0 L8,8", // cross
    "M4,0 L4,8", // vertical
    "dots-sparse",
  ];

  function ensureDefs() {
    if (document.getElementById("chart-defs")) return;
    const defs = svg("defs");
    PATTERNS.forEach((p, i) => {
      const slot = i + 1;
      const pat = svg("pattern", { id: `pat-${slot}`, width: 8, height: 8, patternUnits: "userSpaceOnUse" }, svg("rect", { width: 8, height: 8, style: `fill:var(--c${slot})` }));
      if (p === "dots") pat.append(svg("circle", { cx: 4, cy: 4, r: 1.4, class: "pat-ink" }));
      else if (p === "dots-sparse") pat.append(svg("circle", { cx: 2, cy: 2, r: 1.1, class: "pat-ink" }), svg("circle", { cx: 6, cy: 6, r: 1.1, class: "pat-ink" }));
      else if (p) pat.append(svg("path", { d: p, class: "pat-line" }));
      defs.append(pat);
    });
    const host = svg("svg", { id: "chart-defs", width: 0, height: 0, "aria-hidden": "true", style: "position:absolute;width:0;height:0" }, defs);
    document.body.append(host);
  }

  // SVG fill (pattern) and plain CSS colour for HTML elements.
  const fillFor = (slot) => (slot ? `url(#pat-${slot})` : "var(--accent)");
  const cssColor = (slot) => (slot ? `var(--c${slot})` : "var(--accent)");

  // Small inline swatch for legends and tables, same pattern as the chart.
  function swatch(slot, shape = "rect", cls = null) {
    ensureDefs();
    const s = svg("svg", { class: "swatch", width: 10, height: 10, viewBox: "0 0 10 10", "aria-hidden": "true" });
    if (shape === "rect") s.append(svg("rect", { width: 10, height: 10, rx: 2, fill: fillFor(slot) }));
    else s.append(marker(shape, 5, 5, cls || slot, 4));
    return s;
  }

  function legend(series) {
    return h(
      "div",
      { class: "legend" },
      series.map((s) => h("span", { class: "legend-item", title: s.help || "" }, swatch(s.slot, s.shape, s.cls), s.name)),
    );
  }

  // Event markers differ by shape as well as colour.
  function marker(shape, x, y, cls, r = 4.5) {
    const c = typeof cls === "number" ? `mk mk-s${cls}` : `mk ${cls || ""}`;
    if (shape === "triangle") return svg("path", { class: c, d: `M${x - r},${y - r} L${x + r},${y - r} L${x},${y + r * 0.9} Z` });
    if (shape === "diamond") return svg("path", { class: c, d: `M${x},${y - r} L${x + r},${y} L${x},${y + r} L${x - r},${y} Z` });
    if (shape === "cross") return svg("path", { class: c + " mk-cross", d: `M${x - r},${y - r} L${x + r},${y + r} M${x + r},${y - r} L${x - r},${y + r}` });
    return svg("circle", { class: c, cx: x, cy: y, r: r * 0.85 });
  }

  // ---------- tooltip ----------
  const tip = h("div", { class: "tip", role: "tooltip", id: "chart-tip", hidden: true });
  function mountTip() {
    if (!tip.isConnected) document.body.append(tip);
  }
  function showTip(e, text) {
    mountTip();
    tip.textContent = text;
    tip.hidden = false;
    const x = Math.min(e.clientX + 12, window.innerWidth - tip.offsetWidth - 8);
    const y = e.clientY + 14 + tip.offsetHeight > window.innerHeight ? e.clientY - tip.offsetHeight - 10 : e.clientY + 14;
    tip.style.left = Math.max(4, x) + "px";
    tip.style.top = y + "px";
  }
  function showTipAt(node, text) {
    const r = node.getBoundingClientRect();
    showTip({ clientX: r.left + r.width / 2, clientY: r.top + Math.min(r.height, 24) }, text);
  }
  function hideTip() {
    tip.hidden = true;
  }

  // ---------- axes ----------
  function niceTicks(max, count = 4) {
    if (!(max > 0)) return [0, 1];
    const raw = max / count;
    const mag = Math.pow(10, Math.floor(Math.log10(raw)));
    const n = raw / mag;
    const step = (n <= 1 ? 1 : n <= 2 ? 2 : n <= 2.5 ? 2.5 : n <= 5 ? 5 : 10) * mag;
    const out = [];
    for (let v = 0; v <= max + step * 0.999; v += step) out.push(v);
    if (out.length < 2) out.push(step);
    return out;
  }

  function yAxis(s, ticks, y, fmt, x0, x1) {
    for (const t of ticks) {
      s.append(
        svg("line", { class: "grid", x1: x0, x2: x1, y1: y(t), y2: y(t) }),
        svg("text", { class: "axis", x: x0 - 8, y: y(t) + 3, "text-anchor": "end", text: fmt(t) }),
      );
    }
  }

  function xLabels(s, labels, xCenter, y, minGap = 44, plotW = 600) {
    const every = Math.ceil(labels.length / Math.max(1, Math.floor(plotW / minGap)));
    labels.forEach((l, i) => {
      if (i % every === 0) s.append(svg("text", { class: "axis", x: xCenter(i), y, "text-anchor": "middle", text: l }));
    });
  }

  // ---------- frame ----------
  // A <figure> that draws on resize, carries a text summary, and supports
  // keyboard focus over its marks. draw(width) returns { svg, marks } where
  // marks are [{ node, tip }] in reading order.
  function frame(opts, draw) {
    ensureDefs();
    const fig = h("figure", { class: `chart-fig ${opts.class || ""}` });
    const sumId = "sum-" + Math.random().toString(36).slice(2, 9);
    const sum = h("p", { class: "sr-only", id: sumId, text: opts.summary || "" });
    const host = h("div", { class: "chart-host" });
    fig.append(host, sum);
    let marks = [];
    let idx = -1;
    let lastW = 0;
    const render = () => {
      const W = Math.max(host.clientWidth || opts.width || 640, 200);
      if (W === lastW && host.firstChild) return;
      lastW = W;
      const out = draw(W);
      out.svg.setAttribute("aria-hidden", "true");
      host.replaceChildren(out.svg);
      marks = out.marks || [];
      if (idx >= marks.length) idx = marks.length - 1;
    };
    if (opts.label || opts.summary) {
      fig.setAttribute("role", "group");
      fig.setAttribute("aria-label", opts.label || "Chart");
      fig.setAttribute("aria-describedby", sumId);
    }
    if (opts.keyboard !== false) {
      fig.tabIndex = 0;
      const focusMark = (i) => {
        if (!marks.length) return;
        marks.forEach((m) => m.node.classList.remove("kbd"));
        idx = Math.max(0, Math.min(marks.length - 1, i));
        const m = marks[idx];
        m.node.classList.add("kbd");
        showTipAt(m.node, m.tip);
        let live = fig.querySelector(".chart-live");
        if (!live) fig.append((live = h("span", { class: "sr-only chart-live", "aria-live": "polite" })));
        live.textContent = m.tip.replace(/\n/g, ", ");
      };
      fig.addEventListener("keydown", (e) => {
        const k = e.key;
        if (k === "ArrowRight" || k === "ArrowDown") focusMark(idx + 1);
        else if (k === "ArrowLeft" || k === "ArrowUp") focusMark(idx < 0 ? marks.length - 1 : idx - 1);
        else if (k === "Home") focusMark(0);
        else if (k === "End") focusMark(marks.length - 1);
        else if (k === "Enter" && idx >= 0 && marks[idx].onSelect) marks[idx].onSelect();
        else if (k === "Escape") hideTip();
        else return;
        e.preventDefault();
      });
      fig.addEventListener("blur", () => {
        marks.forEach((m) => m.node.classList.remove("kbd"));
        hideTip();
      });
    }
    const ro = new ResizeObserver(() => {
      if (!fig.isConnected && lastW) return ro.disconnect();
      render();
    });
    ro.observe(host);
    requestAnimationFrame(render);
    return fig;
  }

  function hover(node, text) {
    node.addEventListener("mousemove", (e) => showTip(e, text));
    node.addEventListener("mouseleave", hideTip);
    return node;
  }

  // ---------- bar ----------
  // items: [{ label, value, cls, tip, onSelect }]
  function bar(items, opts = {}) {
    const fmt = opts.fmt || String;
    const H = opts.height || 220;
    return frame(opts, (W) => {
      const padL = 52,
        padB = 26,
        padT = 8;
      const max = Math.max(1e-9, ...items.map((i) => i.value));
      const ticks = niceTicks(max);
      const top = ticks[ticks.length - 1];
      const plotW = W - padL - 4,
        plotH = H - padB - padT;
      const step = plotW / Math.max(items.length, 1);
      const bw = Math.max(2, Math.min(42, step - 2, step * 0.78));
      const y = (v) => padT + plotH - (v / top) * plotH;
      const s = svg("svg", { class: "chart", width: W, height: H, viewBox: `0 0 ${W} ${H}` });
      yAxis(s, ticks, y, fmt, padL, W);
      const marks = [];
      items.forEach((it, i) => {
        const x = padL + i * step + (step - bw) / 2;
        const hgt = Math.max(plotH - (y(it.value) - padT), it.value > 0 ? 1 : 0);
        const text = it.tip || `${it.label}\n${fmt(it.value)}`;
        const g = hover(
          svg(
            "g",
            { class: `col ${it.onSelect ? "clickable" : ""}`, onclick: it.onSelect || null },
            svg("rect", { class: "hit", x: padL + i * step, y: padT, width: step, height: plotH }),
            svg("path", { class: `bar ${it.cls || ""}`, d: roundTop(x, padT + plotH, bw, hgt, Math.min(4, bw / 3)) }),
          ),
          text,
        );
        s.append(g);
        marks.push({ node: g, tip: text, onSelect: it.onSelect });
      });
      xLabels(s, items.map((i) => i.label), (i) => padL + i * step + step / 2, H - 8, 44, plotW);
      return { svg: s, marks };
    });
  }

  // Rect path with rounded top corners only, anchored at the baseline.
  function roundTop(x, base, w, hgt, r) {
    if (hgt <= 0) return "";
    r = Math.min(r, hgt, w / 2);
    const t = base - hgt;
    return `M${x},${base} L${x},${t + r} Q${x},${t} ${x + r},${t} L${x + w - r},${t} Q${x + w},${t} ${x + w},${t + r} L${x + w},${base} Z`;
  }

  // ---------- stacked bar ----------
  // items: [{ label, values: { key: number }, tip, cls }]; series: [{ key, name, slot }]
  function stackedBar(items, series, opts = {}) {
    const fmt = opts.fmt || String;
    const H = opts.height || 220;
    const fig = frame(opts, (W) => {
      const padL = 52,
        padB = 26,
        padT = 8;
      const sums = items.map((it) => series.reduce((a, s) => a + (it.values[s.key] || 0), 0));
      const ticks = niceTicks(Math.max(1e-9, ...sums));
      const top = ticks[ticks.length - 1];
      const plotW = W - padL - 4,
        plotH = H - padB - padT;
      const step = plotW / Math.max(items.length, 1);
      const bw = Math.max(2, Math.min(42, step - 2, step * 0.78));
      const sc = (v) => (v / top) * plotH;
      const s = svg("svg", { class: "chart", width: W, height: H, viewBox: `0 0 ${W} ${H}` });
      yAxis(s, ticks, (v) => padT + plotH - sc(v), fmt, padL, W);
      const marks = [];
      items.forEach((it, i) => {
        const x = padL + i * step + (step - bw) / 2;
        const text = it.tip || [it.label, ...series.map((se) => `${se.name} ${fmt(it.values[se.key] || 0)}`)].join("\n");
        const g = hover(svg("g", { class: `col ${it.cls || ""}` }, svg("rect", { class: "hit", x: padL + i * step, y: padT, width: step, height: plotH })), text);
        let base = padT + plotH;
        const visible = series.filter((se) => sc(it.values[se.key] || 0) >= 0.5);
        visible.forEach((se, j) => {
          const hh = sc(it.values[se.key] || 0);
          const gap = j > 0 && hh > 3 ? 1 : 0; // surface gap between segments
          const last = j === visible.length - 1;
          g.append(
            last
              ? svg("path", { class: "seg", d: roundTop(x, base - gap, bw, hh - gap, Math.min(4, bw / 3)), fill: fillFor(se.slot) })
              : svg("rect", { class: "seg", x, y: base - hh, width: bw, height: Math.max(hh - gap, 0.5), fill: fillFor(se.slot) }),
          );
          base -= hh;
        });
        s.append(g);
        marks.push({ node: g, tip: text });
      });
      xLabels(s, items.map((i) => i.label), (i) => padL + i * step + step / 2, H - 8, 44, plotW);
      return { svg: s, marks };
    });
    if (opts.legend !== false) fig.append(legend(series));
    return fig;
  }

  // ---------- share bar ----------
  // parts: [{ name, value, slot, help, onSelect }] drawn as one 100% bar, with
  // names written on segments wide enough to hold them.
  function shareBar(parts, opts = {}) {
    const fmtShare = opts.fmtShare || ((x) => (x * 100).toFixed(x >= 0.1 ? 0 : 1) + "%");
    const total = parts.reduce((a, p) => a + p.value, 0) || 1;
    const H = opts.height || 30;
    const fig = frame(opts, (W) => {
      const s = svg("svg", { class: "chart share-bar", width: W, height: H, viewBox: `0 0 ${W} ${H}` });
      let x = 0;
      const marks = [];
      parts.forEach((p) => {
        const w = (p.value / total) * W;
        if (w <= 0) return;
        const share = p.value / total;
        const text = `${p.name}  ${fmtShare(share)}${p.tip ? "\n" + p.tip : ""}`;
        const g = hover(svg("g", { class: `seg-g ${p.onSelect ? "clickable" : ""}`, onclick: p.onSelect || null }), text);
        g.append(svg("rect", { class: "seg", x: x + (x > 0 ? 1 : 0), y: 0, width: Math.max(w - (x > 0 ? 1 : 0), 0.8), height: H, fill: fillFor(p.slot) }));
        const label = `${p.name} ${fmtShare(share)}`;
        if (w > label.length * 6.2 + 12) g.append(svg("text", { class: "seg-label", x: x + 6, y: H / 2 + 4, text: label }));
        s.append(g);
        marks.push({ node: g, tip: text, onSelect: p.onSelect });
        x += w;
      });
      return { svg: s, marks };
    });
    if (opts.legend !== false) fig.append(legend(parts.map((p) => ({ name: `${p.name} ${fmtShare(p.value / total)}`, slot: p.slot, help: p.help }))));
    return fig;
  }

  // ---------- stacked area ----------
  // points: [{ values: { key }, tip }]; series: [{ key, name, slot }] bottom first.
  // events: [{ index, shape, cls, tip }] drawn as markers above the plot.
  function stackedArea(points, series, opts = {}) {
    const fmt = opts.fmt || String;
    const H = opts.height || 200;
    const fig = frame(opts, (W) => {
      const padL = 52,
        padB = opts.xLabels ? 24 : 8,
        padT = opts.events && opts.events.length ? 16 : 8;
      const n = points.length;
      const sums = points.map((p) => series.reduce((a, s) => a + (p.values[s.key] || 0), 0));
      const ticks = niceTicks(Math.max(1e-9, ...sums));
      const top = ticks[ticks.length - 1];
      const plotW = W - padL - 6,
        plotH = H - padT - padB;
      const x = (i) => padL + (n <= 1 ? plotW / 2 : (i / (n - 1)) * plotW);
      const y = (v) => padT + plotH - (v / top) * plotH;
      const s = svg("svg", { class: "chart", width: W, height: H, viewBox: `0 0 ${W} ${H}` });
      yAxis(s, ticks, y, fmt, padL, W);
      const base = new Array(n).fill(0);
      for (const se of series) {
        const lo = base.slice();
        const hi = base.map((b, i) => b + (points[i].values[se.key] || 0));
        let d = `M${x(0)},${y(hi[0])}`;
        for (let i = 1; i < n; i++) d += ` L${x(i)},${y(hi[i])}`;
        for (let i = n - 1; i >= 0; i--) d += ` L${x(i)},${y(lo[i])}`;
        s.append(svg("path", { class: "area", d: d + " Z", fill: fillFor(se.slot) }));
        let line = `M${x(0)},${y(hi[0])}`;
        for (let i = 1; i < n; i++) line += ` L${x(i)},${y(hi[i])}`;
        s.append(svg("path", { class: "area-edge", d: line }));
        hi.forEach((v, i) => (base[i] = v));
      }
      if (opts.xLabels) xLabels(s, opts.xLabels, x, H - 6, 70, plotW);
      // Crosshair + tooltip, nearest point.
      const cross = svg("line", { class: "crosshair", x1: 0, x2: 0, y1: padT, y2: padT + plotH, visibility: "hidden" });
      const tipFor = (i) => points[i].tip || series.map((se) => `${se.name} ${fmt(points[i].values[se.key] || 0)}`).join("\n");
      const hit = svg("rect", {
        class: "hit",
        x: padL,
        y: padT,
        width: plotW,
        height: plotH,
        onmousemove: (e) => {
          const r = hit.getBoundingClientRect();
          const i = Math.round(((e.clientX - r.left) / r.width) * (n - 1));
          cross.setAttribute("x1", x(i));
          cross.setAttribute("x2", x(i));
          cross.setAttribute("visibility", "visible");
          showTip(e, tipFor(Math.max(0, Math.min(n - 1, i))));
        },
        onmouseleave: () => {
          cross.setAttribute("visibility", "hidden");
          hideTip();
        },
      });
      s.append(cross, hit);
      const marks = [];
      for (const ev of opts.events || []) {
        const cx = x(ev.index);
        const g = hover(
          svg("g", { class: `ev ${ev.cls || ""}` }, svg("line", { x1: cx, x2: cx, y1: padT - 4, y2: padT + plotH }), marker(ev.shape, cx, padT - 7, ev.cls)),
          ev.tip,
        );
        s.append(g);
        marks.push({ node: g, tip: ev.tip });
      }
      // Keyboard: step through events if any, else through ~24 sample points.
      if (!marks.length) {
        const stepN = Math.max(1, Math.ceil(n / 24));
        for (let i = 0; i < n; i += stepN) {
          const dot = svg("circle", { class: "kbd-dot", cx: x(i), cy: y(sums[i]), r: 3.5 });
          s.append(dot);
          marks.push({ node: dot, tip: tipFor(i) });
        }
      }
      return { svg: s, marks };
    });
    if (opts.legend !== false) fig.append(legend([...series].reverse().concat(opts.eventLegend || [])));
    return fig;
  }

  // ---------- heatmap ----------
  // cells: [{ row, col, value, tip, empty }]; rows/cols: label arrays.
  function heatmap(cells, opts = {}) {
    const rows = opts.rows,
      cols = opts.cols;
    const fig = frame(opts, (W) => {
      const padL = opts.padL || 36,
        padT = 18;
      const gap = 2;
      const cw = Math.min(opts.maxCell || 22, (W - padL) / cols.length - gap);
      const ch = Math.min(opts.maxCell || 22, Math.max(12, cw));
      const H = padT + rows.length * (ch + gap);
      const s = svg("svg", { class: "chart heatmap", width: W, height: H, viewBox: `0 0 ${W} ${H}` });
      const max = Math.max(1e-9, ...cells.map((c) => c.value));
      rows.forEach((r, i) => s.append(svg("text", { class: "axis", x: padL - 6, y: padT + i * (ch + gap) + ch / 2 + 3, "text-anchor": "end", text: r })));
      const every = Math.ceil(cols.length / Math.max(1, Math.floor((W - padL) / 40)));
      cols.forEach((c, j) => {
        if (c && j % every === 0) s.append(svg("text", { class: "axis", x: padL + j * (cw + gap), y: 11, text: c }));
      });
      const marks = [];
      for (const c of cells) {
        // Six steps of one hue; zero is the neutral fill.
        const q = c.value > 0 ? Math.min(6, 1 + Math.floor((c.value / max) * 5.999)) : 0;
        const cell = hover(
          svg("rect", { class: `cell q${q} ${c.empty ? "none" : ""}`, x: padL + c.col * (cw + gap), y: padT + c.row * (ch + gap), width: cw, height: ch, rx: 2 }),
          c.tip,
        );
        s.append(cell);
        if (!c.empty) marks.push({ node: cell, tip: c.tip });
      }
      return { svg: s, marks };
    });
    if (opts.legend !== false) {
      const scale = h("div", { class: "legend heat-legend" }, h("span", { text: "Less" }));
      for (let q = 0; q <= 6; q++) scale.append(h("i", { class: `heat-q q${q}` }));
      scale.append(h("span", { text: "More" }));
      fig.append(scale);
    }
    return fig;
  }

  // ---------- waterfall ----------
  // steps: [{ label, value, total }] — totals are drawn from zero, the rest as
  // signed changes from the running level. Every bar carries its value.
  function waterfall(steps, opts = {}) {
    const fmt = opts.fmt || String;
    const fmtDelta = opts.fmtDelta || ((v) => (v > 0 ? "+" : "−") + fmt(Math.abs(v)));
    const H = opts.height || 200;
    return frame(opts, (W) => {
      const padL = 52,
        padB = 34,
        padT = 18;
      let level = 0;
      const geo = steps.map((st) => {
        if (st.total) {
          level = st.value;
          return { lo: 0, hi: st.value, end: level };
        }
        const from = level;
        level += st.value;
        return { lo: Math.min(from, level), hi: Math.max(from, level), end: level };
      });
      const ticks = niceTicks(Math.max(1e-9, ...geo.map((g) => g.hi)));
      const top = ticks[ticks.length - 1];
      const plotW = W - padL - 4,
        plotH = H - padT - padB;
      const step = plotW / steps.length;
      const bw = Math.min(64, step * 0.6);
      const y = (v) => padT + plotH - (v / top) * plotH;
      const s = svg("svg", { class: "chart waterfall", width: W, height: H, viewBox: `0 0 ${W} ${H}` });
      yAxis(s, ticks, y, fmt, padL, W);
      const marks = [];
      steps.forEach((st, i) => {
        const g0 = geo[i];
        const x = padL + i * step + (step - bw) / 2;
        const cls = st.total ? "wf-total" : st.value >= 0 ? "wf-up" : "wf-down";
        const text = st.tip || `${st.label}\n${st.total ? fmt(st.value) : fmtDelta(st.value)}`;
        const g = hover(svg("g", { class: cls }), text);
        g.append(svg("rect", { class: "wf-bar", x, y: y(g0.hi), width: bw, height: Math.max(1, y(g0.lo) - y(g0.hi)), rx: 3 }));
        if (i < steps.length - 1) {
          s.append(svg("line", { class: "wf-link", x1: x + bw, x2: x + step, y1: y(g0.end), y2: y(g0.end) }));
        }
        g.append(svg("text", { class: "wf-value", x: x + bw / 2, y: y(g0.hi) - 5, "text-anchor": "middle", text: st.total ? fmt(st.value) : fmtDelta(st.value) }));
        g.append(svg("text", { class: "axis", x: x + bw / 2, y: H - 18, "text-anchor": "middle", text: st.label }));
        if (st.sub) g.append(svg("text", { class: "axis", x: x + bw / 2, y: H - 6, "text-anchor": "middle", text: st.sub }));
        s.append(g);
        marks.push({ node: g, tip: text });
      });
      return { svg: s, marks };
    });
  }

  // ---------- sparkline ----------
  function sparkline(values, opts = {}) {
    values = values.map((v) => (Number.isFinite(v) ? v : 0));
    const W = opts.width || 72,
      H = opts.height || 18;
    const max = Math.max(1e-9, ...values);
    const n = values.length;
    const x = (i) => (n <= 1 ? W / 2 : 1 + (i / (n - 1)) * (W - 4));
    const y = (v) => H - 2 - (v / max) * (H - 4);
    const s = svg("svg", { class: "spark", width: W, height: H, viewBox: `0 0 ${W} ${H}`, role: "img", "aria-label": opts.label || "trend" });
    if (!n) return s;
    let d = `M${x(0)},${y(values[0])}`;
    for (let i = 1; i < n; i++) d += ` L${x(i)},${y(values[i])}`;
    s.append(svg("path", { class: "spark-area", d: `${d} L${x(n - 1)},${H} L${x(0)},${H} Z` }), svg("path", { class: "spark-line", d }));
    s.append(svg("circle", { class: "spark-dot", cx: x(n - 1), cy: y(values[n - 1]), r: 2 }));
    return s;
  }

  window.Charts = {
    svg,
    bar,
    stackedBar,
    shareBar,
    stackedArea,
    heatmap,
    waterfall,
    sparkline,
    legend,
    swatch,
    marker,
    niceTicks,
    showTip,
    showTipAt,
    hideTip,
    fillFor,
    cssColor,
  };
})();
