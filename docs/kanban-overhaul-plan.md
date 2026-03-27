# Kanban UI Overhaul Plan — Cline-inspired Agent Orchestration Dashboard

**Branch:** `feat/kanban-ui-overhaul`  
**Date:** 2026-03-27  
**Author:** Spacebot planning pass

---

## Visual Reference — Cline Kanban Demo Analysis

Extracted 38 frames from three demo videos at https://cline.bot/kanban. Key observations:

### Video 1 — "Watch the board, not the terminals"
- **Overall layout**: A full-width dark-themed kanban board with a collapsible left sidebar. The sidebar contains an AI chat interface labelled "Cline" — a natural-language prompt box at the bottom where the user types project descriptions or commands.
- **Column design**: 4-6 status columns rendered as narrow vertical strips with subtle rounded corners and very low-opacity column backgrounds. Column headers show the status name and a small count badge.
- **Task cards**: Cards have a clean flat design with:
  - Title in medium-weight text at top
  - A small coloured priority chip (e.g. "High" in amber, "Critical" in red)
  - A 1–2 line **live activity preview** below the title showing the last output line from the agent (e.g., "Reading file src/api/tasks.rs…")
  - A thin animated progress bar or pulsing dot when the task is actively running
- **Card colours**: Cards are near-black (`#111` / `#0f0f0f`) with a subtle `1px` border. Active/in-progress cards get a faint violet left border accent.
- **Sidebar agent**: Visible typing into a prompt box. The sidebar is ~280px wide, and the board takes the remaining width. A "Break down project" or "Create tasks" button is visible.
- **Animation**: Cards animate smoothly between columns as status changes. New tasks appear with a fade+scale-up transition.

### Video 2 — "Unblock agents quickly. No more issue hunting."
- **Split-panel layout**: When a task card is clicked, the right half of the viewport becomes a **task detail panel** (roughly 50/50 split). The kanban columns compress to fit the left half.
- **Diff view**: The right panel shows a **unified git diff** with syntax-highlighted additions (green) and deletions (red), rendered inline. Line numbers visible on the left margin. The diff scrolls independently.
- **Agent activity stream**: Below or beside the diff is a real-time agent output feed — tool call names, file paths being read/written, and brief status text scroll upward as the agent works.
- **Inline commenting**: A "Add comment" affordance appears on hover over diff lines (a `+` button on the line number gutter). Clicking opens a small text input inline. Comments are forwarded to the running agent.
- **Toolbar**: Above the diff, a small toolbar shows: branch name, commit SHA, file changed count, and "Request changes" / "Approve" buttons.

### Video 3 — "Chain dependent tasks, manually or automatically."
- **Task dependency graph**: A small visual graph (DAG) is shown either below the task detail or as a separate view. Tasks are nodes; dependency arrows connect them. Completed tasks show a green check. Blocked tasks (dependency not met) are visually greyed out.
- **Chain creation UI**: Drag a "link" handle from one task card to another to create a dependency. A confirmation popover appears.
- **Auto-chain modal**: A "Break down with auto-commit" button opens a modal where the user describes a project and Cline generates a set of linked tasks with dependency chains visualised before accepting.
- **Parallelization**: Tasks that can run in parallel are shown side-by-side in the graph, not sequentially. A "Max parallelization" label appears.
- **Status propagation**: When a task completes, its downstream dependents automatically move from "Blocked" → "Ready" (animated transition visible in the frames).

---

## Current State of Spacebot Kanban

File: `interface/src/routes/AgentTasks.tsx`

### What exists:
- **5-column board**: `pending_approval`, `backlog`, `ready`, `in_progress`, `done`
- **Static cards**: Title, priority badge, subtask progress bar, worker badge
- **Quick actions**: Approve / Execute / Mark Done buttons on card
- **Create task dialog**: Modal form with title, description, priority, status
- **Detail dialog**: Modal overlay showing all task fields, subtasks, metadata; approve/execute/delete/reopen actions
- **SSE reactivity**: `taskEventVersion` counter from `useLiveContext` triggers refetch on `task_updated` events
- **Animation**: `framer-motion` `AnimatePresence` on cards (fade + scale)

### What's missing:
- No drag-and-drop between columns
- No real-time agent output on cards (worker_id badge only)
- No diff view / worktree viewer
- No inline commenting on diffs
- No task dependency chaining UI
- No sidebar decomposition agent
- No activity timeline / history
- No git UI integration
- No script shortcut buttons
- Visual polish is functional but minimal

