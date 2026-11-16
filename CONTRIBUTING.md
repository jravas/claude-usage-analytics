# Contributing

## Previewing the UI

The front end in `ui/` is plain HTML, CSS and JS with no build step. Outside
Tauri it falls back to `window.mockInvoke`, so you can work on it in any browser
with a static server and mock data.

```bash
python3 -m http.server 8731 --bind 127.0.0.1 --directory ui
```

Then open <http://localhost:8731/dev/>. `ui/dev/index.html` loads the real
`ui/index.html` (markup, styles and scripts, in order) with mock data in place of
the Tauri commands, so the preview never drifts from the app.

Two kinds of mock data:

- `ui/dev/mock.js`: synthetic, committed. Made-up projects and sessions from a
  seeded generator; filters, ranges and session pages all work. Use it for
  screenshots and anything you share.
- `ui/dev/mock.local.js`: your own numbers, git-ignored. Build it from your local
  index with:

  ```bash
  scripts/make-mock.sh
  ```

  It runs the `usage` CLI (`… --json`) for each command and range the UI asks for,
  plus `session <id> --json` for the top sessions (`--sessions N`, default 10).
  Pass `--copy-index` to query a temporary copy of the index, for example while a
  build with a different schema shares the real one. **The file contains session
  titles and project paths: never commit it, and delete it before
  `cargo tauri build`, since the app bundles everything under `ui/`.**

The dev page uses local data when `mock.local.js` exists, otherwise synthetic. A
bar in the corner switches data, theme and state; the same options work as URL
parameters:

| Parameter | Values |
| --- | --- |
| `data` | `local`, `synthetic` |
| `theme` | `light`, `dark` (default: follow the system) |
| `state` | `empty`, `no-transcripts`, `indexing`, `error` |
| `intro` | `1` shows the first-run screen again |

The `index-updated` button fires the event the core emits when the index changes.

With Claude Code's built-in browser, add this to `.claude/launch.json` and start
`ui-dev`:

```json
{
  "version": "0.0.1",
  "configurations": [
    {
      "name": "ui-dev",
      "runtimeExecutable": "python3",
      "runtimeArgs": ["-m", "http.server", "8731", "--bind", "127.0.0.1", "--directory", "ui"],
      "port": 8731
    }
  ]
}
```

### UI conventions

- Charts live in `ui/charts.js` (bar, stacked bar, share bar, stacked area,
  heatmap, waterfall, sparkline). Each is a focusable figure with a text summary;
  arrow keys move between marks. Series use colour slots `--c1`…`--c8` (light
  and dark steps in `styles.css`, validated for colour-vision deficiency) plus a
  fill pattern per slot, so identity never relies on hue alone.
- Tables use `DataTable` in `ui/table.js`: sortable columns, search, copy as
  Markdown or CSV, save CSV. Give per-session tables an id that includes the
  session id.
- Every metric name goes through `Glossary.term()` (`ui/glossary.js`) so it
  explains itself on hover and focus. Add new metrics there first.
- Check light and dark (`?theme=`) and keyboard-only use before committing.
