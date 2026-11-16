# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.2] - 2026-11-16

### Fixed

- Table name columns keep a readable width and stay pinned; narrow windows
  scroll the numbers sideways instead of squeezing names away.

## [0.1.1] - 2026-11-16

### Changed

- The menu bar shows today against your own usual day up to the same time
  ("1.4× usual", in price-weighted tokens) instead of API cost and a
  percentage against last week. A setting (and a submenu) switches it to the
  active session's context, weighted tokens, raw tokens, API cost or icon
  only. The menu explains today and this week against usual, the session's
  context and compactions, and that plan limits are not in local data.
- The Compare page is gone for now; `usage export` and `usage compare` still
  work from the command line.
- Removed the "Weekdays only" filter.
- Big numbers on the Overview cards say what unit they are in.
- The app's unit switch defaults to weighted tokens, labelled "Usage"; cost
  stays one click away.

## [0.1.0] - 2026-11-16

### Added

- Incremental index of Claude Code transcripts (SQLite), counting each API
  response once, with schema migrations that keep history.
- Overview: today and this week against last week at the same time of day,
  a waterfall of why it changed, and cost, requests, output or context per day.
- Breakdowns by model, project, session, effort, main thread against
  subagents, hour of day and weekday, with sortable tables, search and CSV or
  Markdown export.
- Cost model from Anthropic's published API prices, with a check against Claude
  Code's own cost estimate (`usage cost-check`) and a unit switch: cost,
  price-weighted tokens or raw tokens.
- Session detail: what filled the context (exact from request snapshots when
  recorded), context over the session with compactions, cache rebuilds and
  subagents marked, prompts, largest single items, tools and MCP servers,
  fixed overhead per request, subagents and warnings.
- Insights: ranked findings with the tokens each accounts for and an action.
- Compare: privacy-safe summary export and a week-by-week comparison.
- Menu bar item with today's cost, opt-in notifications, launch at login.
- Claude Desktop session titles and connector names.
- `usage` command-line tool with the same features.