### Backend task API endpoints (Rust/Axum):
```
GET    /agents/tasks              → list_tasks
GET    /agents/tasks/{number}     → get_task
POST   /agents/tasks              → create_task
PUT    /agents/tasks/{number}     → update_task
DELETE /agents/tasks/{number}     → delete_task
POST   /agents/tasks/{number}/approve  → approve_task
POST   /agents/tasks/{number}/execute  → execute_task
GET    /events                    → SSE (task_updated events)
```

### Task data model:
```typescript
interface TaskItem {
  id: string;
  agent_id: string;
  task_number: number;
  title: string;
  description?: string;
  status: "pending_approval" | "backlog" | "ready" | "in_progress" | "done";
  priority: "critical" | "high" | "medium" | "low";
  subtasks: { title: string; completed: boolean }[];
  metadata: Record<string, unknown>;
  source_memory_id?: string;
  worker_id?: string;
  created_by: string;
  approved_at?: string;
  approved_by?: string;
  completed_at?: string;
  created_at: string;
  updated_at: string;
}
```

---

## Proposed Features — Priority Ordered

### 1. Drag-and-Drop Column Movement (Priority: High | Complexity: M)

**What**: Drag task cards between status columns. Drop onto a column header or into the card list area.

**Why**: Core Kanban interaction. Currently users must open the detail dialog and change a dropdown.

**Files to change**:
- `interface/src/routes/AgentTasks.tsx` — add `@dnd-kit/core` drag context, make `KanbanColumn` a drop target, make `TaskCard` draggable
- No backend changes needed — `updateTask` API already supports status changes

**New backend endpoints**: None  
**Complexity**: M

**Implementation notes**:
- Use `@dnd-kit/core` + `@dnd-kit/sortable` (already compatible with framer-motion layout animations)
- Optimistic update: move card immediately in local state, call `updateTask` in background
- Animate column count badges on drop

---

### 2. Real-time Agent Activity on Cards (Priority: High | Complexity: M)

**What**: Show the last agent status line on each in-progress card — e.g., "Reading file src/api/tasks.rs" or "Calling shell tool".

**Why**: Without this, the board is static while agents work. Watching the board means watching cards come alive.

**Files to change**:
- `interface/src/routes/AgentTasks.tsx` — `TaskCard` component reads `activeWorkers` and `liveTranscripts` from `useLiveContext`
- `interface/src/hooks/useLiveContext.tsx` — already has `activeWorkers` and `liveTranscripts` (no change needed)

**New backend endpoints**: None (SSE already carries `tool_started` / `worker_text` events)  
**Complexity**: M

**Implementation notes**:
- Match `task.worker_id` to `activeWorkers[task.worker_id]` to get the live worker
- Display last `liveTranscripts[worker_id]` step as a truncated string
- Add a subtle pulsing dot or spinner on the card while the worker is active
- Fade the activity line in/out using framer-motion

---

### 3. Task Detail Side Panel with Real-time Diff View (Priority: High | Complexity: L)

**What**: Replace the modal dialog with a slide-in side panel (right 45% of the viewport). The panel shows:
  - Task title, status, priority, description, subtasks
  - **Real-time git diff** of the associated worktree (fetched and polled while `in_progress`)
  - **Agent activity stream** (live tool calls scrolling upward)
  - Action buttons: Approve, Execute, Mark Done, Reopen, Delete

**Why**: The modal is a dead-end — you can't see the board while it's open. The side panel lets users monitor the agent and review changes without context-switching.

**Files to change**:
- `interface/src/routes/AgentTasks.tsx` — replace `TaskDetailDialog` with `TaskDetailPanel` (side panel, not modal)
- `interface/src/api/client.ts` — add `getWorktreeDiff(agentId, taskNumber)` API call
- New component: `interface/src/components/DiffViewer.tsx` — unified diff renderer with syntax highlighting
- New component: `interface/src/components/AgentActivityFeed.tsx` — scrolling tool call stream

**New backend endpoints**:
```
GET /agents/tasks/{number}/diff?agent_id=...
```
Returns `{ diff: string, files_changed: string[], branch: string }` by running `git diff HEAD` in the task's worktree.

**Backend files**:
- `src/api/tasks.rs` — add `get_task_diff` handler
- `src/api/server.rs` — register the route

**Complexity**: L

---

### 4. Inline Comments → Agent (Priority: Medium | Complexity: M)

**What**: In the diff view, hover over a line to reveal a `+` button in the gutter. Click to open an inline comment input. Submitting the comment sends it as a message to the running worker (if active) or stores it for next execution.

**Why**: Mirrors the PR review workflow developers already know. Lets you guide the agent without leaving the Kanban board.

**Files to change**:
- `interface/src/components/DiffViewer.tsx` — add per-line comment affordance
- `interface/src/api/client.ts` — add `commentOnTask(agentId, taskNumber, comment, lineContext)` 
- New: `interface/src/components/InlineComment.tsx`

