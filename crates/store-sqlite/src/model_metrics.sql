-- Created on every open, like task_models, so a store made before model metrics gains them
-- without being moved aside.

-- Every model the orchestrator or the user's effort chose for a Task: each launch, relaunch and
-- each message that hands a Task to a node with a model or an approach, with the provider it runs
-- on and the tags the orchestrator gave the task (areas and risks as JSON arrays). Stores made
-- before provider and tags gain those columns empty (`model_metrics::add_routing_columns`).
CREATE TABLE IF NOT EXISTS model_decisions (
 id INTEGER PRIMARY KEY,
 task_id TEXT NOT NULL REFERENCES tasks(id),
 request_id TEXT NOT NULL REFERENCES requests(id),
 agent_id TEXT,
 at INTEGER NOT NULL,
 kind TEXT NOT NULL CHECK(kind IN('launch','relaunch','send')),
 model TEXT,
 effort TEXT,
 approach TEXT CHECK(approach IS NULL OR approach IN('solo','subagents','workflow')),
 chosen_by TEXT NOT NULL CHECK(chosen_by IN('orchestrator','user')),
 reason TEXT,
 title TEXT NOT NULL,
 provider TEXT,
 task_kind TEXT,
 areas TEXT CHECK(areas IS NULL OR json_valid(areas)),
 risks TEXT CHECK(risks IS NULL OR json_valid(risks))
);
CREATE INDEX IF NOT EXISTS model_decisions_task ON model_decisions(task_id,at);
CREATE INDEX IF NOT EXISTS model_decisions_agent ON model_decisions(agent_id);
CREATE TRIGGER IF NOT EXISTS immutable_model_decision BEFORE UPDATE ON model_decisions
 BEGIN SELECT RAISE(ABORT,'model decisions are immutable'); END;
CREATE TRIGGER IF NOT EXISTS retain_model_decision BEFORE DELETE ON model_decisions
 BEGIN SELECT RAISE(ABORT,'model decisions are retained'); END;

