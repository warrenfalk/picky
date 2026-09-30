# Codex sessions

Picky includes the same local session families as `/warren-desktop/codex-waybar`.
It runs one `codex agents --json --watch --local unix://` process while open.
Updates arrive from the feed; typing searches the latest snapshot without
starting another process or querying the server again.

With an empty query, working and attention-needed sessions appear near the top,
below notifications. Idle sessions appear below applications, windows, and
workspaces, ordered by `updatedAt`, newest first. Unloaded idle history
and unnamed, empty idle drafts are hidden, matching Waybar. The CLI aggregates
subagents into their root session, so Picky does not list family members twice.

Search matches the title, full working directory, status, and `Codex`. Text
search uses relevance with a bonus for active sessions and a strong penalty for
idle sessions; equally relevant idle sessions keep their recency order. Each row
shows the title, all current statuses, and the directory. While typing, the first
match stays selected. While navigating
results, live updates preserve the selected item by identity; if it disappears,
selection moves to the nearest remaining row.

Status icons use both shape and color: a green play icon for working, a muted
pause icon for idle, a red cross for errors, an amber exclamation mark for
approval, a blue speech bubble for input, and a purple exclamation mark for
other attention. When a family has multiple statuses, the icon uses Waybar's
priority: error, approval, input, other attention, working, then idle. The
subtitle still shows every status. Icons update with the live feed.

Enter or a click runs `codex agents --local unix:// --focus=SESSION_ID`. This
focuses the existing TUI, including its Kitty tab or pane, and closes Picky on
success. It does not launch or resume a session. A loaded session can have no
attached TUI; in that case the focus error is displayed and Picky stays open.

The `codex` executable must be on Picky's `PATH` and support the fork's `agents
--json --watch` and `agents --focus` options. The local app server and target TUI
must support focusing, with Kitty remote control enabled, just as for Waybar.
Picky uses the installed Codex rather than pinning a second CLI package.

Disconnected or invalid snapshots clear Codex rows and display a Codex warning.
Other results remain available. The CLI reconnects to its server; if the CLI
exits or cannot start, Picky retries after three seconds. The watcher is killed
when its subscription ends, and Linux also terminates it when Picky exits or is
killed. Snapshot errors never include raw conversation text.

Run `nix develop -c cargo test` for parser, search, live-selection, recovery, and
watcher-lifecycle checks. Desktop verification requires running Picky in the
Niri session with `ICED_BACKEND=wgpu` and selecting an attached Codex session.