**New backend endpoints**:
```
POST /agents/tasks/{number}/comment
Body: { agent_id, comment: string, line_context?: string }
```
Appends to task `metadata.comments[]` and, if a worker is running for this task, sends the comment as a message to the worker channel.

**Complexity**: M

---

### 5. Task Dependency Chaining (Priority: Medium | Complexity: L)

**What**: 
- A task can declare `depends_on: number[]` (list of upstream task numbers)
- Blocked tasks (upstream not done) show a "Blocked" visual state on the card
- When upstream completes → downstream auto-transitions from `backlog` to `ready`
- UI: drag a "link handle" from one card to another; or set via detail panel dropdown

**Why**: Lets users model multi-step projects where agent work must happen in sequence.

**Files to change**:
- `src/tasks/store.rs` — add `depends_on: Vec<i64>` field to `Task` struct
- `src/tasks/store.rs` — after marking a task done, check dependents and auto-advance them
- `src/api/tasks.rs` — include `depends_on` in create/update requests
- `interface/src/routes/AgentTasks.tsx` — blocked state UI, dependency drag handle
- `interface/src/api/client.ts` — expose `depends_on` field

**New backend endpoints**: None (handled via `depends_on` field in update)  
**Complexity**: L

---

### 6. Sidebar Decomposition Agent (Priority: Medium | Complexity: L)

**What**: A collapsible left sidebar (~280px) with a chat prompt. User types a project description; the Cortex agent breaks it into tasks and creates them all atomically. Results appear on the board in real-time.

**Why**: This is the "Watch the board, not the terminals" pitch. The sidebar turns the board from a task tracker into an AI-driven planning tool.

**Files to change**:
- `interface/src/routes/AgentTasks.tsx` — add sidebar toggle, `TaskDecompositionSidebar` component
- New: `interface/src/components/TaskDecompositionSidebar.tsx`
- `interface/src/api/client.ts` — add `decomposeProject(agentId, description)` call

**New backend endpoints**:
```
POST /agents/tasks/decompose
Body: { agent_id, description: string }
```
Spawns a short-lived cortex worker that uses the `update_task` tool to create multiple tasks, then returns the created task IDs. The frontend polls/SSE for new tasks during decomposition.

**Complexity**: L

---

### 7. Activity Timeline on Task Detail (Priority: Medium | Complexity: S)

**What**: Below the task description in the detail panel, show a chronological timeline of status changes and agent actions:
```
[09:42] created by cortex
[09:43] approved by marc
[09:44] worker abc123 started
[09:44] → shell: cargo build
[09:45] → file_write: src/api/tasks.rs
[09:47] worker abc123 completed
[09:47] status → done
```

**Why**: Gives full traceability of what happened to a task without digging through logs.

**Files to change**:
- `src/tasks/store.rs` — add `events: Vec<TaskEvent>` to Task (or store separately in a `task_events` table)
- `src/api/tasks.rs` — expose events in `get_task` response
- `interface/src/routes/AgentTasks.tsx` — render timeline in detail panel
- `migrations/` — new migration for `task_events` table

**New backend endpoints**: None (part of `get_task` response)  
**Complexity**: S (if appended to metadata) / M (if proper DB table)

---

### 8. Git UI Panel (Priority: Low | Complexity: L)

**What**: A "Git" tab on the task detail panel showing:
- Current branch and recent commits
- Fetch / Pull / Push buttons
- File status (modified, staged, untracked)

**Why**: Lets users manage the worktree from the dashboard without switching to a terminal.

**New backend endpoints**:
```
GET  /agents/tasks/{number}/git/status
GET  /agents/tasks/{number}/git/log
POST /agents/tasks/{number}/git/fetch
POST /agents/tasks/{number}/git/push
```

**Complexity**: L

---

### 9. Script Shortcut Buttons (Priority: Low | Complexity: S)

**What**: Configurable quick-run buttons on the task detail panel (e.g., "Run Tests", "Build", "Lint"). Configured per-agent in the agent config.

**Why**: Common operations should be one click from the board.

**Files to change**:
- `interface/src/routes/AgentConfig.tsx` — add script shortcuts config section
- `interface/src/routes/AgentTasks.tsx` — render shortcut buttons in task detail
- `src/api/` — route to run a script in the worktree context

**Complexity**: S

---

### 10. Visual Polish (Priority: High | Complexity: S)

