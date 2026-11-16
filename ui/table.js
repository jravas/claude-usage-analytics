// Tables with sortable columns, a search box, CSV export and copy as Markdown.
// Exposes `window.DataTable(opts)`, which returns a <section>.
//
// opts:
//   id          stable key; sort, search and "show all" survive re-renders under
//               it. Include the session id for per-session tables so state never
//               leaks from one session to the next.
//   columns     [{ key, label, term, value(r), render(r), text(r), left, width, cls }]
//                 value  raw value for sorting and CSV (number or string)
//                 render node or string for the cell (default: text)
//                 text   plain text for search and Markdown (default: value)
//                 term   glossary name for the header tooltip (default: label)
//   rows        array of row objects
//   rowKey(r)   stable id for focus restore (default: index)
//   sort        { key, dir: "asc"|"desc" } initial sort; omit to keep row order
//   limit       show this many rows, with a "Show all" button
//   onOpen(r)   makes rows activatable by click, Enter or Space
//   rowClass(r) extra class per row
//   name        file name and caption for exports, e.g. "projects-14d"
//   label       accessible name of the table
//   tools       false to hide search and export (tiny tables)

(function () {
  const STATE = new Map();
  const $h = (tag, attrs = {}, ...kids) => {
    const n = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs)) {
      if (k === "class") n.className = v;
      else if (k === "text") n.textContent = v;
      else if (k.startsWith("on")) n.addEventListener(k.slice(2), v);
      else if (v !== null && v !== undefined && v !== false) n.setAttribute(k, v);
    }
    for (const c of kids.flat()) if (c !== null && c !== undefined && c !== false) n.append(c.nodeType ? c : document.createTextNode(String(c)));
    return n;
  };

  const textOf = (c, r) => {
    if (c.text) return String(c.text(r) ?? "");
    const v = c.value(r);
    return v == null ? "" : String(v);
  };

  function csvCell(v) {
    const s = v == null ? "" : String(v);
    return /[",\n]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
  }

  function toCsv(columns, rows) {
    const head = columns.map((c) => csvCell(c.label)).join(",");
    const body = rows.map((r) =>
      columns
        .map((c) => {
          const v = c.value(r);
          return csvCell(typeof v === "number" ? (Number.isInteger(v) ? v : Number(v.toFixed(4))) : textOf(c, r));
        })
        .join(","),
    );
    return [head, ...body].join("\n") + "\n";
  }

  function toMarkdown(columns, rows) {
    const esc = (s) => s.replace(/\|/g, "\\|").replace(/\n/g, " ");
    const head = `| ${columns.map((c) => esc(c.label)).join(" | ")} |`;
    const sep = `| ${columns.map((c) => (c.left ? "---" : "---:")).join(" | ")} |`;
    const body = rows.map((r) => `| ${columns.map((c) => esc(textOf(c, r))).join(" | ")} |`);
    return [head, sep, ...body].join("\n") + "\n";
  }

  async function copyText(text) {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch {
      const ta = $h("textarea", { style: "position:fixed;opacity:0" });
      ta.value = text;
      document.body.append(ta);
      ta.select();
      const ok = document.execCommand("copy");
      ta.remove();
      return ok;
    }
  }

  function download(name, text) {
    const url = URL.createObjectURL(new Blob([text], { type: "text/csv;charset=utf-8" }));
    const a = $h("a", { href: url, download: name });
    document.body.append(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(url), 2000);
  }

  function DataTable(opts) {
    const cols = opts.columns.map((c) => ({ value: (r) => r[c.key], ...c }));
    const st = STATE.get(opts.id) || { sort: opts.sort ? opts.sort.key : null, dir: opts.sort ? opts.sort.dir : "desc", q: "", all: false };
    STATE.set(opts.id, st);
    const rowKey = opts.rowKey || ((r, i) => i);
    const name = opts.name || opts.id.replace(/[^a-z0-9]+/gi, "-");

    const section = $h("section", { class: "dtable" });
    const status = $h("span", { class: "dt-status muted", "aria-live": "polite" });
    const table = $h("table", { class: `breakdown ${opts.cls || ""}`, "aria-label": opts.label || name });
    const colgroup = $h("colgroup", {}, cols.map((c) => $h("col", c.width ? { style: `width:${c.width}` } : {})));
    const thead = $h("thead");
    const tbody = $h("tbody");
    table.append(colgroup, thead, tbody);

    const visible = () => {
      const q = st.q.trim().toLowerCase();
      let rows = opts.rows.map((r, i) => ({ r, i }));
      if (q) rows = rows.filter(({ r }) => cols.some((c) => textOf(c, r).toLowerCase().includes(q)));
      if (st.sort) {
        const c = cols.find((x) => x.key === st.sort);
        if (c) {
          const m = st.dir === "asc" ? 1 : -1;
          rows.sort((a, b) => {
            const va = c.value(a.r),
              vb = c.value(b.r);
            if (va == null && vb == null) return a.i - b.i;
            if (va == null) return 1;
            if (vb == null) return -1;
            const d = typeof va === "number" && typeof vb === "number" ? va - vb : String(va).localeCompare(String(vb));
            return d * m || a.i - b.i;
          });
        }
      }
      return rows;
    };

    const drawHead = () => {
      thead.replaceChildren(
        $h(
          "tr",
          {},
          cols.map((c) => {
            const active = st.sort === c.key;
            const def = window.Glossary && window.Glossary.define(c.term || c.label);
            const btn = $h(
              "button",
              {
                class: "sort",
                "data-fk": `sort:${opts.id}:${c.key}`,
                "aria-description": def || null,
                onclick: () => {
                  if (st.sort === c.key) st.dir = st.dir === "asc" ? "desc" : "asc";
                  else {
                    st.sort = c.key;
                    st.dir = c.left ? "asc" : "desc";
                  }
                  drawHead();
                  drawBody();
                },
              },
              $h("span", { class: def ? "term-h" : "", text: c.label }),
              $h("span", { class: "arrow", "aria-hidden": "true", text: active ? (st.dir === "asc" ? "▲" : "▼") : "" }),
            );
            if (def) {
              const show = () => window.Charts.showTipAt(btn, `${c.term || c.label}\n${def}`);
              btn.addEventListener("mouseenter", show);
              btn.addEventListener("focus", show);
              btn.addEventListener("mouseleave", window.Charts.hideTip);
              btn.addEventListener("blur", window.Charts.hideTip);
            }
            return $h("th", { scope: "col", class: c.left ? "left" : "", "aria-sort": active ? (st.dir === "asc" ? "ascending" : "descending") : null }, btn);
          }),
        ),
      );
    };

    const drawBody = () => {
      const rows = visible();
      const lim = opts.limit && !st.all && !st.q ? opts.limit : Infinity;
      const shown = rows.slice(0, lim);
      tbody.replaceChildren(
        ...shown.map(({ r, i }) => {
          const tr = $h("tr", { class: `${opts.onOpen ? "link" : ""} ${opts.rowClass ? opts.rowClass(r) : ""}`, "data-fk": `row:${opts.id}:${rowKey(r, i)}` });
          if (opts.onOpen) {
            tr.tabIndex = 0;
            tr.setAttribute("role", "link");
            tr.addEventListener("click", (e) => {
              if (!window.getSelection().toString()) opts.onOpen(r);
            });
          }
          for (const c of cols) {
            const v = c.render ? c.render(r) : textOf(c, r);
            tr.append($h("td", { class: `${c.left ? "left" : ""} ${c.cls || ""}` }, v));
          }
          return tr;
        }),
      );
      if (!shown.length) tbody.append($h("tr", { class: "zero" }, $h("td", { colspan: cols.length, class: "left", text: st.q ? `No rows match “${st.q}”.` : "No rows." })));
      status.textContent = st.q ? `${rows.length} of ${opts.rows.length} rows` : rows.length > shown.length ? `${shown.length} of ${rows.length} rows` : `${rows.length} rows`;
      more.hidden = !(opts.limit && rows.length > opts.limit && !st.q);
      more.textContent = st.all ? "Show fewer" : `Show all ${rows.length}`;
    };

    // Arrow keys move between activatable rows; Enter/Space opens.
    tbody.addEventListener("keydown", (e) => {
      const tr = e.target.closest("tr");
      if (!tr || e.target !== tr) return;
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        const next = e.key === "ArrowDown" ? tr.nextElementSibling : tr.previousElementSibling;
        if (next && next.tabIndex === 0) next.focus();
        e.preventDefault();
      } else if ((e.key === "Enter" || e.key === " ") && opts.onOpen) {
        const all = visible();
        const idx = [...tbody.children].indexOf(tr);
        if (all[idx]) opts.onOpen(all[idx].r);
        e.preventDefault();
      }
    });

    const more = $h("button", {
      class: "btn more",
      onclick: () => {
        st.all = !st.all;
        drawBody();
      },
    });

    const flash = (msg) => {
      status.textContent = msg;
      setTimeout(drawBody, 2200);
    };

    if (opts.tools !== false) {
      const search = $h("input", {
        type: "search",
        class: "dt-search",
        placeholder: "Search",
        "aria-label": `Search ${opts.label || "table"}`,
        "data-fk": `q:${opts.id}`,
        value: st.q,
        oninput: (e) => {
          st.q = e.target.value;
          drawBody();
        },
        onkeydown: (e) => {
          if (e.key === "Escape" && st.q) {
            st.q = "";
            e.target.value = "";
            drawBody();
            e.stopPropagation();
          }
        },
      });
      const exportRows = () => visible().map((x) => x.r);
      section.append(
        $h(
          "div",
          { class: "dt-tools" },
          search,
          status,
          $h("span", { class: "spacer" }),
          $h("button", {
            class: "btn small",
            text: "Copy Markdown",
            title: "Copy the rows shown (search and sort applied) as a Markdown table",
            onclick: async () => flash((await copyText(toMarkdown(cols, exportRows()))) ? "Copied as Markdown" : "Copy failed"),
          }),
          $h("button", {
            class: "btn small",
            text: "Copy CSV",
            title: "Copy the rows as CSV with unrounded numbers",
            onclick: async () => flash((await copyText(toCsv(cols, exportRows()))) ? "Copied as CSV" : "Copy failed"),
          }),
          $h("button", {
            class: "btn small",
            text: "Save CSV",
            title: "Save the rows as a CSV file",
            onclick: () => {
              download(`${name}.csv`, toCsv(cols, exportRows()));
              flash(`Saved ${name}.csv`);
            },
          }),
        ),
      );
    }
    section.append($h("div", { class: "table-scroll" }, table), more);
    drawHead();
    drawBody();
    return section;
  }

  DataTable.toCsv = toCsv;
  DataTable.toMarkdown = toMarkdown;
  DataTable.copyText = copyText;
  window.DataTable = DataTable;
})();