-- What each session reported it spent, by model: the latest running totals of its provider, kept
-- until a week after its ended task is folded into task_metrics. `folded` turns 1 when its task's
-- fold read it, and back to 0 with each new report. `input` is what was neither read from nor
-- written to the prompt cache. `complete` turns 0 once a report went down without being empty,
-- which a session the provider restarted without its earlier spend does.
DROP TABLE IF EXISTS node_costs;
CREATE TABLE IF NOT EXISTS usage_sessions (
 session_id TEXT NOT NULL CHECK(length(session_id)>0),
 model TEXT NOT NULL CHECK(length(model)>0),
 provider TEXT NOT NULL,
 task_id TEXT,
 role TEXT NOT NULL CHECK(role IN('node','orchestrator')),
 cost_micros INTEGER,
 input INTEGER NOT NULL,
 output INTEGER NOT NULL,
 cache_read INTEGER NOT NULL,
 cache_write INTEGER NOT NULL,
 complete INTEGER NOT NULL DEFAULT 1 CHECK(complete IN(0,1)),
 folded INTEGER NOT NULL DEFAULT 0 CHECK(folded IN(0,1)),
 first_at INTEGER NOT NULL,
 last_at INTEGER NOT NULL,
 PRIMARY KEY(session_id,model)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS usage_sessions_task ON usage_sessions(task_id);

-- The model calls of each session, counted as its turns end, until its task is folded.
CREATE TABLE IF NOT EXISTS session_steps (
 session_id TEXT PRIMARY KEY CHECK(length(session_id)>0),
 task_id TEXT,
 steps INTEGER NOT NULL,
 subagent_steps INTEGER NOT NULL,
 looks INTEGER NOT NULL,
 polls INTEGER NOT NULL,
 tool_calls INTEGER NOT NULL,
 folded INTEGER NOT NULL DEFAULT 0 CHECK(folded IN(0,1)),
 last_at INTEGER NOT NULL
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS session_steps_task ON session_steps(task_id);

-- What each ended task came to, folded from its history, its feedback, its launches and what its
-- sessions spent. Kept 90 days, then added into metric_weeks.
CREATE TABLE IF NOT EXISTS task_metrics (
 task_id TEXT PRIMARY KEY,
 project TEXT,
 model TEXT,
 effort TEXT,
 approach TEXT,
 role TEXT,
 outcome TEXT NOT NULL CHECK(outcome IN('done','cancelled')),
 started_at INTEGER,
 finished_at INTEGER NOT NULL,
 clean INTEGER NOT NULL,
 change_rounds INTEGER NOT NULL,
 reviewer_rounds INTEGER NOT NULL,
 interventions INTEGER NOT NULL,
 reopened INTEGER NOT NULL,
 relaunched INTEGER NOT NULL,
 work_ms INTEGER NOT NULL,
 waiting_user_ms INTEGER NOT NULL,
 queued_ms INTEGER NOT NULL,
 blocked_ms INTEGER NOT NULL,
 measured INTEGER NOT NULL,
 sessions INTEGER NOT NULL,
 steps INTEGER NOT NULL,
 subagent_steps INTEGER NOT NULL,
 looks INTEGER NOT NULL,
 polls INTEGER NOT NULL,
 pr_merged INTEGER NOT NULL,
 pr_lines INTEGER,
 folded_revision INTEGER NOT NULL,
 provider TEXT,
 kind TEXT
) WITHOUT ROWID;

-- What each ended task's sessions spent, by the model that spent it.
CREATE TABLE IF NOT EXISTS task_model_usage (
 task_id TEXT NOT NULL,
 model TEXT NOT NULL,
 provider TEXT NOT NULL,
 cost_micros INTEGER,
 input INTEGER NOT NULL,
 output INTEGER NOT NULL,
 cache_read INTEGER NOT NULL,
 cache_write INTEGER NOT NULL,
 PRIMARY KEY(task_id,model)
) WITHOUT ROWID;

-- The tasks that ended more than 90 days ago, added up by week (its Monday), project and the model,
-- effort and approach of the attempt that ended each. Empty text stands for unknown.
CREATE TABLE IF NOT EXISTS metric_weeks (
 week TEXT NOT NULL,
 project TEXT NOT NULL,
 model TEXT NOT NULL,
 effort TEXT NOT NULL,
 approach TEXT NOT NULL,
 tasks INTEGER NOT NULL,
 done INTEGER NOT NULL,
 clean INTEGER NOT NULL,
 change_rounds INTEGER NOT NULL,
 reviewer_rounds INTEGER NOT NULL,
 interventions INTEGER NOT NULL,
 reopened INTEGER NOT NULL,
 relaunched INTEGER NOT NULL,
 work_ms INTEGER NOT NULL,
 waiting_user_ms INTEGER NOT NULL,
 queued_ms INTEGER NOT NULL,
 blocked_ms INTEGER NOT NULL,
 measured INTEGER NOT NULL,
 sessions INTEGER NOT NULL,
 steps INTEGER NOT NULL,
 subagent_steps INTEGER NOT NULL,
 looks INTEGER NOT NULL,
 polls INTEGER NOT NULL,
 merged_prs INTEGER NOT NULL,
 pr_lines INTEGER NOT NULL,
 lined_prs INTEGER NOT NULL,
 cost_micros INTEGER NOT NULL,
 costed INTEGER NOT NULL,
 merged_cost_micros INTEGER NOT NULL,
 lined_cost_micros INTEGER NOT NULL,
 input INTEGER NOT NULL,
 output INTEGER NOT NULL,
 cache_read INTEGER NOT NULL,
 cache_write INTEGER NOT NULL,
 PRIMARY KEY(week,project,model,effort,approach)
) WITHOUT ROWID;

-- What sessions spent by day and the model that spent it: the orchestrator's, those no task was
-- known for, and the tasks' own once their rows leave task_model_usage.
CREATE TABLE IF NOT EXISTS usage_days (
 day TEXT NOT NULL,
 provider TEXT NOT NULL,
 model TEXT NOT NULL,
 role TEXT NOT NULL CHECK(role IN('node','orchestrator','unattributed')),
 cost_micros INTEGER NOT NULL,
 costed INTEGER NOT NULL,
 input INTEGER NOT NULL,
 output INTEGER NOT NULL,
 cache_read INTEGER NOT NULL,
 cache_write INTEGER NOT NULL,
 sessions INTEGER NOT NULL,
 PRIMARY KEY(day,provider,model,role)
) WITHOUT ROWID;

-- When each version of the daemon first started, to compare before and after a release.
CREATE TABLE IF NOT EXISTS daemon_releases (
 version TEXT PRIMARY KEY CHECK(length(version)>0),
 first_seen_at INTEGER NOT NULL
) WITHOUT ROWID;

-- Each verdict a reviewer gave on the tasks it blocks, one row per round, with how many of its
-- findings were serious. The reviewer's verdict on the task is replaced each round; this is not.
CREATE TABLE IF NOT EXISTS review_findings (
 task_id TEXT NOT NULL REFERENCES tasks(id),
 reviewer_task_id TEXT NOT NULL REFERENCES tasks(id),
 at_revision INTEGER NOT NULL,
 kind TEXT NOT NULL CHECK(kind IN('approved','changes_requested')),
 serious_findings INTEGER NOT NULL CHECK(serious_findings>=0),
 PRIMARY KEY(task_id,reviewer_task_id,at_revision)
);
CREATE TRIGGER IF NOT EXISTS immutable_review_finding BEFORE UPDATE ON review_findings
 BEGIN SELECT RAISE(ABORT,'review findings are immutable'); END;
CREATE TRIGGER IF NOT EXISTS retain_review_finding BEFORE DELETE ON review_findings
 BEGIN SELECT RAISE(ABORT,'review findings are retained'); END;

CREATE TABLE IF NOT EXISTS codex_usage_baselines (
 task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
 account TEXT NOT NULL,
 thread_id TEXT NOT NULL,
 turn_id TEXT NOT NULL,
 model TEXT,
 input INTEGER,
 output INTEGER,
 cache_read INTEGER,
 cache_write INTEGER,
 last_at INTEGER NOT NULL,
 PRIMARY KEY(account,thread_id)
) WITHOUT ROWID;