**What**:
- **Dark theme improvements**: Use `#0a0a0a` board background, `#111` card backgrounds (matches Cline's palette)
- **Column accent lines**: Thin 2px top border on each column header matching the status colour
- **Card hover states**: Lift shadow + border highlight on hover (already partially done)
- **Priority colour coding**: Left border accent on cards (red=critical, amber=high, accent=medium, muted=low)
- **Animated transitions**: Column count badges animate on change (framer-motion `AnimatePresence`)
- **In-progress pulse**: Animated gradient or dot on cards with active workers
- **Empty state illustrations**: SVG placeholder when a column is empty

**Files to change**:
- `interface/src/routes/AgentTasks.tsx` — card, column, toolbar styling updates
- `interface/src/index.css` or Tailwind config — any new utility classes

**Complexity**: S

---

## Implementation Phases

### Phase 1 — Quick Wins (1–2 days)
Goal: Board feels alive and polished.

| Feature | Files | Effort |
|---|---|---|
| Visual polish — card priority borders, dark palette, hover states | `AgentTasks.tsx` | S |
| Real-time agent activity on cards | `AgentTasks.tsx` + `useLiveContext` (no backend) | M |
| Activity timeline (metadata-based, no DB migration) | `AgentTasks.tsx`, `tasks.rs` | S |
| Drag-and-drop between columns | `AgentTasks.tsx` + `@dnd-kit` | M |

### Phase 2 — Core Power (3–5 days)
Goal: The board replaces the terminal for monitoring and reviewing.

| Feature | Files | Effort |
|---|---|---|
| Task detail side panel (replace modal) | `AgentTasks.tsx` | M |
| Real-time diff view | `DiffViewer.tsx`, `tasks.rs` (new route), `client.ts` | L |
| Agent activity feed in panel | `AgentActivityFeed.tsx` | S |
| Inline comments → agent | `DiffViewer.tsx`, `tasks.rs`, `client.ts` | M |

### Phase 3 — Advanced Orchestration (5–10 days)
Goal: The board becomes an AI-driven project management tool.

| Feature | Files | Effort |
|---|---|---|
| Task dependency chaining | `tasks store.rs`, `AgentTasks.tsx`, migration | L |
| Sidebar decomposition agent | `TaskDecompositionSidebar.tsx`, backend route | L |
| Git UI panel | Multiple backend routes + frontend tab | L |
| Script shortcut buttons | `AgentConfig.tsx`, new route | S |

---

## Architecture Notes

### Diff Endpoint Design
The worktree diff endpoint should run in the agent's worktree (git working directory). The `Task` struct should include a `worktree_path` field (or derive it from the worker's sandbox directory). The diff is returned as a raw unified diff string; the frontend renders it using a diff-splitting parser.

### Dependency Resolution
Task dependency resolution should happen in the task store on every `update_task` call that sets `status = done`. A simple SQL query finds tasks where all `depends_on` references are `done` and their own status is `backlog`, then sets them to `ready` and emits `task_updated` SSE events.

### Side Panel Layout
The side panel uses CSS `flex` with `basis-[45%]` on the panel and `min-w-0 flex-1` on the board. The board columns compress gracefully. On small screens (<768px), the panel overlays the board (modal-like).

### DnD Library
`@dnd-kit/core` + `@dnd-kit/sortable` is the correct choice:
- Works with React 18 + framer-motion layout animations
- No global DOM event side-effects
- Supports accessibility (keyboard drag, screen reader announcements)

---

## File Inventory

### Frontend — Modified
- `interface/src/routes/AgentTasks.tsx` — main kanban route (all phases)
- `interface/src/api/client.ts` — new API calls for diff, comments, decompose
- `interface/src/hooks/useLiveContext.tsx` — expose last agent status text per worker

### Frontend — New
- `interface/src/components/DiffViewer.tsx` — diff renderer with line-level comments
- `interface/src/components/AgentActivityFeed.tsx` — scrolling real-time tool call stream
- `interface/src/components/TaskDecompositionSidebar.tsx` — AI task breakdown sidebar
- `interface/src/components/InlineComment.tsx` — comment input anchored to a diff line

### Backend — Modified
- `src/api/tasks.rs` — add diff, comment, decompose, git endpoints
- `src/api/server.rs` — register new routes
- `src/tasks/store.rs` — add `depends_on`, `events` fields; dependency resolution logic

### Backend — New
- `src/api/git.rs` — git operations on worktree (status, log, fetch, push)
- `migrations/YYYYMMDD_task_events.sql` — task event log table

---

## References

- Cline Kanban videos: https://cline.bot/kanban
  - Section 01: Board overview + sidebar decomposition agent
  - Section 02: Diff view + inline commenting
  - Section 03: Task dependency chaining
- Spacebot task API: `src/api/tasks.rs`
- Spacebot SSE events: `src/api/system.rs` (events endpoint), `interface/src/hooks/useLiveContext.tsx`
- Current Kanban component: `interface/src/routes/AgentTasks.tsx`
