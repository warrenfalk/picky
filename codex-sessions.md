# Codex sessions

Picky includes the same local session families as `/warren-desktop/codex-waybar`.
It runs one `codex agents --json --watch --local unix://` process for the entire
resident Picky process lifetime, including while the picker window is hidden.
Updates arrive from the feed; typing searches the latest snapshot without
starting another process or querying the server again.

Escape, focus loss, compositor close requests, and successful activation dismiss
the window while retaining the process and its indexes. Running `picky` again
opens a fresh window or focuses the existing one. Each opening clears the query,
selection, and thumbnails. Run `picky --quit` to terminate the resident process.
The instance lock and activation socket are scoped to the Niri session.

One persistent `niri msg --json event-stream` process also maintains the window
and workspace indexes while hidden. It reconnects after a disconnect and replaces
the indexes with fresh snapshots; searches use direct Niri queries while the feed
is unavailable. Visible results coalesce relevant changes at 100 ms intervals,
preserving selection by identity while navigating. Layout-only events do not
refresh results. Thumbnails are fetched only for visible rows and regenerated
on each opening. Sources without live feeds are searched again on opening;
application entries and icon indexes remain cached for the process lifetime.

With an empty query, sessions appear below notifications in this order: error,
approval, input, other attention, unread, then working.
Idle sessions appear below applications, windows, and
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
other attention, plus a blue dot for unread. When a family has multiple statuses,
the icon uses this priority: error, approval, input, other attention, unread,
working, then idle. Equally relevant search results use this order too. The
subtitle still shows every status. Icons update with the live feed.

Enter or a click runs `codex agents --local unix:// --focus=SESSION_ID`. This
focuses the existing TUI, including its Kitty tab or pane, and dismisses the picker on
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
