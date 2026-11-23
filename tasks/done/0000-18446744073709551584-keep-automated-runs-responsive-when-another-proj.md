Keep automated runs responsive when another project is updating the agent registry.

Completion note:
COMPLETED 2026-11-23: Live automated runs retry shared registry contention while their lease and worker ownership remain valid. Resumed activity switches to Running after Codex confirms the session. Kanban r retries in the background with immediate feedback, m moves tasks, and Shift+M opens Models. Board navigation caches task reads and formats visible rows. Verified against the live FishECS worker and with cargo fmt, Clippy, and the full test suite (981 passed, one ignored).

clt:manual <!-- clt-coordination: {"id":"888d473d-02ad-44de-83ce-74bc06518902","depends_on":[],"scope":{"intent":"Keep live run observation and retry input responsive and report when a resumed worker is running","files":[],"modules":["runner","agent registry","TUI retry and board refresh","worker activity"],"interfaces":[],"resources":[],"behavior":{}},"reviews":[]} --> codex:01a20913-9214-7d13-be1a-2221330c310a
