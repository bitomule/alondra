-- Created on every open, like task_models, so a store made before model metrics gains them
-- without being moved aside.

-- Every model the orchestrator or the user's effort chose for a Task: each launch, relaunch and
-- each message that hands a Task to a node with a model or an approach.
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
 title TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS model_decisions_task ON model_decisions(task_id,at);
CREATE INDEX IF NOT EXISTS model_decisions_agent ON model_decisions(agent_id);
CREATE TRIGGER IF NOT EXISTS immutable_model_decision BEFORE UPDATE ON model_decisions
 BEGIN SELECT RAISE(ABORT,'model decisions are immutable'); END;
CREATE TRIGGER IF NOT EXISTS retain_model_decision BEFORE DELETE ON model_decisions
 BEGIN SELECT RAISE(ABORT,'model decisions are retained'); END;

-- What a node's session cost and how full its context got, as its mod last reported when it
-- ended or was closed. The live reports are lost when the daemon restarts; this row is not.
CREATE TABLE IF NOT EXISTS node_costs (
 session_id TEXT PRIMARY KEY CHECK(length(session_id)>0),
 task_id TEXT REFERENCES tasks(id),
 model TEXT,
 cost_usd REAL,
 context_peak_percent REAL,
 started_at INTEGER,
 ended_at INTEGER NOT NULL
);
