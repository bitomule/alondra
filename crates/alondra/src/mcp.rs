use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Write};
use std::time::Duration;

use anyhow::Result;
use serde_json::{Value, json};

use crate::client::Client;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Node,
    Orchestrator,
}

fn self_contained_card(user: &str) -> String {
    format!("Every card is decided without reading your chat: {user} sees only the card. Its content carries the whole thing being decided (the proposed text and the current one, the design, the command, the list, the numbers), never a summary or \"what I showed you\". Each option says what happens if it is chosen, and the recommended one why. Never refer to the chat: repeat what is needed, and ask yourself whether someone who sees only this card can decide. {user} cannot open a file from the app: write a plan into the card, never point at a file. List a document's Markdown path in files to show it; a path only mentioned is never read.")
}

fn instructions(role: Role, user: &str) -> String {
    let language = match crate::i18n::lang() {
        crate::i18n::Lang::Es => "Spanish",
        crate::i18n::Lang::En => "English",
    };
    let skill = match role {
        Role::Node => "alondra:alondra",
        Role::Orchestrator => "alondra-orchestrator:alondra-orchestrator",
    };
    let common = format!(
        "Alondra manages durable task state. The user is {user}; write user-facing content in {language}, and your replies in this terminal too: the apps show your last one beside your work. Load the role skill {skill} before your first Alondra call and follow it: use read_skill when the provider has no native Skill tool. Write self-contained decisions and results, never process jargon, steps Alondra takes itself, or internal ids. Every card is decided without reading your chat: {user} sees only the card, so it carries the whole thing being decided, and {user} cannot open a file from the app. A refused card was not sent and is never a permission: do not proceed as if granted; fix the field it names and send it again with the same interaction_id; when a mutation's reply is lost, inspect its durable outcome and current state before continuing; never blindly repeat it. Read-only calls may be retried."
    );
    match role {
        Role::Node => format!(
            "{common} Your terminal does not reach {user}: these tools do, and {user}'s answers are typed into this session. Work only on your assigned Task; never install, restart or replace the Alondra daemon or CLI: test with the e2e sandbox. Use ask only for a real decision whose options change what you do next, then end your turn; request_input, request_action and request_test for what only {user} has, does or tries. Never send the same card twice: replace your open one with replaces. Use request_plan_mode for an Alondra-managed planning turn, then end this turn. Produce the complete checklist plan in native plan mode; Alondra must receive and approve it before implementation. Native update_plan is progress, not approval. When you are done, deliver with present_result and is_final true; Alondra merges your one PR: never merge it yourself or ask {user} to authorize a merge. Stopped by the Mac's memory, disk or CPU: wait_for_machine. A missing MCP server: wait_for_mcp. Never wait out a usage limit yourself: end your turn."
        ),
        Role::Orchestrator => format!(
            "{common} Name a Request with title_request before its second Task, then create cohesive Tasks with create_task, each with its launch plan; Alondra starts them itself. A node works on one Task for its whole life and closes with it; a Task owns one PR. Answer a Request without a Task with close_request (headline and answer): Alondra writes the user's card; close one whose Tasks are over with close_request, its answer saying what came of it. A failure only {user} can resolve goes in raise_problem, never in an ask. Alondra merges PRs: never merge one yourself or ask {user} to authorize a merge."
        ),
    }
}


fn tools(role: Role, user: &str) -> Value {
    let mut list = match role {
        Role::Node => node_domain_tools(user),
        Role::Orchestrator => orchestrator_domain_tools(user),
    };
    if role == Role::Node {
        list.push(domain_tool("request_plan_mode","Request read-only native planning for your own task. Stop or join existing work first, call this, then end the current turn. Alondra applies the mode before the next turn; produce the full checklist plan there and wait for its approval. This does not approve a plan.",json!({"task_id":{"type":"string"}}),&["task_id"]));
    }
    list.push(domain_tool("read_skill", "Read the installed instructions for your Alondra role. Follow next_line when the skill has another page.", json!({"name":{"type":"string","enum":[if role == Role::Node { "alondra" } else { "alondra-orchestrator" }]},"start_line":{"type":"integer","minimum":1,"maximum":1000000}}), &["name"]));
    if role == Role::Node {
        list.push(domain_tool("task_context", "Read a page of your task's retained public conversation. When next is non-null, pass the last entry reference as after. Missing older content and omitted bytes are explicit; private reasoning is never returned.", json!({"task_id":{"type":"string"},"after":{"type":"object","properties":{"task":{"type":"string"},"sequence":{"type":"integer","minimum":1},"generation":{"type":"integer","minimum":1},"cursor":{"type":"integer","minimum":0}},"required":["task","sequence","generation","cursor"],"additionalProperties":false},"limit":{"type":"integer","minimum":1,"maximum":128}}), &["task_id"]));
    } else {
        list.push(domain_tool("batch_outcomes","Read durable domain and physical tool outcome references from an earlier batch. Set list_runs=true to discover every retained batch when prompt references are omitted; follow next with after, then read each run_id. Without list_runs, omitted run_id means this batch. Uncertain is never proof of success; read current state before continuing goals.",json!({"list_runs":{"type":"boolean","default":false},"run_id":{"type":"string","description":"Omit for this batch; omit when list_runs is true"},"after":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":32}}),&[]));
        list.push(domain_tool("read_file", "Read a bounded page of a UTF-8 file inside the configured projects root. Symbolic links and parent traversal are refused.", json!({"path":{"type":"string"},"start_line":{"type":"integer","minimum":1,"maximum":1000000},"max_lines":{"type":"integer","minimum":1,"maximum":400}}), &["path"]));
        list.push(domain_tool("glob_files", "Find up to 100 files under a directory in the projects root. Supports *, ** and ?. Symbolic links are never followed; limits and omissions are reported.", json!({"path":{"type":"string"},"pattern":{"type":"string"}}), &["pattern"]));
        list.push(domain_tool("search_files", "Search a bounded set of project file previews for a literal string. Reports line numbers and omitted previews. No regular expressions or shell programs.", json!({"path":{"type":"string"},"query":{"type":"string"}}), &["query"]));
    }
    let (whose, task_help) = match role {
        Role::Node => ("your Task", "Your Task's id"),
        Role::Orchestrator => ("you", "Only the cards of this Task; leave it out for all of yours"),
    };
    list.push(json!({"name": "open_cards", "description": format!("The cards {whose} still has open in front of {user} or the orchestrator: id, kind, need, the slot it holds, whether it blocks and whether a Claude Code hook holds it. Read them before sending a card about something you already asked: send the new one with replaces set to the old card's id, or withdraw it. A new plan, final result or problem with the same cause replaces the open one in its slot by itself."),
        "inputSchema": {"type": "object", "properties": {"task_id": {"type": "string", "description": task_help}}, "required": if role == Role::Node { json!(["task_id"]) } else { json!([]) }, "additionalProperties": false}}));
    if role == Role::Node {
        list.push(json!({"name": "wait_for_machine", "description": "Declare that you are stopped waiting for the Mac's resources (memory, a full disk, the CPU), also when something you ran died for lack of memory. Not for your turn in a build queue such as the build queue: waiting there is normal; build_queue is for a queue that is stuck. Never for the subscription's usage (weekly or 5-hour limits, tokens): there is no resource for it, because Alondra paces usage itself, so keep working. When a turn ends on a usage limit, never wait for it with sleep, cron or a background loop either: end your turn, and Alondra wakes you when Claude answers again. End your turn after it: Alondra tells you to carry on when the Mac has room for you.", "inputSchema": {"type": "object", "properties": {"resource": {"type": "string", "enum": ["memory", "disk", "cpu", "build_queue"], "description": "The resource of the Mac you wait for."}, "reason": {"type": "string", "description": "What you wait for, in a few words in the user's language; the user reads it on your row."}}, "required": ["resource", "reason"]}}));
    } else {
        list.push(
        json!({"name": "schedule_create", "description": format!("Create a durable scheduled Alondra task for {user}. Use this instead of CronCreate for work that should continue beyond this Claude session. Times use the Mac's local timezone. Ask only if the mission, project or time cannot be inferred from the conversation."), "inputSchema": {"type": "object", "properties": {
            "title": {"type": "string"}, "text": {"type": "string", "description": "Self-contained mission for a fresh node"},
            "project": {"type": ["string", "null"], "description": "Folder name directly under ~/Projects, or null for the Projects folder"},
            "frequency": {"type": "string", "enum": ["daily", "weekly", "every_hours", "every_days", "every_months", "once"]},
            "hour": {"type": "integer", "minimum": 0, "maximum": 23}, "minute": {"type": "integer", "minimum": 0, "maximum": 59},
            "weekday": {"type": ["integer", "null"], "description": "For weekly: Sunday 0, Monday 1, through Saturday 6"},
            "interval": {"type": ["integer", "null"], "description": "For every_hours/days/months: interval from 1 to 365"},
            "start_at": {"type": ["integer", "null"], "description": "Unix milliseconds for once or interval anchor"},
            "options": {"type": "object", "additionalProperties": {"type": "string"}, "description": "Alondra launch choices, e.g. pr: review or auto_merge, effort: normal"},
            "disallowed_tools": {"type": "array", "items": {"type": "string"}, "description": "Optional Claude tool deny patterns"}
        }, "required": ["title", "text", "project", "frequency", "hour", "minute"]}}));
    }
    if role == Role::Orchestrator {
        list.extend([
            json!({"name": "tasks_in_progress", "description": format!("Work in progress: unfinished nodes that are running, dormant or owe {user} something, with what they run in the background (subagents and shells, each since when it runs); open questions, unread reports, undelivered messages for you, and the machine."), "inputSchema": {"type": "object", "properties": {"all": {"type": "boolean", "description": "Add finished and gone nodes"}}}}),
            json!({"name": "node", "description": "A node's state, tool in progress, last message, open questions and actions, unread reports, pending messages and recent events.", "inputSchema": {"type": "object", "properties": {"node": {"type": "string", "description": "Name, short id or session_id"}, "history": {"type": "boolean", "description": "Add every question, report and message, and 100 events"}, "before": {"type": "integer", "description": "The 100 events before this seq"}}, "required": ["node"]}}),
            json!({"name": "machine", "description": "The models you choose from (Available models: each model of an enabled account with its efforts, the accounts' margin, its sheet and Alondra's measured results), the public evidence of each sheet, the account catalogs and profiles; machine capacity (memory, load, CPU, disk) and the subscription's usage: the week, today against its slice, and whether today is over pace.", "inputSchema": {"type": "object", "properties": {}}}),
            json!({"name": "daemon_log", "description": "The last lines of Alondra's daemon log (alondrad.log), to find out why something in Alondra failed.", "inputSchema": {"type": "object", "properties": {"lines": {"type": "integer", "description": "How many lines from the end: 200 by default, at most 1000"}}}}),
            json!({"name": "get_launch_plan", "description": "Read a task’s complete stored launch plan, including its mission, before reviewing or changing it. Does not modify the task.", "inputSchema": {"type": "object", "additionalProperties": false, "properties": {"task_id": {"type": "string"}}, "required": ["task_id"]}}),
            json!({"name": "update_launch_plan", "description": "Change only the supplied model, effort, approach, tags or reason of a task that has never started; preserve the mission, name, folder, role, approved checklist and added context. For a model-only change pass task_id and changes: {model: \"codex/gpt-…\"}. Read get_launch_plan first. Omitted fields stay unchanged; null, empty changes and unknown fields are refused. tags replaces only the tags. A pending plan review must be settled with settle_plan_review; user-fixed effort remains binding.", "inputSchema": {"type": "object", "additionalProperties": false, "properties": {"task_id": {"type": "string"}, "changes": launch_plan_changes_schema()}, "required": ["task_id", "changes"]}}),
            json!({"name": "set_launch_plan", "description": "Replace the entire launch plan of a task that has never started. Use update_launch_plan to change only its model or other selection fields without losing its mission. A pending plan review must be settled with settle_plan_review; user-fixed effort remains binding.", "inputSchema": {"type": "object", "properties": {"task_id": {"type": "string"}, "launch": launch_plan_schema()}, "required": ["task_id", "launch"]}}),
            json!({"name": "send", "description": "Send a message to a node; Alondra types it into the node's session (waking it if it is dormant). With task, the message is about that task, which must be the node's own: a node works only on the task it was launched for, and a closed task's node is closed with it.", "inputSchema": {"type": "object", "properties": {"node": {"type": "string"}, "text": {"type": "string"}, "task": {"type": "string", "description": "The id of the task (t-…) the message is about"}, "approach": {"type": "string", "enum": ["solo", "subagents", "workflow"], "default": "solo", "description": "How the node should work: solo (alone), subagents (hands independent parts to subagents and reviews them) or workflow (a parallel, checkable run). Alondra writes the instructions into the mission"}, "reason": {"type": "string", "description": "With task and an approach: one short line on what the task is and why this approach. Kept to measure how each approach does"}}, "required": ["node", "text"]}}),
            json!({"name": "interrupt", "description": "Press Esc in a node's session to stop the call it is running now (the node's `tool`, running since its `since`); `send` then moves it on.", "inputSchema": {"type": "object", "properties": {"node": {"type": "string", "description": "Name, short id or session_id"}}, "required": ["node"]}}),
            json!({"name": "launch", "description": format!("Launch a new node with the Alondra plugin. With task, the node is launched for that task (one whose start failed, or whose node is gone) and gets its brief unless you pass a prompt. Over today's slice of the weekly subscription usage (see `machine`) the launch goes ahead and Alondra asks {user} itself whether to pause until tomorrow: do not relay it. While the selected account has no available usage, launches wait for an eligible account or its reset. name and cwd are optional when the task has a launch plan."), "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}, "cwd": {"type": "string"}, "prompt": {"type": "string", "description": "The node's mission; optional with task"}, "task": {"type": "string", "description": "The id of the task (t-…) the node is launched for"}, "replace": {"type": "boolean", "description": "With task, recover a disconnected legacy node only on its original account. Existing SDK nodes cannot be replaced through this tool: use the task options for supported same-session model changes. Account and provider are selected only when creating a node and stay fixed. Unsupported changes require a new task and node."}, "title": {"type": "string", "description": format!("A short name for the task {user} reads (a few words); without it the task goes by the node's name")}, "provider": {"type":"string","enum":["claude","codex"]}, "profile": {"type":"string","enum":["implementation","general","design","review"]}, "reasoning_effort":{"type":"string","description":"Native effort advertised by the exact selected model; distinct from inherited work effort"}, "model": {"type": "string", "description": format!("Exact advertised model named with its provider (claude/opus, codex/model-id); a name without provider is refused when both providers offer it. Automatic uses configured profiles; {user}'s explicit choice remains binding")}, "request_id": {"type": "string", "description": format!("Without task, the request_id of {user}'s request this launch serves: Alondra adds the options {user} chose to the node's mission")}, "effort": {"type": "string", "description": "Legacy alias for native reasoning_effort; use only an effort advertised for the selected model"}, "approach": {"type": "string", "enum": ["solo", "subagents", "workflow"], "default": "solo", "description": "How the node should work: solo (alone), subagents (hands independent parts to subagents and reviews them) or workflow (a parallel, checkable run). Alondra writes the instructions into the mission"}, "reason": {"type": "string", "description": format!("One short line on what the task is and why this model (and effort) fits it. Required when the request's effort is auto; optional when {user} chose the effort. Kept to measure how each model does")}}, "required": ["task"]}}),
        ]);
    }
    for tool in &mut list {
        if tool["name"] == "launch" {
            tool["description"] = json!(format!(
                "Launch a new node with the Alondra plugin for a Task no connected node works on, or with replace: true for supported legacy recovery on its original account (existing SDK nodes refuse replacement; use the task options for supported same-session model controls, with account and provider fixed): it gets the Task's mission (its title and its Request) unless you pass a prompt, the options {user} chose for the Request, its domain capability, and the Task. Over today's slice of the weekly subscription usage (see `machine`) the launch goes ahead and Alondra asks {user} itself whether to pause until tomorrow: do not relay it. While the selected account has no available usage, launches wait for an eligible account or its reset."
            ));
            let properties = tool["inputSchema"]["properties"]
                .as_object_mut()
                .expect("launch properties");
            properties.remove("request_id");
            properties.insert("tags".into(), tags_schema());
            tool["inputSchema"]["required"] = json!(["task"]);
        }
    }
    Value::Array(list)
}

fn domain_tool(name: &str, description: &str, fields: Value, required: &[&str]) -> Value {
    json!({"name":name,"description":description,"inputSchema":{"type":"object","properties":fields,"required":required,"additionalProperties":false}})
}

fn interaction_fields() -> Value {
    json!({
        "interaction_id":{"type":"string"},
        "scope":{"type":"object","oneOf":[
            {"type":"object","properties":{"kind":{"type":"string","const":"task"},"task_id":{"type":"string"}},"required":["kind","task_id"],"additionalProperties":false},
            {"type":"object","properties":{"kind":{"type":"string","const":"orchestrator"},"request_id":{"type":["string","null"]}},"required":["kind","request_id"],"additionalProperties":false}
        ]},
        "recipient":{"type":"object","oneOf":[
            {"type":"object","properties":{"kind":{"type":"string","const":"user"}},"required":["kind"],"additionalProperties":false},
            {"type":"object","properties":{"kind":{"type":"string","const":"orchestrator"}},"required":["kind"],"additionalProperties":false},
            {"type":"object","properties":{"kind":{"type":"string","const":"agent"},"agent_id":{"type":"string"}},"required":["kind","agent_id"],"additionalProperties":false}
        ]},
        "body":{"type":"string"},"blocking":{"type":"boolean"},
        "options":{"type":"array","description":"Each option is a button: pressing it answers. The user can always write on the card too, alone or with the option; to ask for a value, use request_input.","items":{"type":"object","properties":{
            "id":{"type":"string"},"label":{"type":"string"},"description":{"type":"string"}
        },"required":["id","label"],"additionalProperties":false}},
        "attachments":{"type":"array","items":{"type":"object","properties":{"id":{"type":"string"},"name":{"type":"string"},"content_digest":{"type":"string","pattern":"^[0-9a-f]{64}$"},"content_reference":{"type":"string"},"bytes":{"type":"integer","minimum":0}},"required":["id","name","content_digest","content_reference","bytes"],"additionalProperties":false}}
    })
}

fn input_value_schema() -> Value {
    json!({"type":"object","additionalProperties":{"type":"object","oneOf":[
        {"type":"object","properties":{"kind":{"type":"string","const":"text"},"value":{"type":"string"}},"required":["kind","value"],"additionalProperties":false},
        {"type":"object","properties":{"kind":{"type":"string","const":"boolean"},"value":{"type":"boolean"}},"required":["kind","value"],"additionalProperties":false},
        {"type":"object","properties":{"kind":{"type":"string","const":"integer"},"value":{"type":"integer"}},"required":["kind","value"],"additionalProperties":false}
    ]}})
}

fn questions_schema(options: &Value) -> Value {
    json!({"type":"array","minItems":1,"maxItems":4,"description":"Instead of options: 2 to 4 questions asked together, each with its own options. Each becomes its own card for the user; you get every answer at once when the last is in. A question is a full sentence that makes sense on its own.","items":{"type":"object","properties":{
        "question":{"type":"string","maxLength":90,"description":"The question, one line, clear without any internal context"},
        "options":options,
        "recommended":{"type":"string","description":"The id of the option of this question you recommend"},
        "why":{"type":"string","maxLength":240,"description":"What the user needs to know to answer this question; never more questions or options"}
    },"required":["question","options"],"additionalProperties":false}})
}

fn files_schema() -> Value {
    json!({"type":"array","items":{"type":"string"},"description":"Markdown files of your worktree the user reads beside the card, each a path relative to your worktree ending in .md (like docs/plan.md). Alondra reads each one and shows it as a document of the card; a listed file that cannot be read refuses the card. Only these files are shown: a path you mention in need, why, content or plan is never read"})
}

fn content_schema() -> Value {
    json!({"type":"object","description":"The long part of the card, shown only in its detail: the document, diff, command, report, steps, options, diagnosis or attachments. It holds the whole thing being decided, never a summary of it. Put anything longer than a sentence here, never in why. A separate document (a plan, a design, a spec) is never pasted here: list its Markdown path in files, which the card shows as a document of its own, or send it as an Alondra document. Use steps for an ordered list and report for prose; the plan kind belongs to a plan Claude Code holds for approval, never to a card you send","properties":{
        "kind":{"type":"string","enum":["document","diff","command","report","steps","options","diagnosis","attachments"]},
        "summary":{"type":"string","maxLength":60,"description":"Optional one-line label such as \"6 files · +120 −40\"; Alondra computes one when you leave it out"},
        "body":{"type":"string"}
    },"required":["kind","body"],"additionalProperties":false})
}

fn blocks_schema() -> Value {
    json!({"type":"array","maxItems":16,"description":"What this card holds back until it is answered: an open PR, document or delivery of the Task","items":{"type":"object","oneOf":[
        {"type":"object","properties":{"kind":{"type":"string","const":"pull_request"},"pr_id":{"type":"string"}},"required":["kind","pr_id"],"additionalProperties":false},
        {"type":"object","properties":{"kind":{"type":"string","const":"document"},"document_id":{"type":"string"}},"required":["kind","document_id"],"additionalProperties":false},
        {"type":"object","properties":{"kind":{"type":"string","const":"delivery"},"interaction_id":{"type":"string"}},"required":["kind","interaction_id"],"additionalProperties":false}
    ]}})
}

fn checklist_schema() -> Value {
    json!({"type":"array","maxItems":alondra_core::domain::CHECKLIST_MAX_ITEMS,"description":"Only in the final result of a Task whose plan the user approved: every point of that plan's checklist (`- [ ] …` lines, numbered from 1 in order), marked done with what proves it. A point that no longer applies goes out with a revised plan the user approves, never by leaving it out here","items":{"type":"object","properties":{
        "item":{"type":"string","description":"The number of the point in the approved plan, from 1"},
        "evidence":{"type":"array","minItems":1,"maxItems":alondra_core::domain::EVIDENCE_MAX_PER_ITEM,"items":{"type":"object","properties":{
            "kind":{"type":"string","enum":alondra_core::domain::EvidenceKind::ALL.map(alondra_core::domain::EvidenceKind::as_str)},
            "value":{"type":"string","minLength":1,"maxLength":alondra_core::domain::EVIDENCE_MAX_TEXT,"description":"commit: its sha, in your branch's history; test or command: what you ran; screenshot: the id of one of this result's attachments; pr: your PR's number; file: its path; link: the URL"},
            "detail":{"type":"string","maxLength":alondra_core::domain::EVIDENCE_MAX_TEXT,"description":"What it showed, such as \"713 passed\""}
        },"required":["kind","value"],"additionalProperties":false}}
    },"required":["item","evidence"],"additionalProperties":false}})
}

fn need_schema() -> Value {
    json!({"type":"string","minLength":1,"maxLength":90,"description":"What you need from the reader, in the imperative and on one line (at most 90 characters). Clear on its own: the work or the decision itself, never process steps Alondra takes (merging, authorizing, retrying) or internal ids"})
}

fn why_schema() -> Value {
    json!({"type":"string","maxLength":240,"description":"Why, and whom it holds back, in one or two sentences (at most 240 characters). Name tasks and PRs by their title or number, never by an internal id such as t-foo. Long text goes in content, never here"})
}

fn replaces_schema() -> Value {
    json!({"type":"string","description":"The id of your own open card this one replaces: it is withdrawn in the same step"})
}

fn place<'a>(interaction: &Value, extra: Value, required: &[&'a str]) -> (Value, Vec<&'a str>) {
    let mut fields = serde_json::Map::new();
    for key in ["interaction_id", "scope", "recipient"] {
        fields.insert(key.into(), interaction[key].clone());
    }
    fields.extend(extra.as_object().expect("card fields").clone());
    let mut needed = vec!["interaction_id", "scope", "recipient"];
    needed.extend_from_slice(required);
    (Value::Object(fields), needed)
}

/// The tools that put a card in front of someone, one per kind of card.
fn card_tools(user: &str, interaction: &Value, node: bool) -> Vec<Value> {
    let attachments = interaction["attachments"].clone();
    let blocking = json!({"type":"boolean","description":"true when you cannot go on until the answer arrives"});
    let rule = self_contained_card(user);
    let mut tools = vec![];
    let mut add = |name: &str, description: String, extra: Value, required: &[&str]| {
        let (fields, needed) = place(interaction, extra, required);
        tools.push(domain_tool(name, &description, fields, &needed));
    };
    add(
        "ask",
        format!("Put a decision in front of {user} (or the orchestrator): a Decide card with 2 to 8 options. Only for a real decision, whose options change what you do next: never to say your work is ready, to show a report or plan to read, or to point at a file; finished work goes out with present_result, and the task stays open while you only ask. Prefer a recommendation: set recommended to the option you would pick; never write \"(Recommended)\" in a label. Explain each choice in its description. To ask several related things at once send `questions` (2 to 4, each {{question, options, recommended, why}}) instead of options: each becomes its own card, {user} answers them one by one, and all the answers reach you together once the last is answered. Every question must stand on its own and be clear without any context from your work; why only says what {user} needs to know to answer it, never another question or option. To collect text or a file use request_input, to hand over a task use request_action. Long context goes in content, never in why. A node holds one blocking ask at a time: replace it with replaces. Never ask whether to stop or wait for the subscription's usage: Alondra paces usage and {user} decides on its usage card; keep working. {rule}"),
        json!({"need":need_schema(),"why":why_schema(),"options":interaction["options"],"recommended":{"type":"string","description":"The id of the option you recommend"},
            "questions":questions_schema(&interaction["options"]),"blocking":blocking,"blocks":blocks_schema(),"replaces":replaces_schema(),"content":content_schema(),"files":files_schema(),"attachments":attachments}),
        &["why", "blocking"],
    );
    add(
        "request_action",
        format!("Hand {user} something to do outside Alondra (a Do card). Say how you will check it was done in verify; when {user} answers Done you run that check. Put the instructions in steps or content, not in why. {rule}"),
        json!({"need":need_schema(),"why":why_schema(),"verify":{"type":"string","description":"How you will check it was done"},"steps":{"type":"array","items":{"type":"string"},"description":"The steps to follow, in order"},"options":interaction["options"],"blocking":blocking,"blocks":blocks_schema(),"replaces":replaces_schema(),"content":content_schema(),"files":files_schema(),"attachments":attachments}),
        &["need", "why", "verify", "blocking"],
    );
    add(
        "request_input",
        format!("Ask {user} for something only they can provide (a Provide card): a file, a piece of text or a secret. {user} writes it or attaches it on the card, and it reaches you as their answer; the card's only button is «I can't provide it». Use ask instead when the answer is one of a few choices."),
        json!({"need":need_schema(),"why":why_schema(),"kind":{"type":"string","enum":["file","text","secret"],"description":"What you need: a file, text, or a secret value"},"destination":{"type":"object","properties":{"file":{"type":"string","description":"A git-ignored file, relative to the repository root, like .env"},"variable":{"type":"string","description":"The variable name, like STRIPE_KEY"}},"required":["file","variable"],"description":"Required for kind secret: Alondra writes VARIABLE=value to this file in your worktree and the main checkout, and you only get «VARIABLE written to file»"},"blocking":blocking,"blocks":blocks_schema(),"replaces":replaces_schema(),"content":content_schema(),"files":files_schema(),"attachments":attachments}),
        &["need", "why", "kind", "blocking"],
    );
    if node {
        add(
            "request_test",
            format!("Ask {user} to try something out and say whether it works (a Test card): the answers are fixed, Works or Doesn't work, with any text {user} writes on the card. Say where to look in where (build, version, screen) and what to look at in what."),
            json!({"need":need_schema(),"where":{"type":"string","description":"Where to test it: build, version, screen"},"what":{"type":"string","description":"What to look at"},"blocking":blocking,"blocks":blocks_schema(),"replaces":replaces_schema(),"files":files_schema(),"attachments":attachments}),
            &["need", "where", "what"],
        );
    }
    if !node {
        add(
        "publish_information",
        format!("Tell {user} (or the orchestrator) something they only need to read (a Know card): need is the headline, body the update. It never holds anyone back. Never about a Request {user} made or one of its Tasks: Alondra tells {user} how each of those ended from close_request, and refuses such a card. Never to confirm something routine (launching, reassigning, cancelling, adding to a design): Tasks already shows it; a maintenance message (a Request sent with alondra send without --reply) gets no card, and one about it is refused; never status chatter, never a question or a request: anything that needs {user} to answer or do something is an ask, request_input or request_action, never a note that you are waiting. To correct an earlier notice of yours that is still open, send the correction with replaces set to its id: it takes the old card's place instead of adding a card. Sending the same unread notice twice returns the first one. Send a notice only when knowing it helps {user}, and say its effect for them in plain words (\"You can update Alondra to 0.2.56: it brings X and Y\"), never script names, task ids or internal steps (\"release.sh + tag finished: release 0.2.56 published\" is wrong). An answer to a message from Claude or a maintenance message never becomes a notice for {user}. {rule}"),
        json!({"need":need_schema(),"why":why_schema(),"body":interaction["body"],"replaces":replaces_schema(),"content":content_schema(),"files":files_schema(),"attachments":attachments}),
        &["need", "body"],
        );
    }
    if node {
        let target = json!({"type":"array","items":{"type":"object","oneOf":[
            {"type":"object","properties":{"kind":{"type":"string","const":"text"},"text":{"type":"string"}},"required":["kind","text"],"additionalProperties":false},
            {"type":"object","properties":{"kind":{"type":"string","const":"document_revision"},"document_id":{"type":"string"},"revision_id":{"type":"string"}},"required":["kind","document_id","revision_id"],"additionalProperties":false},
            {"type":"object","description":"The PR your task registered","properties":{"kind":{"type":"string","const":"pull_request"},"head":{"type":"string","description":"The full 40-character commit sha the PR is at (git rev-parse HEAD after git push), never a branch name. Leave it out and Alondra presents the commit the PR is at; a commit you just pushed is checked with GitHub"}},"required":["kind"],"additionalProperties":false}
        ]}});
        tools.push(domain_tool(
            "present_result",
            &format!("Present a result to {user}. With is_final true it is a Review card: {user} accepts it, and for a PR Alondra then merges it and tells you (with auto-merge, once its checks pass). With is_final false it is a progress notice, which never carries an open PR: an open PR goes only in a final result. Finishing is always present_result, never an ask. need says what to review and content carries the report written out in its body, never a path or a pasted document: {user} cannot open files from the app; files go in attachments (a final result needs content). Both describe the change itself, never the merge or an approval to give. A final result is refused while your worktree holds uncommitted or unpushed work, or work outside the default branch and your registered PR. Final results are refused while one of your cards still blocks the Task. need and content say what the result means for {user} in plain words, never script names, task ids or internal steps. When the result needs {user} to choose anything, ask it with ask before delivering; never leave choices only in the content, because accepting the result does not answer them. When {user} approved a plan for the Task, a final result marks every point done in checklist, each with its evidence. {rule}"),
            json!({"task_id":{"type":"string"},"interaction_id":interaction["interaction_id"],"need":need_schema(),"why":why_schema(),"content":content_schema(),"targets":target,"is_final":{"type":"boolean"},"nothing_to_review":{"type":"boolean","description":"With is_final true and only text targets: the work found nothing for the user to look at (a scheduled check with nothing open, nothing changed). The task closes in silence, without a card"},"checklist":checklist_schema(),"plan":{"type":"string","description":"Only on a final result without a pull request whose research concludes that something should be built: that plan, in Markdown, with one checklist line (- [ ] ...) per thing the implementation will deliver, and the report in content. The result is then a plan card: the user reads the plan, opens the report as a document beside it, and presses Approve, which creates the tasks that build exactly this plan, or Request changes; its need asks to approve the plan, never to read the report or decide whether to build. Ask every decision the plan leaves open with ask before, never inside the plan. A research that proposes nothing to build leaves plan out: its report alone is marked Seen and creates nothing. Never put a plan only inside the report; a plan the user asked you for in your own task goes through ExitPlanMode instead"},"body":interaction["body"],"files":files_schema(),"attachments":attachments}),
            &["task_id", "interaction_id", "need", "targets", "is_final"],
        ));
    }
    let withdraw_fields = json!({"interaction_id":{"type":"string"}});
    tools.push(domain_tool("withdraw", "Withdraw one of your own cards that no longer needs an answer. An answered card cannot be withdrawn.", withdraw_fields.clone(), &["interaction_id"]));
    tools.push(domain_tool("withdraw_question", "Alias of withdraw.", withdraw_fields, &["interaction_id"]));
    let mut clarification_fields = json!({"id":{"type":"string","description":"The card the user asked you to clarify"},
        "replacement":{"type":"object","properties":{"tool":{"type":"string","enum":["ask","request_action","request_input","request_test","present_result"]},"arguments":{"type":"object","additionalProperties":true,"description":"The arguments of that tool, as you would call it"}},"required":["tool","arguments"],"additionalProperties":false}});
    let clarification_help = if node {
        format!("Settle a card after {user} used Ask for clarification on it. Until you do, your next card must replace it. Send replacement, the corrected card as {{\"tool\": \"ask\", \"arguments\": {{...}}}} (any card tool; replaces is set for you), and put the explanation {user} asked for in its why or content. Or withdraw the card.")
    } else {
        clarification_fields["text"] = json!({"type":"string"});
        clarification_fields["interaction_id"] = json!({"type":"string","description":"With text: the id of the notice"});
        clarification_fields["scope"] = interaction["scope"].clone();
        clarification_fields["recipient"] = interaction["recipient"].clone();
        format!("Settle a card after {user} used Ask for clarification on it. Until you do, your next card must replace it. Send replacement, the corrected card as {{\"tool\": \"ask\", \"arguments\": {{...}}}} (any card tool; replaces is set for you), or text to answer in words: then interaction_id, scope and recipient say where the notice goes, and the old card is withdrawn.")
    };
    tools.push(domain_tool("answer_clarification", &clarification_help, clarification_fields, &["id"]));
    if !node {
        tools.push(domain_tool(
            "raise_problem",
            &format!("Put a failure in front of {user} with its ways out (a Problem card): a launch that failed, a batch you could not handle. State what failed in need, why it matters in why and the diagnosis in content, and give the options {user} can choose from."),
            json!({"interaction_id":interaction["interaction_id"],"scope":interaction["scope"],"need":need_schema(),"why":why_schema(),"options":interaction["options"],"content":content_schema(),"attachments":interaction["attachments"],
                "cause":{"type":"string","maxLength":120,"description":"A short key for what failed, the same every time it fails (for example launch:t-12): a newer problem with the same cause in the same place replaces the open one"}}),
            &["interaction_id", "scope", "need", "why", "options"],
        ));
    }
    if node {
        let scope = interaction["scope"].clone();
        tools.push(domain_tool(
            "suggest",
            &format!("Tell the orchestrator about something you saw outside your task (an unrelated bug, an improvement) as soon as you see it, and keep working: it never blocks you, never goes to {user} and never touches your delivery. The orchestrator answers with one decision: it is part of your plan (do it in this task), discarded (with the reason), or a new task it created (with its id). At most {} suggestions wait at a time per task.", alondra_core::domain::OPEN_SUGGESTIONS_MAX),
            json!({"interaction_id":{"type":"string"},"scope":scope,
                "what":{"type":"string","maxLength":90,"description":"What you saw, on one line"},
                "where":{"type":"string","description":"Where it is: file and line, screen, PR, command"},
                "why_not_mine":{"type":"string","maxLength":240,"description":"Why it is not part of your task"},
                "detail":{"type":"string","description":"Anything else the orchestrator needs to decide: what you measured, how to reproduce it"}}),
            &["interaction_id", "scope", "what", "where", "why_not_mine"],
        ));
    }
    tools
}

fn node_domain_tools(user: &str) -> Vec<Value> {
    let mut interaction = interaction_fields();
    interaction["scope"] = json!({"type":"object","properties":{"kind":{"type":"string","const":"task"},"task_id":{"type":"string"}},"required":["kind","task_id"],"additionalProperties":false});
    let mut tools = card_tools(user, &interaction, true);
    let attachment = json!({"type":"object","properties":{"id":{"type":"string"},"name":{"type":"string"},"content_digest":{"type":"string","pattern":"^[0-9a-f]{64}$"},"content_reference":{"type":"string"},"bytes":{"type":"integer","minimum":0}},"required":["id","name","content_digest","content_reference","bytes"],"additionalProperties":false});
    let simple = [
        (
            "start_task",
            "start_task",
            "Start the Task the orchestrator assigned you after your previous one ended, once its dependencies allow.",
            json!({"task_id":{"type":"string"}}),
            vec!["task_id"],
        ),
        (
            "cleanup_done",
            "cleanup_done",
            "Say that you deleted what you generated for your task, after Alondra told you the task is closing. Alondra then closes this session and every worktree of the task, yours and those of the subagents you launched, once none has uncommitted changes or unpushed commits. In discard, list the paths of the task's worktrees whose remaining work you give up: Alondra deletes them as they are. Call it again after fixing what Alondra said stopped it from closing the session.",
            json!({"task_id":{"type":"string"},"discard":{"type":"array","items":{"type":"string"}}}),
            vec!["task_id"],
        ),
        (
            "post_merge_work",
            "post_merge_work",
            "Say that you are still working on something for your task after its PR merged (a deploy, a Homebrew publish, a release script), with its reason. Alondra does not close this session while you say so, even if the task is closing and you look idle. Call it again without reason as soon as that work ends, or call cleanup_done: Alondra's wait to close the session then starts over. Work your task still has to do after the merge (publish, install, deploy, verify) is declared here before you present the PR as final: the task then stays in progress after the merge until you end it. When that work, after your PR merged, has to wait for other work to land (another task's fix on main), name it in waits_for and end your turn: Alondra wakes you with its result and merged PR when it lands. A wait written only in reason has nothing that wakes you.",
            json!({"task_id":{"type":"string"},"reason":{"type":"string","description":"What you are still doing, in one short line, e.g. \"release.sh publishes 0.2.54\", or what you will do once what you wait for lands. Leave it out when the work has ended."},"waits_for":{"type":"array","items":{"type":"string"},"description":"Only after your PR merged: the work yours waits for, each an Alondra task id (t-…) or a pull request an Alondra task registered (owner/repo#123 or its URL). Needs reason. A pull request no Alondra task registered is refused: ask the orchestrator for a task that delivers it."}}),
            vec!["task_id"],
        ),
        (
            "wait_for_mcp",
            "wait_for_mcp",
            "Say that your task needs an MCP server that is not connected (it needs a sign-in, failed or is off), then end your turn. The user sees one card to sign in to it, however many tasks wait for it; never call its mcp__<server>__authenticate tool yourself. Alondra tells you when it is connected, or that you go on without it.",
            json!({"task_id":{"type":"string"},"server":{"type":"string","description":"The server's name as Claude Code lists it, e.g. linear for the tools mcp__linear__*"}}),
            vec!["task_id", "server"],
        ),
        (
            "answer_question",
            "answer_question",
            "Answer a question addressed to this node: option_id is the option chosen (empty for an answer in your own words), and inputs.text your words, alone or with it.",
            json!({"interaction_id":{"type":"string"},"option_id":{"type":"string"},"inputs":input_value_schema(),"attachments":{"type":"array","items":attachment}}),
            vec!["interaction_id", "option_id"],
        ),
        (
            "create_document",
            "create_document",
            "Create an immutable document revision attached to your Task.",
            json!({"task_id":{"type":"string"},"document_id":{"type":"string"},"title":{"type":"string"},"revision_id":{"type":"string"},"content_digest":{"type":"string","pattern":"^[0-9a-f]{64}$"},"content_reference":{"type":"string"},"role":{"type":"string"}}),
            vec![
                "task_id",
                "document_id",
                "title",
                "revision_id",
                "content_digest",
                "content_reference",
                "role",
            ],
        ),
        (
            "revise_document",
            "revise_document",
            "Publish a new immutable revision of a linked document.",
            json!({"task_id":{"type":"string"},"document_id":{"type":"string"},"revision_id":{"type":"string"},"content_digest":{"type":"string","pattern":"^[0-9a-f]{64}$"},"content_reference":{"type":"string"}}),
            vec![
                "task_id",
                "document_id",
                "revision_id",
                "content_digest",
                "content_reference",
            ],
        ),
        (
            "register_pr",
            "register_pr",
            "Associate exactly one canonical PR with your Task; a hook can only observe GitHub.",
            json!({"task_id":{"type":"string"},"host":{"type":"string"},"repository":{"type":"string"},"number":{"type":"integer","minimum":1},"head":{"type":"string","description":"The full 40-character commit sha you pushed (git rev-parse HEAD after git push), never a branch name"},"base":{"type":"string"},"allowed_methods":{"type":"array","items":{"type":"string","enum":["merge","squash","rebase"]},"minItems":1}}),
            vec![
                "task_id",
                "host",
                "repository",
                "number",
                "head",
                "base",
                "allowed_methods",
            ],
        ),
        (
            "link_interaction_subject",
            "link_interaction_subject",
            "Link your Task question to its PR or document review subject; a question linked to a document revision opens that document for review.",
            json!({"interaction_id":{"type":"string"},"subject":{"type":"object","oneOf":[{"type":"object","properties":{"kind":{"type":"string","const":"pull_request"},"pr_id":{"type":"string"}},"required":["kind","pr_id"],"additionalProperties":false},{"type":"object","properties":{"kind":{"type":"string","const":"document_review"},"document_id":{"type":"string"},"revision_id":{"type":"string"}},"required":["kind","document_id","revision_id"],"additionalProperties":false}]}}),
            vec!["interaction_id", "subject"],
        ),
        (
            "publish_stack_readiness",
            "publish_stack_readiness",
            "Report whether your presented PR is ready for its dependent stack.",
            json!({"task_id":{"type":"string"},"presentation_id":{"type":"string"},"pr_id":{"type":"string"},"head":{"type":"string"},"ready":{"type":"boolean"}}),
            vec!["task_id", "presentation_id", "pr_id", "head", "ready"],
        ),
        (
            "record_verdict",
            "record_verdict",
            "Say what your final delivery concluded about the Tasks you block (a review): approved, or changes_requested with the findings as text. In both, serious_findings is how many of the failures you found are serious: they break behaviour, lose data, or open a security or session hole (0 if none); it is required with changes_requested. Call it right after present_result with is_final true. Changes_requested sends those Tasks back to work with your text, and you review their next final delivery; after three rounds your last word closes the review. Approved closes your Task and lets the blocked one reach the user. If the task you review delivers again before you give the verdict, present your result again first: a verdict only counts for the delivery you saw. You cannot register a pull request: a reviewer delivers a verdict, not code.",
            json!({"task_id":{"type":"string"},"presentation_id":{"type":"string"},"verdict":{"type":"object","oneOf":[{"type":"object","properties":{"kind":{"type":"string","const":"approved"},"summary":{"type":"string"},"open_issues":{"type":"boolean"},"serious_findings":{"type":"integer","minimum":0}},"required":["kind","summary"],"additionalProperties":false},{"type":"object","properties":{"kind":{"type":"string","const":"changes_requested"},"text":{"type":"string"},"serious_findings":{"type":"integer","minimum":0}},"required":["kind","text","serious_findings"],"additionalProperties":false}]}}),
            vec!["task_id", "presentation_id", "verdict"],
        ),
    ];
    tools.extend(
        simple
            .into_iter()
            .map(|(name, _, description, fields, required)| {
                domain_tool(name, description, fields, &required)
            }),
    );
    tools
}

/// How Alondra starts a Task by itself once everything it depends on is done.
fn launch_plan_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"required":["name","cwd"],
        "description":"How Alondra starts this Task by itself once everything it depends on is done: required when dependencies or stack are set",
        "properties":{
            "name":{"type":"string","description":"The new node's name, with its hierarchy (lumo/phase-c)"},
            "cwd":{"type":"string","description":"The folder the node starts in"},
            "mission":{"type":"string","description":"The node's mission; without it, the task title and the request"},
            "model":{"type":"string","description":"Optional model named with its provider (claude/opus, codex/gpt-…) as the machine catalogs list it; a name without provider is refused when both providers offer it. Otherwise the configured work profile resolves Automatic"},
            "effort":{"type":"string","description":"Native reasoning effort advertised for the exact model, not inherited work effort"},
            "approach":{"type":"string","enum":["solo","subagents","workflow"]},
            "reason":{"type":"string","description":"Required when the request's effort is auto"},
            "role":{"type":"string","enum":["design","implementation","review"],"description":"For work split into design, implementation and review"},
            "tags":tags_schema()}})
}

fn launch_plan_changes_schema() -> Value {
    let mut schema = launch_plan_schema();
    schema["required"] = json!([]);
    schema["description"] = json!("Only the fields to change; omitted fields remain stored unchanged. Null is not accepted.");
    schema["properties"].as_object_mut().unwrap().retain(|key, _| matches!(key.as_str(), "model" | "effort" | "approach" | "tags" | "reason"));
    schema["properties"]["reasoning_effort"] = schema["properties"]["effort"].clone();
    schema
}

/// What the task is: kept with each model decision, so the model is chosen and measured by kind.
fn tags_schema() -> Value {
    use alondra_core::task_tags::{TaskArea, TaskKind, TaskRisk};
    json!({"type":"object","additionalProperties":false,"required":["kind"],
        "description":"What the task is: its kind, where it works and its risks. Alondra keeps them with the model chosen and reports results by kind",
        "properties":{
            "kind":{"type":"string","enum":TaskKind::names()},
            "areas":{"type":"array","items":{"type":"string","enum":TaskArea::names()}},
            "risks":{"type":"array","items":{"type":"string","enum":TaskRisk::names()},
                "description":"concurrency, security and data_migration call for the strongest model for the kind"}}})
}

fn orchestrator_domain_tools(user: &str) -> Vec<Value> {
    let interaction = interaction_fields();
    let mut tools = card_tools(user, &interaction, false);
    let simple = [
        (
            "create_request",
            "Create a durable Request from the user's new work.",
            json!({"request_id":{"type":"string"},"body":{"type":"string"},"auto_merge":{"type":"boolean","description":"True only if the user explicitly chose automatic PR merge"}}),
            vec!["request_id", "body"],
        ),
        (
            "close_request",
            "Close a Request: one that needs no Task with its answer, one whose Tasks are all over with what came of it, or mark it withdrawn. For a Request the user made, Alondra writes the user's card from it: headline (at most 90 characters, in the user's language) is the card's headline and already says the answer, answer the whole answer below it; once its Tasks are over, the card says Done and answer is its summary. A maintenance message, and a Request a node or you raised, get no card.",
            json!({"request_id":{"type":"string"},"answer":{"type":"string"},"headline":{"type":"string","maxLength":90},"withdrawn":{"type":"boolean"},"title":{"type":"string"}}),
            vec!["request_id"],
        ),
        (
            "title_request",
            "Name a Request with a short user-facing title (a few words, at most 60 characters, in the user's language, saying what it is about; never a copy of the prompt). Do this for every Request you split into Tasks, before creating its second Task: create_task refuses it on an untitled Request.",
            json!({"request_id":{"type":"string"},"title":{"type":"string","maxLength":60}}),
            vec!["request_id", "title"],
        ),
        (
            "create_task",
            "Create a cohesive Task under an open Request, with its launch plan (required), so Alondra starts it by itself when there is room and what it depends on is done. Dependencies are waits: the Task starts, or its node goes on, once they are done, and they never hold back the merge of a PR it presented; a PR built on another Task's open PR uses stack, which merges after it. A phase of an approved plan takes its points with plan_items: their numbers in the plan and the Task of this Request that holds them now; they leave that Task, which keeps the rest.",
            json!({"task_id":{"type":"string"},"request_id":{"type":"string"},"title":{"type":"string"},"agent_id":{"type":["string","null"]},"review_prs":{"type":"boolean"},"launch":launch_plan_schema(),"dependencies":{"type":"array","items":{"type":"string"}},"stack":{"type":["object","null"],"properties":{"upstream_task":{"type":"string"},"presentation_id":{"type":"string"},"pr_id":{"type":"string"},"head":{"type":"string"}},"required":["upstream_task","presentation_id","pr_id","head"],"additionalProperties":false},"plan_items":{"type":["object","null"],"properties":{"from_task":{"type":"string"},"items":{"type":"array","items":{"type":"string"},"minItems":1}},"required":["from_task","items"],"additionalProperties":false}}),
            vec![
                "task_id",
                "request_id",
                "title",
                "review_prs",
                "launch",
                "dependencies",
                "stack",
            ],
        ),
        (
            "assign_task",
            "Assign an active Task to a registered node that never worked on another Task: a node works on one Task for its whole life. To start work on a Task, use launch.",
            json!({"task_id":{"type":"string"},"agent_id":{"type":"string"}}),
            vec!["task_id", "agent_id"],
        ),
        (
            "retitle_task",
            "Give a Task a short user-facing title.",
            json!({"task_id":{"type":"string"},"title":{"type":"string"}}),
            vec!["task_id", "title"],
        ),
        (
            "set_dependencies",
            "Set a Task's dependencies and optional stack relationship. Dependencies are waits: the Task starts, or its node goes on, once they are done, and they never hold back the merge of a PR it presented; a PR built on another Task's open PR uses stack, which merges after it. A node that asks to wait for another Task gets it here, on its running Task, keeping the dependencies it has: Alondra resumes it when they are done.",
            json!({"task_id":{"type":"string"},"dependencies":{"type":"array","items":{"type":"string"}},"stack":{"type":["object","null"],"properties":{"upstream_task":{"type":"string"},"presentation_id":{"type":"string"},"pr_id":{"type":"string"},"head":{"type":"string"}},"required":["upstream_task","presentation_id","pr_id","head"],"additionalProperties":false}}),
            vec!["task_id", "dependencies", "stack"],
        ),
        (
            "set_relations",
            "Relate a Task to others beyond dependencies. delivered_dependencies: tasks whose final delivery it waits for (not their completion). blocks: tasks that cannot reach the user's review, or have a card, while this one is open. A review is B with delivered_dependencies [A] and blocks [A]; the reviewer's verdict (record_verdict) decides what happens to A.",
            json!({"task_id":{"type":"string"},"delivered_dependencies":{"type":"array","items":{"type":"string"}},"blocks":{"type":"array","items":{"type":"string"}}}),
            vec!["task_id", "delivered_dependencies", "blocks"],
        ),
        (
            "set_priority",
            "Set where a Task's node stands in the queue when more nodes want to work than the Mac takes: high, normal (the default) or low. Within a priority the oldest goes first. A Task that reviews another follows the priority of the one it reviews. Set it right after create_task when the Task is not normal.",
            json!({"task_id":{"type":"string"},"priority":{"type":"string","enum":["high","normal","low"]}}),
            vec!["task_id", "priority"],
        ),
        (
            "start_task",
            "Mark a Task started once its dependencies are ready.",
            json!({"task_id":{"type":"string"}}),
            vec!["task_id"],
        ),
        (
            "answer_question",
            "Answer a question addressed to the orchestrator: option_id is the option chosen (empty for an answer in your own words), and inputs.text your words, alone or with it. Every question a node asks you gets its answer here: an answer written only in your conversation never reaches the node. A node's suggestion is settled with settle_suggestion instead.",
            json!({"interaction_id":{"type":"string"},"option_id":{"type":"string"},"inputs":input_value_schema(),"attachments":interaction["attachments"]}),
            vec!["interaction_id", "option_id"],
        ),
        (
            "settle_suggestion",
            "Settle a node's suggestion with exactly one decision, which reaches the node. new_task creates the task in this same call, under a new request of its own (request_id, title of at most 60 characters, launch plan as in create_task), and the answer carries its id; current_task: the plan of the task that suggested it already covers it, and note says which step; discard: reason says which open task covers it or what the user turned down. If it is a product call, ask the user first and settle it once they answer.",
            json!({"interaction_id":{"type":"string"},"decision":{"type":"object","required":["kind"],"properties":{
                "kind":{"type":"string","enum":["new_task","current_task","discard"]},
                "task_id":{"type":"string","description":"new_task: the new task's id"},
                "request_id":{"type":"string","description":"new_task: the id of the request created for it"},
                "title":{"type":"string","maxLength":60,"description":"new_task: the task's title, also its request's"},
                "launch":launch_plan_schema(),
                "dependencies":{"type":"array","items":{"type":"string"},"description":"new_task: tasks it waits for"},
                "note":{"type":"string","description":"new_task: optional; current_task: required, the step of its plan that covers it"},
                "reason":{"type":"string","description":"discard: required"}}}}),
            vec!["interaction_id", "decision"],
        ),
        (
            "settle_plan_review",
            "Settle the plan review of one task a decision of the user holds: the message about that decision gives its review_id, the tasks it holds and their launch plans. unchanged: the plan still holds after the decision, and reason says why; plan_changed: launch is the whole new launch plan (as in create_task), stored in this same call, and reason says what changed; cancelled: the task no longer applies, reason says why, and it is cancelled in this same call. A held task never starts before this, and set_launch_plan does not change its plan. If it is a product call, ask the user first and settle it once they answer.",
            json!({"task_id":{"type":"string"},"review_id":{"type":"string"},"outcome":{"type":"object","required":["kind","reason"],"properties":{
                "kind":{"type":"string","enum":["unchanged","plan_changed","cancelled"]},
                "launch":launch_plan_schema(),
                "reason":{"type":"string","description":"required: why the plan holds, what changed in it, or why the task no longer applies"}}}}),
            vec!["task_id", "review_id", "outcome"],
        ),
        (
            "settle_stall",
            "Settle the case of a node that stopped with nothing explaining its next step (no result, no question, no work of its own): the message about it gives its case_id, the task, its mission, the node's last words and its worktree. It is never a question for the user. instruct: text is one concrete next step the node gets, and it goes on with its task; close: the task ends without a delivery, and result says what got done and what did not (the user reads it on the task). After three instructions since the task last moved on, close is the only outcome.",
            json!({"case_id":{"type":"string"},"task_id":{"type":"string"},"outcome":{"type":"object","required":["kind"],"properties":{
                "kind":{"type":"string","enum":["instruct","close"]},
                "text":{"type":"string","description":"instruct: required, the instruction the node gets"},
                "result":{"type":"string","description":"close: required, in the user's language: what got done and what did not"}}}}),
            vec!["case_id", "task_id", "outcome"],
        ),
        (
            "link_task_document",
            "Link a document to a Task with a named role.",
            json!({"task_id":{"type":"string"},"document_id":{"type":"string"},"role":{"type":"string"}}),
            vec!["task_id", "document_id", "role"],
        ),
        (
            "cancel_task",
            "Cancel an active Task when its Request no longer needs it.",
            json!({"task_id":{"type":"string"}}),
            vec!["task_id"],
        ),
        (
            "reopen_task",
            "Reopen a terminal Task while its Request remains open.",
            json!({"task_id":{"type":"string"}}),
            vec!["task_id"],
        ),
    ];
    tools.extend(
        simple
            .into_iter()
            .map(|(name, description, fields, required)| {
                domain_tool(name, description, fields, &required)
            }),
    );
    tools
}

/// Every call waits this long for alondrad.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a call waits for alondrad. `launch` and `send` wait for any start under way (up to a
/// launch) and then launch, or hand the task, themselves: a shorter wait gave up on calls that
/// went through, and the retry made a second node.
fn call_timeout(tool: &str) -> Duration {
    match tool {
        "launch" | "send" => alondra_core::LAUNCH_TIMEOUT * 2 + CALL_TIMEOUT,
        _ => CALL_TIMEOUT,
    }
}

fn call_node(client: &Client, name: &str, args: &Value) -> Result<String> {
    match name {
        "request_plan_mode" => execute_tool(client,name,args),
        "publish_information" | "propose" => anyhow::bail!("forbidden: {}", alondra_core::domain::NODES_DO_NOT_NOTIFY),
        "answer_clarification" if args.get("text").is_some() => {
            anyhow::bail!("forbidden: {} Settle a clarification with replacement, the corrected card.", alondra_core::domain::NODES_DO_NOT_NOTIFY)
        }
        "read_skill" | "task_context" => execute_tool(client, name, args),
        "start_task"
        | "ask"
        | "request_action"
        | "request_input"
        | "request_test"
        | "suggest"
        | "withdraw"
        | "withdraw_question"
        | "answer_clarification"
        | "answer_question"
        | "create_document"
        | "revise_document"
        | "register_pr"
        | "link_interaction_subject"
        | "present_result"
        | "publish_stack_readiness"
        | "record_verdict"
        | "cleanup_done"
        | "open_cards"
        | "post_merge_work"
        | "wait_for_mcp"
        | "wait_for_machine" => {
            let (args, notice) = match (std::env::current_dir(), name) {
                (Ok(cwd), "create_document" | "revise_document" | "present_result") => {
                    with_relative_images(name, args, &cwd, &alondra_core::config::alondra_home(&|k| std::env::var(k).ok()))
                }
                _ => (args.clone(), None),
            };
            let answer = execute_tool(client, name, &args)?;
            Ok(notice.map_or_else(|| answer.clone(), |notice| format!("{answer} {notice}")))
        }
        _ => anyhow::bail!("incompatible_protocol: this tool is unavailable to the node MCP"),
    }
}

/// A document or result reaches the user with the images it cites from the node's worktree
/// copied into Alondra's files folder; the node hears about any it left out.
fn with_relative_images(name: &str, args: &Value, cwd: &std::path::Path, home: &std::path::Path) -> (Value, Option<String>) {
    let mut args = args.clone();
    let mut skipped = vec![];
    if name == "present_result" {
        for pointer in ["/content/body", "/body"] {
            if let Some(body) = args.pointer_mut(pointer).filter(|b| b.is_string()) {
                let (carried, left) = crate::doc_images::carry(body.as_str().unwrap_or_default(), cwd, home);
                *body = json!(carried);
                for (dest, why) in left {
                    if !skipped.iter().any(|(seen, _)| *seen == dest) {
                        skipped.push((dest, why));
                    }
                }
            }
        }
    } else if let (Some(reference), Some(digest)) = (args["content_reference"].as_str(), args["content_digest"].as_str())
        && let Some((path, digest, left)) = crate::doc_images::carry_document(reference, digest, cwd, home)
    {
        args["content_reference"] = json!(path.to_string_lossy());
        args["content_digest"] = json!(digest);
        skipped = left;
    }
    (args, crate::doc_images::notice(&skipped))
}

fn call_orchestrator(client: &Client, name: &str, args: &Value) -> Result<String> {
    match name {
        "read_file" | "glob_files" | "search_files" | "read_skill" | "batch_outcomes" => execute_tool(client, name, args),
        "propose" => anyhow::bail!("forbidden: {}", alondra_core::domain::NODES_DO_NOT_NOTIFY),
        "create_request"
        | "close_request"
        | "title_request"
        | "create_task"
        | "assign_task"
        | "retitle_task"
        | "set_dependencies"
        | "set_relations"
        | "set_priority"
        | "start_task"
        | "publish_information"
        | "ask"
        | "request_action"
        | "request_input"
        | "raise_problem"
        | "withdraw"
        | "withdraw_question"
        | "answer_clarification"
        | "answer_question"
        | "settle_suggestion"
        | "settle_plan_review"
        | "settle_stall"
        | "link_task_document"
        | "cancel_task"
        | "reopen_task"
        | "launch"
        | "send"
        | "interrupt"
        | "set_launch_plan"
        | "update_launch_plan"
        | "get_launch_plan"
        | "machine"
        | "daemon_log"
        | "schedule_create"
        | "tasks_in_progress"
        | "open_cards"
        | "node" => execute_tool(client, name, args),
        _ => {
            anyhow::bail!("incompatible_protocol: this tool is unavailable to the orchestrator MCP")
        }
    }
}

fn execute_tool(client: &Client, name: &str, args: &Value) -> Result<String> {
    let waiting;
    let client = match call_timeout(name) {
        CALL_TIMEOUT => client,
        longer => {
            waiting = match client.operation_id() { Some(operation) => Client::new(longer).with_mcp_operation(operation),None => Client::new(longer) };
            &waiting
        }
    };
    Ok(match name {
        "request_plan_mode" => serde_json::to_string_pretty(&client.domain_post("/agents/plan-mode",&domain_bearer()?,args)?)?,
        "read_file" | "glob_files" | "search_files" => serde_json::to_string_pretty(&client.domain_post("/orchestrator/read", &domain_bearer()?, &json!({"operation":name,"arguments":args}))?)?,
        "read_skill" => serde_json::to_string_pretty(&client.domain_post("/tools/read-skill", &domain_bearer()?, args)?)?,
        "task_context" => serde_json::to_string_pretty(&client.domain_post("/agents/context", &domain_bearer()?, args)?)?,
        "batch_outcomes" => serde_json::to_string_pretty(&client.domain_post("/orchestrator/outcomes", &domain_bearer()?, args)?)?,
        "answer_clarification" => answer_clarification(client, args)?,
        "ask"
        | "request_action"
        | "request_input"
        | "request_test"
        | "suggest"
        | "raise_problem"
        | "publish_information"
        | "withdraw"
        | "withdraw_question"
        | "answer_question"
        | "settle_suggestion"
        | "settle_plan_review"
        | "settle_stall"
        | "create_request"
        | "close_request"
        | "title_request"
        | "create_task"
        | "assign_task"
        | "retitle_task"
        | "set_dependencies"
        | "set_relations"
        | "set_priority"
        | "start_task"
        | "create_document"
        | "revise_document"
        | "link_task_document"
        | "register_pr"
        | "link_interaction_subject"
        | "present_result"
        | "publish_stack_readiness"
        | "record_verdict"
        | "cancel_task"
        | "reopen_task"
        | "cleanup_done"
        | "post_merge_work" => submit(client, &domain_bearer()?, name, args)?,
        "wait_for_mcp" => {
            submit(client, &domain_bearer()?, name, args)?;
            let server = args.get("server").and_then(Value::as_str).unwrap_or("the server");
            format!("Noted: your task waits for {server}. End your turn now; Alondra tells you when it is connected or that you go on without it.")
        }
        "launch" | "send" | "interrupt" => {
            let bearer = domain_bearer()?;
            serde_json::to_string_pretty(&client.domain_post(
                &format!("/orchestrator/{name}"),
                &bearer,
                args,
            )?)?
        }
        "get_launch_plan" => {
            let bearer = domain_bearer()?;
            let task = args.get("task_id").and_then(Value::as_str).unwrap_or("");
            serde_json::to_string_pretty(&client.domain_get(&format!("/orchestrator/launch-plan?task_id={}", urlencode(task)), &bearer)?)?
        }
        "update_launch_plan" => {
            let bearer = domain_bearer()?;
            serde_json::to_string_pretty(&client.domain_post("/orchestrator/launch-plan/update", &bearer, args)?)?
        }
        "set_launch_plan" => {
            let bearer = domain_bearer()?;
            serde_json::to_string_pretty(&client.domain_post("/orchestrator/launch-plan", &bearer, args)?)?
        }
        "schedule_create" => {
            let bearer = domain_bearer()?;
            serde_json::to_string_pretty(&client.domain_post(
                "/orchestrator/schedules",
                &bearer,
                args,
            )?)?
        }
        "tasks_in_progress" => {
            let bearer = domain_bearer()?;
            serde_json::to_string_pretty(&tasks_in_progress_summary(
                &client.domain_get("/orchestrator/tasks_in_progress", &bearer)?,
                flag(args, "all"),
            ))?
        }
        "node" => {
            let bearer = domain_bearer()?;
            let key = args.get("node").and_then(Value::as_str).unwrap_or("");
            let page = if flag(args, "history") {
                Page::History { before: None }
            } else {
                Page::Recent
            };
            serde_json::to_string_pretty(&node_view(
                &client.domain_get(
                    &format!("/orchestrator/node?node={}", urlencode(key)),
                    &bearer,
                )?,
                page,
            ))?
        }
        "open_cards" => {
            let bearer = domain_bearer()?;
            let path = match args.get("task_id").and_then(Value::as_str) {
                Some(task) => format!("/cards/open?task_id={}", urlencode(task)),
                None => "/cards/open".to_string(),
            };
            serde_json::to_string_pretty(&client.domain_get(&path, &bearer)?)?
        }
        "wait_for_machine" => {
            let bearer = domain_bearer()?;
            let reason = args.get("reason").and_then(Value::as_str).unwrap_or("");
            let resource: alondra_core::machine_incident::MachineResource = serde_json::from_value(args.get("resource").cloned().unwrap_or(Value::Null))
                .map_err(|_| anyhow::anyhow!("invalid_input: resource must be one of memory, disk, cpu or build_queue. Call wait_for_machine again with it"))?;
            client.domain_post(
                "/agents/waiting-machine",
                &bearer,
                &json!({"resource": resource, "reason": reason}),
            )?;
            "Noted: you are waiting for the machine. End your turn now; Alondra tells you to carry on when the Mac has room for you.".to_string()
        }
        "machine" => {
            let bearer = domain_bearer()?;
            machine_text(client.domain_get("/orchestrator/machine", &bearer)?)?
        }
        "daemon_log" => {
            let bearer = domain_bearer()?;
            let path = match args["lines"].as_u64() {
                Some(lines) => format!("/orchestrator/log?lines={lines}"),
                None => "/orchestrator/log".to_string(),
            };
            client.domain_get(&path, &bearer)?["log"].as_str().unwrap_or_default().to_string()
        }
        other => anyhow::bail!("unknown tool: {other}"),
    })
}

/// The machine as the orchestrator reads it: the table of available models first, as plain
/// lines, then the rest of the answer.
fn machine_text(mut answer: Value) -> Result<String> {
    let table = answer.as_object_mut().and_then(|fields| fields.remove("available_models")).and_then(|table| table.as_str().map(str::to_owned));
    let rest = serde_json::to_string_pretty(&answer)?;
    Ok(match table {
        Some(table) => format!("{table}\n{rest}"),
        None => rest,
    })
}

/// Sends one typed command. A card carries its own id, so a retry after a timeout is the same
/// command and the daemon answers it with the first outcome instead of making a second card.
fn submit(client: &Client, bearer: &str, name: &str, args: &Value) -> Result<String> {
    let capabilities = client.domain_get("/capabilities", bearer)?;
    let data_epoch = capabilities.get("data_epoch").and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("incompatible_protocol: daemon returned no data epoch"))?;
    let (method, path, body) = scoped_command_request(name, args, data_epoch,client.operation_id())?;
    let response = client.domain_call(method, &path, bearer, Some(&body))?;
    Ok(response.get("notice").and_then(Value::as_str).unwrap_or("Recorded.").to_owned())
}

/// The resource call a tool makes: its typed command as the body.
fn command_request(name: &str, args: &Value, data_epoch: &str) -> Result<(&'static str, String, Value)> {
    let envelope = domain_envelope(name, args, data_epoch, &command_id(name, args))?;
    let (method, path) = resource_target(name, &envelope["command"])?;
    let mut body = envelope["command"].clone();
    body.as_object_mut().expect("typed command object").remove("kind");
    for key in ["data_epoch", "command_id", "expected_revision"] {
        body[key] = envelope[key].clone();
    }
    Ok((method, path, body))
}

fn scoped_command_request(name:&str,args:&Value,epoch:&str,operation:Option<&str>)->Result<(&'static str,String,Value)> {
    let (method,path,mut body)=command_request(name,args,epoch)?;
    let stable_card=CARD_COMMANDS.contains(&name) && args.get("interaction_id").and_then(Value::as_str).is_some_and(|id|id.len()<=200 && !id.chars().any(char::is_control));
    if let Some(operation)=operation && !stable_card {
        use sha2::{Digest,Sha256};
        let digest=Sha256::digest(serde_json::to_vec(&(operation,name))?);
        let identity:String=digest.iter().map(|byte|format!("{byte:02x}")).collect();
        body["command_id"]=json!(format!("mcp-native-{identity}"));
    }
    Ok((method,path,body))
}

const CARD_COMMANDS: [&str; 10] = [
    "ask", "request_action", "request_input", "request_test", "suggest", "raise_problem",
    "publish_information", "present_result", "withdraw", "withdraw_question",
];

fn command_id(name: &str, args: &Value) -> String {
    let card = args.get("interaction_id").and_then(Value::as_str)
        .filter(|id| id.len() <= 200 && !id.chars().any(char::is_control));
    match card {
        Some(id) if CARD_COMMANDS.contains(&name) => format!("mcp-{name}-{id}"),
        _ => crate::client::new_id("mcp"),
    }
}

/// What a node reads when a call fails: a daemon it could not reach is never a success, and a
/// refusal says what is wrong, where and how to fix it, and that nothing was granted.
fn explain(error: &anyhow::Error) -> String {
    if let Some(crate::client::Refused(refused)) = error.downcast_ref() {
        let guidance = match refused.code.as_str() {
            "idempotency_conflict" => "That id already carries a different card: send a new interaction_id for a different card, or replaces to correct one that is still open.",
            _ => "Fix what it says and send the card again with the same interaction_id. Repeating the same refused card three times raises a Problem for the user.",
        };
        return format!("Alondra refused it [{}]: {} Nothing was sent, and a refusal is never an answer or a permission: do not go on as if it had been granted. {guidance}", refused.code, refused.message);
    }
    if error.chain().any(|cause| cause.downcast_ref::<ureq::Error>().is_some()) {
        return "don't proceed: could not reach Alondra; the mutation's result is unconfirmed. Inspect its durable outcome and current state before continuing; never blindly send it again or treat the lost reply as permission.".to_owned();
    }
    error.to_string()
}

fn answer_clarification(client: &Client, args: &Value) -> Result<String> {
    let text_of = |key: &str| args.get(key).and_then(Value::as_str).filter(|s| !s.trim().is_empty());
    let Some(card) = text_of("id") else {
        anyhow::bail!("invalid_input: id is required: the card the user asked you to clarify");
    };
    match (args.get("replacement"), text_of("text")) {
        (Some(replacement), None) => {
            let tool = replacement.get("tool").and_then(Value::as_str).unwrap_or_default();
            if !["ask", "request_action", "request_input", "request_test", "present_result"].contains(&tool) {
                anyhow::bail!("invalid_input: replacement.tool must be one of ask, request_action, request_input, request_test, present_result");
            }
            let mut arguments = replacement.get("arguments").cloned().unwrap_or(json!({}));
            arguments["replaces"] = json!(card);
            execute_tool(client, tool, &arguments)
        }
        (None, Some(text)) => {
            let mut notice = serde_json::Map::new();
            for key in ["interaction_id", "scope", "recipient"] {
                let value = args.get(key).filter(|v| !v.is_null()).ok_or_else(|| {
                    anyhow::anyhow!("invalid_input: {key} is required with text: it says where the notice goes")
                })?;
                notice.insert(key.into(), value.clone());
            }
            let first_line = text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or(text);
            notice.insert("need".into(), json!(alondra_core::domain::clip(first_line, alondra_core::domain::CardSpec::NEED_MAX)));
            notice.insert("body".into(), json!(text));
            let bearer = domain_bearer()?;
            submit(client, &bearer, "publish_information", &Value::Object(notice))?;
            submit(client, &bearer, "withdraw", &json!({"interaction_id": card}))?;
            Ok("Recorded: the user has your answer and the card is withdrawn.".to_owned())
        }
        _ => anyhow::bail!("invalid_input: send exactly one of replacement (the corrected card) or text"),
    }
}

pub(crate) fn resource_target(name: &str, command: &Value) -> Result<(&'static str, String)> {
    let value = |key: &str| -> Result<&str> {
        command.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("{name} requires {key}"))
    };
    let id = |key: &str| -> Result<String> { Ok(urlencode(value(key)?)) };
    let target = match name {
        "ask" if asks_together(command) => ("POST", "/question-groups".to_owned()),
        "ask" | "ask_question" | "request_input" | "request_test" | "suggest" => ("POST", "/questions".to_owned()),
        "request_action" => ("POST", "/action-requests".to_owned()),
        "publish_information" => ("POST", "/messages".to_owned()),
        "raise_problem" => ("POST", "/problems".to_owned()),
        "withdraw" | "withdraw_question" => ("POST", format!("/interactions/{}/withdrawal", id("interaction_id")?)),
        "request_clarification" => ("POST", format!("/interactions/{}/clarification", id("interaction_id")?)),
        "answer_question" => ("POST", format!("/interactions/{}/answers", id("interaction_id")?)),
        "settle_suggestion" => ("POST", format!("/interactions/{}/settlement", id("interaction_id")?)),
        "settle_plan_review" => ("POST", format!("/tasks/{}/plan-review", id("task_id")?)),
        "settle_stall" => ("POST", format!("/tasks/{}/stall-settlement", id("task_id")?)),
        "mark_read" => ("PUT", format!("/interactions/{}/read-state", id("interaction_id")?)),
        "submit_feedback" => ("POST", format!("/interactions/{}/feedback", id("presentation_id")?)),
        "accept_delivery" => ("POST", format!("/interactions/{}/acceptance", id("presentation_id")?)),
        "authorize_merge" => ("POST", format!("/interactions/{}/merge-authorizations", id("presentation_id")?)),
        "create_request" => ("POST", "/requests".to_owned()),
        "close_request" => ("POST", format!("/requests/{}/close", id("request_id")?)),
        "title_request" => ("POST", format!("/requests/{}/title", id("request_id")?)),
        "create_task" => ("POST", format!("/requests/{}/tasks", id("request_id")?)),
        "assign_task" => ("PUT", format!("/tasks/{}/assignee", id("task_id")?)),
        "retitle_task" => ("PATCH", format!("/tasks/{}", id("task_id")?)),
        "set_dependencies" => ("PUT", format!("/tasks/{}/dependencies", id("task_id")?)),
        "set_relations" => ("PUT", format!("/tasks/{}/relations", id("task_id")?)),
        "set_priority" => ("PUT", format!("/tasks/{}/priority", id("task_id")?)),
        "record_verdict" => ("POST", format!("/tasks/{}/verdict", id("task_id")?)),
        "start_task" => ("POST", format!("/tasks/{}/start", id("task_id")?)),
        "cleanup_done" => ("POST", format!("/tasks/{}/cleanup", id("task_id")?)),
        "post_merge_work" => ("PUT", format!("/tasks/{}/post-merge-work", id("task_id")?)),
        "wait_for_mcp" => ("PUT", format!("/tasks/{}/mcp-wait", id("task_id")?)),
        "create_document" => ("POST", format!("/tasks/{}/documents", id("task_id")?)),
        "revise_document" => ("PUT", format!("/tasks/{}/documents/{}", id("task_id")?, id("document_id")?)),
        "decide_document_review" => ("PUT", format!("/tasks/{}/documents/{}/review", id("task_id")?, id("document_id")?)),
        "link_task_document" => ("POST", format!("/tasks/{}/document-links", id("task_id")?)),
        "register_pr" => ("POST", format!("/tasks/{}/pull-requests", id("task_id")?)),
        "link_interaction_subject" => ("PUT", format!("/interactions/{}/subject", id("interaction_id")?)),
        "present_result" => ("POST", format!("/tasks/{}/results", id("task_id")?)),
        "publish_stack_readiness" => ("POST", format!("/tasks/{}/stack-readiness", id("task_id")?)),
        "cancel_task" => ("POST", format!("/tasks/{}/cancellation", id("task_id")?)),
        "reopen_task" => ("POST", format!("/tasks/{}/reopening", id("task_id")?)),
        _ => anyhow::bail!("unknown domain resource operation: {name}"),
    };
    Ok(target)
}

fn domain_envelope(name: &str, args: &Value, data_epoch: &str, command_id: &str) -> Result<Value> {
    let kind = match name {
        "ask" if asks_together(args) => "ask_questions",
        "ask" | "request_input" | "request_test" | "suggest" => "ask_question",
        "request_action" => "request_action",
        "publish_information" => "publish_information",
        "raise_problem" => "raise_problem",
        "withdraw" | "withdraw_question" => "withdraw",
        "answer_question" => "answer_question",
        "settle_suggestion" => "settle_suggestion",
        "settle_plan_review" => "settle_plan_review",
        "settle_stall" => "settle_stall",
        "create_request" => "create_request",
        "close_request" => "close_request",
        "title_request" => "title_request",
        "create_task" => "create_task",
        "assign_task" => "assign_task",
        "retitle_task" => "retitle_task",
        "set_dependencies" => "set_dependencies",
        "set_relations" => "set_relations",
        "set_priority" => "set_priority",
        "record_verdict" => "record_verdict",
        "start_task" => "start_task",
        "cleanup_done" => "confirm_cleanup",
        "post_merge_work" => "declare_post_merge_work",
        "wait_for_mcp" => "wait_for_mcp",
        "create_document" => "create_document",
        "revise_document" => "revise_document",
        "link_task_document" => "link_task_document",
        "register_pr" => "register_pr",
        "link_interaction_subject" => "link_interaction_subject",
        "present_result" => "present_result",
        "publish_stack_readiness" => "publish_stack_readiness",
        "cancel_task" => "cancel_task",
        "reopen_task" => "reopen_task",
        _ => anyhow::bail!("unknown domain tool: {name}"),
    };
    let mut command = args
        .as_object()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("tool arguments must be an object"))?;
    for key in PROTOCOL_METADATA {
        command.remove(key);
    }
    attach_card(name, &mut command)?;
    if name == "register_pr" {
        if let Some(host) = command.get("host").and_then(Value::as_str) {
            let host = alondra_core::domain::normalize_pr_host(host);
            command.insert("host".into(), Value::String(host));
        }
        if let Some(repository) = command.get("repository").and_then(Value::as_str) {
            let repository = alondra_core::domain::normalize_pr_repository(repository);
            command.insert("repository".into(), Value::String(repository));
        }
        if let (Some(host), Some(repository), Some(number)) = (
            command.get("host").and_then(Value::as_str),
            command.get("repository").and_then(Value::as_str),
            command.get("number").and_then(Value::as_u64),
        ) {
            let id = alondra_core::domain::hosted_pr_id(host, repository, number);
            command.insert("pr_id".into(), Value::String(id.0));
        }
    }
    if name == "post_merge_work" && let Some(waits) = command.get_mut("waits_for").and_then(Value::as_array_mut) {
        for wait in waits.iter_mut() {
            if let Some(text) = wait.as_str() {
                *wait = serde_json::to_value(alondra_core::domain::WaitTarget::parse(text))?;
            }
        }
    }
    command.insert("kind".into(), Value::String(kind.into()));
    let envelope = json!({"protocol_version":2,"data_epoch":data_epoch,
        "command_id":command_id,"expected_revision":0,"command":command});
    let mut typed = envelope.clone();
    for target in typed.pointer_mut("/command/targets").and_then(Value::as_array_mut).into_iter().flatten() {
        if target.get("kind").and_then(Value::as_str) == Some("pull_request") {
            for field in ["pr_id", "head"] {
                if target.get(field).is_none() {
                    target[field] = Value::String("filled-by-alondra".into());
                }
            }
        }
    }
    serde_json::from_value::<alondra_core::domain::CommandEnvelope>(typed).map_err(|error| {
        let error = error.to_string();
        let reason = error.split(", expected").next().unwrap_or(&error);
        anyhow::anyhow!("invalid_input: {reason}. Fix the arguments of {name} to match its schema and call it again")
    })?;
    Ok(envelope)
}

/// The kind of card each card tool puts in front of its reader.
fn card_kind(name: &str, command: &serde_json::Map<String, Value>) -> Option<&'static str> {
    Some(match name {
        "ask" => "decide",
        "request_action" => "do",
        "request_input" => "provide",
        "request_test" => "test",
        "suggest" => "propose",
        "publish_information" => "know",
        "raise_problem" => "problem",
        "present_result" if command.get("is_final").and_then(Value::as_bool) == Some(true) => "review",
        "present_result" => "know",
        _ => return None,
    })
}

/// Only a plan Claude Code holds is a plan: content a node labels one is always a report, so the
/// card never claims a plan was approved.
fn unplan_content(mut content: Value) -> Value {
    if content["kind"].as_str() == Some("plan") {
        content["kind"] = json!("report");
    }
    content
}

/// The steps of an action card always reach its detail: alone they are its content, and beside
/// content the node wrote they follow it, with a command or diff kept fenced above them.
fn with_steps(content: Option<Value>, steps: &str) -> Value {
    let Some(content) = content else { return json!({"kind":"steps","body":steps}) };
    let body = content["body"].as_str().unwrap_or_default().trim_end();
    let (kind, body) = match content["kind"].as_str() {
        Some(kind @ ("command" | "diff")) => {
            let fence = "`".repeat((body.split(|c| c != '`').map(str::len).max().unwrap_or(0) + 1).max(3));
            ("report", format!("{fence}{}\n{body}\n{fence}", if kind == "diff" { "diff" } else { "" }))
        }
        kind => (kind.unwrap_or("report"), body.to_owned()),
    };
    let body = if body.is_empty() { steps.to_owned() } else { format!("{body}\n\n{steps}") };
    json!({"kind":kind,"body":body})
}

/// The worktree Markdown files a card lists in `files`, read from where the agent works and
/// included at the end of `text`: the user cannot open a file from the app.
fn with_files(text: &str, files: &[String], cwd: Option<&std::path::Path>) -> Result<String> {
    alondra_core::domain::include_files(text, files, |path| alondra_core::cited_file::read_cited_file(cwd, path))
        .map_err(|fix| anyhow::anyhow!("invalid_input: files: {fix}"))
}

fn take_files(command: &mut serde_json::Map<String, Value>) -> Result<Vec<String>> {
    match command.remove("files").filter(|v| !v.is_null()) {
        None => Ok(vec![]),
        Some(files) => serde_json::from_value(files).map_err(|_| anyhow::anyhow!("invalid_input: files must be a list of paths relative to your worktree, like [\"docs/plan.md\"]")),
    }
}

fn choice(id: &str, label: &str) -> Value {
    json!({"id":id,"label":label})
}

fn take_text(command: &mut serde_json::Map<String, Value>, key: &str) -> String {
    command.remove(key).and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default()
}

/// An `ask` that puts several questions in front of the user at once.
fn asks_together(command: &Value) -> bool {
    command.get("questions").and_then(Value::as_array).is_some_and(|questions| questions.len() > 1)
}

/// An option labelled "(Recommended)" is the recommended one: the label loses the mark, and the
/// option's id goes to `recommended` unless the author already named one.
fn recommend_marked(options: Option<&mut Value>, recommended: &mut Option<Value>) {
    let Some(options) = options.and_then(Value::as_array_mut) else { return };
    for option in options {
        let bare = option["label"].as_str().and_then(alondra_core::domain::without_recommended_suffix);
        if let Some(bare) = bare {
            if recommended.is_none() {
                *recommended = Some(option["id"].clone()).filter(|id| id.is_string());
            }
            option["label"] = json!(bare);
        }
    }
}

/// The marks "(Recommended)" of an `ask`'s labels become `recommended`. An `ask` with
/// `questions` puts them in front of the user together, one card each, answered one by one and
/// delivered together: two or more reshape the command into the group (true); one is the card's
/// single decision.
fn fold_questions(command: &mut serde_json::Map<String, Value>) -> Result<bool> {
    let mut recommended = command.remove("recommended").filter(|v| !v.is_null());
    recommend_marked(command.get_mut("options"), &mut recommended);
    let Some(questions) = command.remove("questions").filter(|v| !v.is_null()) else {
        if let Some(recommended) = recommended {
            command.insert("recommended".into(), recommended);
        }
        return Ok(false);
    };
    let Value::Array(questions) = questions else { anyhow::bail!("invalid_input: questions is a list of {{question, options, recommended, why}}") };
    let mut folded = vec![];
    for question in questions {
        let Value::Object(mut question) = question else { anyhow::bail!("invalid_input: every question is {{question, options, recommended, why}}") };
        let text = take_text(&mut question, "question");
        let mut chosen = question.remove("recommended").filter(|v| !v.is_null());
        recommend_marked(question.get_mut("options"), &mut chosen);
        let mut one = serde_json::Map::new();
        one.insert("need".into(), json!(text));
        one.insert("why".into(), json!(take_text(&mut question, "why")));
        one.insert("options".into(), question.remove("options").unwrap_or_else(|| json!([])));
        if let Some(chosen) = chosen {
            one.insert("recommended".into(), chosen);
        }
        if let Some(extra) = question.keys().next() {
            anyhow::bail!("invalid_input: a question has no field {extra}: use question, options, recommended and why");
        }
        folded.push(Value::Object(one));
    }
    let has_options = command.get("options").and_then(Value::as_array).is_some_and(|options| !options.is_empty());
    if folded.len() == 1 {
        if has_options {
            anyhow::bail!("invalid_input: send options or questions, not both");
        }
        let Some(Value::Object(only)) = folded.pop() else { unreachable!() };
        if command.get("need").and_then(Value::as_str).is_none_or(|need| need.trim().is_empty()) {
            command.insert("need".into(), only["need"].clone());
        }
        command.insert("options".into(), only["options"].clone());
        if let Some(chosen) = only.get("recommended") {
            command.insert("recommended".into(), chosen.clone());
        }
        return Ok(false);
    }
    if has_options || recommended.is_some() {
        anyhow::bail!("invalid_input: with questions each one has its own options and recommended: leave the ask's own out");
    }
    for key in ["content", "blocks", "replaces", "verify", "attachments", "body", "options", "need"] {
        if command.get(key).is_some_and(|value| !value.is_null()) && key != "need" && key != "options" {
            anyhow::bail!("invalid_input: {key} does not go with questions: each question is a card of its own with its options and why");
        }
        command.remove(key);
    }
    command.entry("why").or_insert_with(|| json!(""));
    command.insert("questions".into(), Value::Array(folded));
    Ok(true)
}

/// The card a card tool describes, from the flat fields the node sent. Missing words stay empty
/// so alondrad refuses them with the field and the fix, and counts the refusal.
fn attach_card(name: &str, command: &mut serde_json::Map<String, Value>) -> Result<()> {
    let Some(kind) = card_kind(name, command) else { return Ok(()) };
    if command.contains_key("card") {
        anyhow::bail!("invalid_input: {name} has no field card: need, why and content go at the top level of its arguments");
    }
    if name == "ask" && fold_questions(command)? {
        return Ok(());
    }
    if name == "suggest" {
        suggestion_fields(command)?;
    }
    let need = take_text(command, "need");
    let mut why = take_text(command, "why");
    let mut content = command.remove("content").filter(|v| !v.is_null()).map(unplan_content);
    let mut card = serde_json::Map::new();
    for key in ["recommended", "blocks", "replaces", "verify", "cause"] {
        if let Some(value) = command.remove(key).filter(|v| !v.is_null()) {
            card.insert(key.into(), value);
        }
    }
    let steps = command.remove("steps");
    match name {
        "request_action" => {
            let lines: Vec<String> = steps.as_ref().and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str)
                .enumerate().map(|(i, step)| format!("{}. {step}", i + 1)).collect();
            if !lines.is_empty() {
                content = Some(with_steps(content, &lines.join("\n")));
            }
            command.entry("options").or_insert_with(|| json!([choice("done", "Done"), choice("cannot", "I can't do it")]));
        }
        "request_input" => {
            let kind = take_text(command, "kind");
            if !matches!(kind.as_str(), "file" | "text" | "secret") {
                anyhow::bail!("invalid_input: kind must be file, text or secret");
            }
            match (kind.as_str(), command.remove("destination").filter(|v| !v.is_null())) {
                ("secret", Some(destination)) => { card.insert("secret_destination".into(), destination); }
                ("secret", None) => anyhow::bail!("invalid_input: destination is required for kind secret: the git-ignored file of the repository (like .env) and the variable name (like STRIPE_KEY) Alondra writes the value to"),
                (_, Some(_)) => anyhow::bail!("invalid_input: destination goes only with kind secret"),
                _ => {}
            }
            command.insert("options".into(), json!([choice("skip", "I can't provide it")]));
        }
        "request_test" => {
            let (place, what) = (take_text(command, "where"), take_text(command, "what"));
            if place.trim().is_empty() || what.trim().is_empty() {
                anyhow::bail!("invalid_input: where and what are required: say where to test it (build, version, screen) and what to look at");
            }
            if why.is_empty() {
                why = alondra_core::domain::clip(&format!("{place}: {what}"), alondra_core::domain::CardSpec::WHY_MAX);
            }
            content = content.or_else(|| Some(json!({"kind":"steps","body":format!("Where: {place}\nWhat to look at: {what}")})));
            command.insert("options".into(), json!([choice("works", "Works"), choice("fails", "Doesn't work")]));
            command.entry("blocking").or_insert(json!(false));
        }
        "publish_information" | "present_result" if content.is_none() => {
            content = command.get("body").and_then(Value::as_str).map(str::trim)
                .filter(|body| !body.is_empty() && *body != need.trim())
                .map(|body| json!({"kind":"report","body":body}));
        }
        _ => {}
    }
    let cwd = std::env::current_dir().ok();
    let files = take_files(command)?;
    let plan = (name == "present_result").then(|| command.remove("plan")).flatten()
        .and_then(|v| v.as_str().map(str::trim).map(str::to_owned)).filter(|plan| !plan.is_empty());
    if let Some(plan) = plan {
        card.insert("plan".into(), json!(with_files(&plan, &files, cwd.as_deref())?));
    } else if !files.is_empty() {
        let mut shown = content.unwrap_or_else(|| json!({"kind": "document"}));
        shown["body"] = json!(with_files(shown["body"].as_str().unwrap_or_default(), &files, cwd.as_deref())?);
        content = Some(shown);
    }
    if !files.is_empty() {
        card.insert("files".into(), json!(files));
    }
    card.insert("kind".into(), json!(kind));
    card.insert("need".into(), json!(need));
    card.insert("why".into(), json!(why));
    if let Some(content) = &content {
        card.insert("content".into(), content.clone());
    }
    if !command.contains_key("body") && name != "publish_information" {
        let body = content.as_ref().and_then(|c| c["body"].as_str()).filter(|b| !b.trim().is_empty())
            .or(Some(why.as_str()).filter(|w| !w.is_empty()))
            .or(Some(need.as_str()).filter(|n| !n.is_empty()))
            .unwrap_or("(no need given)");
        command.insert("body".into(), json!(body));
    }
    command.insert("card".into(), Value::Object(card));
    Ok(())
}

/// A suggestion is a question for the orchestrator with its fixed answers: what the node saw is
/// the need, why it is not the node's the why, and where it is (with any detail) the content.
fn suggestion_fields(command: &mut serde_json::Map<String, Value>) -> Result<()> {
    let (what, place, not_mine, detail) =
        (take_text(command, "what"), take_text(command, "where"), take_text(command, "why_not_mine"), take_text(command, "detail"));
    if what.trim().is_empty() || place.trim().is_empty() || not_mine.trim().is_empty() {
        anyhow::bail!("invalid_input: what, where and why_not_mine are required: say what you saw, where, and why it is not part of your task");
    }
    let mut body = format!("Where: {}", place.trim());
    if !detail.trim().is_empty() {
        body = format!("{body}\n\n{}", detail.trim());
    }
    command.insert("need".into(), json!(what));
    command.insert("why".into(), json!(not_mine));
    command.insert("content".into(), json!({"kind":"report","body":body}));
    command.insert("recipient".into(), json!({"kind":"orchestrator"}));
    command.insert("blocking".into(), json!(false));
    command.insert("options".into(), serde_json::to_value(alondra_core::domain::suggestion_options())?);
    Ok(())
}

fn domain_bearer() -> Result<String> {
    let bearer = std::env::var("ALONDRA_DOMAIN_CAPABILITY").ok().or_else(settings_bearer)
        .ok_or_else(|| anyhow::anyhow!("incompatible_protocol: domain producer capability unavailable"))?;
    validate_domain_bearer(&bearer)?;
    Ok(bearer)
}

/// A background session's bearer, written by the daemon into the settings file it was launched
/// with once the session was bound; read on every call, since it can arrive after startup.
fn settings_bearer() -> Option<String> {
    let path = std::env::var_os("ALONDRA_DOMAIN_SETTINGS")?;
    let settings: Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    settings["env"]["ALONDRA_DOMAIN_CAPABILITY"].as_str().map(str::to_owned)
}

fn validate_domain_bearer(bearer: &str) -> Result<()> {
    if bearer.len() != 64 || !bearer.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        anyhow::bail!("incompatible_protocol: invalid domain producer capability");
    }
    Ok(())
}

fn flag(args: &Value, key: &str) -> bool {
    match args.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s == "true",
        _ => false,
    }
}

fn pick(v: &Value, keys: &[&str]) -> Value {
    Value::Object(keys.iter().filter_map(|k| v.get(*k).filter(|x| !x.is_null()).map(|x| (k.to_string(), x.clone()))).collect())
}

/// The subagents and shells a node runs, when it runs any.
fn running(n: &Value) -> Option<Value> {
    n.get("background").filter(|b| b.as_array().is_some_and(|a| !a.is_empty())).cloned()
}

fn list(v: &Value, key: &str) -> Vec<Value> {
    v.get(key).and_then(Value::as_array).cloned().unwrap_or_default()
}

/// What the orchestrator acts on: unfinished nodes that are running, dormant, or still owe
/// the user something. `all` adds finished nodes and the ones whose session is gone.
fn tasks_in_progress_summary(o: &Value, all: bool) -> Value {
    let open = list(o, "open_questions");
    let unread: Vec<Value> = list(o, "reports").into_iter().filter(|r| r.get("read") == Some(&Value::Bool(false))).collect();
    let owes = |session: Option<&str>| open.iter().chain(&unread).any(|x| x.get("session_id").and_then(Value::as_str) == session);
    let (mut done, mut gone) = (0, 0);
    let mut nodes = vec![];
    for n in list(o, "nodes") {
        let is = |k: &str| n.get(k) == Some(&Value::Bool(true));
        let finished = n.get("state").and_then(Value::as_str) == Some("done");
        let present = is("alive") || is("in_daemon") || owes(n.get("session_id").and_then(Value::as_str));
        if !all && finished {
            done += 1;
        } else if !all && !present {
            gone += 1;
        } else {
            let mut row = pick(&n, &["name", "short_id", "project", "state", "state_since", "model", "daemon_needs", "waiting_permission", "alive", "in_daemon", "sdk_host", "undeliverable_messages"]);
            if let Some(b) = running(&n) {
                row["background"] = b;
            }
            nodes.push(row);
        }
    }
    let questions: Vec<Value> = open.iter().map(|q| pick(q, &["id", "title", "node_name", "origin", "to_orchestrator", "action", "asked_at"])).collect();
    let reports: Vec<Value> = unread.iter().map(|r| pick(r, &["id", "title", "kind", "node_name"])).collect();
    let messages: Vec<Value> =
        list(o, "orchestrator_messages").into_iter().filter(|m| all || m.get("handed_at").is_none_or(Value::is_null)).collect();
    let (requests, done_requests) = requests_summary(o, all);
    let mut out = json!({"nodes": nodes, "requests": requests, "open_questions": questions, "unread_reports": reports,
                         "orchestrator_messages": messages, "machine": o.get("machine"), "usage": o.get("usage")});
    if done + gone + done_requests > 0 {
        let mut hidden = serde_json::Map::new();
        for (k, n) in [("done_nodes", done), ("gone_nodes", gone), ("done_requests", done_requests)] {
            if n > 0 {
                hidden.insert(k.into(), json!(n));
            }
        }
        hidden.insert("see_them_with".into(), json!("tasks_in_progress with all: true"));
        out["hidden"] = Value::Object(hidden);
    }
    out
}

/// The requests still open, each with its tasks: their state, what they build on and the node
/// on them. A request with no task yet is one to `define`. `all` adds the finished ones.
fn requests_summary(o: &Value, all: bool) -> (Vec<Value>, usize) {
    let nodes = list(o, "nodes");
    let name_of = |session: &str| {
        nodes.iter().find(|n| n.get("session_id").and_then(Value::as_str) == Some(session)).and_then(|n| n.get("name").or(n.get("short_id"))).cloned().unwrap_or(json!(session))
    };
    let tasks = list(o, "tasks");
    let (mut shown, mut hidden) = (vec![], 0);
    for r in list(o, "requests").iter().filter(|r| r.get("synthetic") != Some(&Value::Bool(true))) {
        if !all && r.get("state").and_then(Value::as_str) == Some("done") {
            hidden += 1;
            continue;
        }
        let id = r.get("id").and_then(Value::as_str);
        let mine: Vec<Value> = tasks
            .iter()
            .filter(|t| t.get("request").and_then(Value::as_str) == id)
            .map(|t| {
                let mut row = pick(t, &["id", "title", "state"]);
                if let Some(activity) = t.get("activity").filter(|a| !a.is_null()) {
                    row["activity"] = activity.clone();
                }
                if let Some(d) = t.get("depends_on").filter(|d| d.as_array().is_some_and(|a| !a.is_empty())) {
                    row["depends_on"] = d.clone();
                }
                for key in ["delivered_dependencies", "blocks", "blocked_by"] {
                    if let Some(list) = t.get(key).filter(|d| d.as_array().is_some_and(|a| !a.is_empty())) {
                        row[key] = list.clone();
                    }
                }
                if let Some(verdict) = t.get("verdict").filter(|v| !v.is_null()) {
                    row["verdict"] = verdict.clone();
                }
                if let Some(rounds) = t.get("review_rounds").and_then(Value::as_u64).filter(|n| *n > 0) {
                    row["review_rounds"] = json!(rounds);
                }
                if let Some(session) = t.pointer("/assignee/session_id").and_then(Value::as_str) {
                    row["node"] = name_of(session);
                }
                row
            })
            .collect();
        let mut row = pick(r, &["id", "state", "text"]);
        if let Some(text) = row.get("text").and_then(Value::as_str).filter(|t| t.chars().count() > 160) {
            row["text"] = json!(format!("{}…", text.chars().take(160).collect::<String>()));
        }
        row["tasks"] = json!(mine);
        shown.push(row);
    }
    (shown, hidden)
}

/// Which part of a node's record `node` returns.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Page {
    /// What the orchestrator acts on, with the last few distinct events.
    Recent,
    /// Every question, report and message, and a page of distinct events: the newest ones, or
    /// the ones before a sequence number.
    History { before: Option<u64> },
}

impl Page {
    /// Raw events fetched, enough for the watcher's heartbeats to collapse into `shown` distinct ones.
    fn window(self) -> usize {
        match self {
            Page::Recent => 200,
            Page::History { .. } => 400,
        }
    }

    fn shown(self) -> usize {
        match self {
            Page::Recent => 12,
            Page::History { .. } => 100,
        }
    }
}

/// Strings this long are texts (messages, prompts, details); shorter ones are values like
/// states, which repeat by nature.
const TEXT_MIN: usize = 40;
const EVENT_NOISE: &[&str] = &["session_id", "short_id", "updated_at", "last_text_at"];

fn is_text(v: &Value) -> Option<&String> {
    match v {
        Value::String(s) if s.chars().count() >= TEXT_MIN => Some(s),
        _ => None,
    }
}

fn texts(v: &Value, out: &mut HashSet<String>) {
    match v {
        Value::Array(a) => a.iter().for_each(|x| texts(x, out)),
        Value::Object(o) => o.values().for_each(|x| texts(x, out)),
        _ => {
            if let Some(s) = is_text(v) {
                out.insert(s.clone());
            }
        }
    }
}

/// Drops every text already shown, and remembers the ones it keeps.
fn drop_seen(v: &mut Value, seen: &mut HashSet<String>) {
    let repeated = |x: &Value, seen: &HashSet<String>| is_text(x).is_some_and(|s| seen.contains(s));
    match v {
        Value::Object(o) => {
            o.retain(|_, x| !repeated(x, seen));
            o.values_mut().for_each(|x| drop_seen(x, seen));
        }
        Value::Array(a) => {
            a.retain(|x| !repeated(x, seen));
            a.iter_mut().for_each(|x| drop_seen(x, seen));
        }
        _ => {
            if let Some(s) = is_text(v) {
                seen.insert(s.clone());
            }
        }
    }
}

/// The node's timeline, oldest first, each event flattened, leaving out any event equal to the
/// previous one of its type (the watcher's heartbeats).
fn distinct_events(timeline: &[Value]) -> Vec<Value> {
    let mut last: HashMap<String, Value> = HashMap::new();
    let mut out = vec![];
    for e in timeline.iter().rev() {
        let mut event = e.get("event").cloned().unwrap_or(Value::Null);
        if let Some(o) = event.as_object_mut() {
            o.retain(|k, x| !x.is_null() && !EVENT_NOISE.contains(&k.as_str()));
        }
        let kind = event.get("type").and_then(Value::as_str).unwrap_or("").to_string();
        if last.get(&kind) == Some(&event) {
            continue;
        }
        let mut row = json!({"seq": e.get("seq"), "at": e.get("at")});
        if let (Some(row), Some(fields)) = (row.as_object_mut(), event.as_object()) {
            row.extend(fields.clone());
        }
        last.insert(kind, event);
        out.push(row);
    }
    out
}

/// What the orchestrator acts on about one node: its state, its last message once, what is open
/// with the user, and its recent distinct events. A history page adds every question, report and
/// message, and pages through the events. No text appears twice in one response.
fn node_view(d: &Value, page: Page) -> Value {
    let history = page != Page::Recent;
    let mut node = pick(
        d.get("node").unwrap_or(&Value::Null),
        &["name", "short_id", "cwd", "model", "state", "state_since", "alive", "in_daemon", "is_orchestrator", "busy", "tool", "waiting_permission",
          "menu_waiting", "mission", "daemon_needs", "usage_limited", "evidence", "last_prompt", "last_prompt_at", "last_message", "last_message_at",
          "sdk_host", "task", "last_event_at"],
    );
    if let Some(b) = d.get("node").and_then(running) {
        node["background"] = b;
    }
    let questions = list(d, "questions");
    let is_open = |q: &Value| q.get("status").and_then(Value::as_str) == Some("open");
    let open: Vec<Value> = questions
        .iter()
        .filter(|q| is_open(q))
        .map(|q| pick(q, &["id", "title", "detail", "options", "links", "attachments", "blocking", "to_orchestrator", "action", "steps", "asked_at"]))
        .collect();
    let reports = list(d, "reports");
    let unread = |r: &Value| r.get("read") == Some(&Value::Bool(false));
    let shown_reports: Vec<Value> = reports
        .iter()
        .filter(|r| history || unread(r))
        .map(|r| pick(r, &["id", "title", "kind", "body", "links", "attachments", "read", "created_at"]))
        .collect();
    let messages = list(d, "messages");
    let pending = |m: &Value| m.get("handed_at").is_none_or(Value::is_null);
    let shown_messages: Vec<Value> = messages
        .iter()
        .filter(|m| history || pending(m))
        .map(|m| pick(m, &["id", "from", "text", "answers_question", "created_at", "handed_at"]))
        .collect();

    let mut out = json!({"node": node, "open_questions": open, "reports": shown_reports, "messages": shown_messages});
    if let Some(held) = d.get("messages_held_for_turn_end").filter(|h| h.as_u64().unwrap_or(0) > 0) {
        out["messages_held_for_turn_end"] = held.clone();
    }
    if history {
        out["closed_questions"] =
            Value::Array(questions.iter().filter(|q| !is_open(q)).map(|q| pick(q, &["id", "title", "status", "answer", "asked_at"])).collect());
    }

    let mut seen = HashSet::new();
    texts(&out, &mut seen);
    if let Page::History { before: Some(_) } = page {
        out = json!({"node": pick(&out["node"], &["name", "short_id"])});
    }

    let timeline = list(d, "timeline");
    let all = distinct_events(&timeline);
    let keep = page.shown().min(all.len());
    let mut events = all[all.len() - keep..].to_vec();
    events.iter_mut().for_each(|e| drop_seen(e, &mut seen));
    let earlier = (keep < all.len() || timeline.len() >= page.window()).then(|| events.first().and_then(|e| e.get("seq")).cloned()).flatten();
    out[if history { "events" } else { "recent_events" }] = Value::Array(events);

    if history {
        if let Some(seq) = earlier {
            out["earlier_events"] = json!({"see_them_with": format!("node with before: {seq}")});
        }
        return out;
    }
    let mut hidden = serde_json::Map::new();
    for (k, n) in [
        ("closed_questions", questions.len() - open.len()),
        ("read_reports", reports.iter().filter(|r| !unread(r)).count()),
        ("delivered_messages", messages.iter().filter(|m| !pending(m)).count()),
    ] {
        if n > 0 {
            hidden.insert(k.into(), json!(n));
        }
    }
    if earlier.is_some() {
        hidden.insert("earlier_events".into(), json!(true));
    }
    if !hidden.is_empty() {
        hidden.insert("see_them_with".into(), json!("node with history: true"));
        out["hidden"] = Value::Object(hidden);
    }
    out
}

pub fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

pub fn serve_node() -> Result<()> {
    let user = crate::client::config().name.clone();
    serve_server(
        "alondra-node",
        instructions(Role::Node, &user),
        tools(Role::Node, &user),
        call_node,
    )
}

pub fn serve_orchestrator() -> Result<()> {
    let user = crate::client::config().name.clone();
    serve_server(
        "alondra-orchestrator",
        instructions(Role::Orchestrator, &user),
        tools(Role::Orchestrator, &user),
        call_orchestrator,
    )
}

/// Fields Alondra supplies itself: a caller that still sends them is not refused, they are dropped.
const PROTOCOL_METADATA: [&str; 4] = ["protocol_version", "data_epoch", "command_id", "expected_revision"];

/// A call's arguments against the schema its tool publishes, and a clarification's corrected card
/// against the schema of the tool it names.
fn check_arguments(tools: &Value, name: &str, args: &Value) -> Result<(), String> {
    let schema_of = |name: &str| tools.as_array().into_iter().flatten().find(|tool| tool["name"] == name).map(|tool| &tool["inputSchema"]);
    let Some(schema) = schema_of(name) else { return Ok(()) };
    let mut args = args.clone();
    if let Some(fields) = args.as_object_mut() {
        fields.retain(|key, _| !PROTOCOL_METADATA.contains(&key.as_str()));
    }
    crate::mcp_arguments::check(name, schema, &args)?;
    if name == "answer_clarification"
        && let Some(tool) = args.pointer("/replacement/tool").and_then(Value::as_str)
        && let Some(schema) = schema_of(tool)
    {
        let mut arguments = args.pointer("/replacement/arguments").cloned().unwrap_or(json!({}));
        if let Some(fields) = arguments.as_object_mut() {
            fields.retain(|key, _| key != "replaces" && !PROTOCOL_METADATA.contains(&key.as_str()));
        }
        crate::mcp_arguments::check(tool, schema, &arguments).map_err(|problem| format!("replacement: {problem}"))?;
    }
    Ok(())
}

fn require_native_root(params: &Value, mapping: &std::path::Path) -> Result<()> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    anyhow::ensure!(mapping.is_absolute(), "The native root thread mapping is unavailable");
    let mut file=std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC).open(mapping)?;
    let metadata=file.metadata()?;
    anyhow::ensure!(metadata.is_file() && metadata.nlink()==1 && metadata.uid()==unsafe {libc::geteuid()} && metadata.mode() & 0o7777 == 0o600 && metadata.len()<=1024,"The private native root thread mapping is invalid");
    let mut bytes=Vec::new();Read::by_ref(&mut file).take(1025).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len()<=1024,"The native root thread mapping exceeds its limit");
    let native:Value=serde_json::from_slice(&bytes)?;
    let root=native["threadId"].as_str().filter(|id|!id.is_empty() && id.len()<=256).ok_or_else(||anyhow::anyhow!("The native root thread identity is unavailable"))?;
    anyhow::ensure!(params["_meta"]["threadId"].as_str()==Some(root),"Only the root Alondra node may use its business MCP tools; child agents must report to their parent");
    Ok(())
}

fn mcp_operation_identity(params: &Value, rpc: &Value, instance: &str) -> Result<String> {
    use sha2::{Digest,Sha256};
    let meta = &params["_meta"];
    let native = ["threadId","sessionId","callId"].map(|key| meta[key].as_str().filter(|value| !value.is_empty() && value.len() <= 256));
    let identity = match native {
        [Some(thread),Some(session),Some(call)] => json!({"thread":thread,"session":session,"call":call}),
        _ if std::env::var("ALONDRA_PROVIDER").ok().as_deref() == Some("codex") => anyhow::bail!("incompatible_protocol: native Codex MCP call identity is missing"),
        _ => json!({"instance":instance,"rpc":rpc}),
    };
    Ok(Sha256::digest(serde_json::to_vec(&identity)?).iter().map(|byte| format!("{byte:02x}")).collect())
}

fn serve_server(
    server_name: &str,
    instructions: String,
    tools: Value,
    dispatch: fn(&Client, &str, &Value) -> Result<String>,
) -> Result<()> {
    let client = Client::new(CALL_TIMEOUT);
    let instance = crate::client::new_id("mcp-server");
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(req) = serde_json::from_str::<Value>(&line) else { continue };
        let Some(id) = req.get("id").cloned() else { continue };
        let method = req.get("method").and_then(Value::as_str).unwrap_or("");
        let params = req.get("params").cloned().unwrap_or(Value::Null);
        let result = match method {
            "initialize" => Ok(json!({
                "protocolVersion": params.get("protocolVersion").cloned().unwrap_or(json!("2025-06-18")),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": server_name, "version": env!("CARGO_PKG_VERSION")},
                "instructions": instructions,
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tools})),
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                let checked = check_arguments(&tools, name, &args).map_err(anyhow::Error::msg);
                Ok(match checked.and_then(|()| {
                    let operation = mcp_operation_identity(&params,&id,&instance)?;
                    if std::env::var("ALONDRA_PROVIDER").ok().as_deref()==Some("codex") {
                        let mapping=std::env::var_os("ALONDRA_CODEX_THREAD_MAPPING").ok_or_else(||anyhow::anyhow!("incompatible_protocol: the host did not provide its native root thread mapping"))?;
                        require_native_root(&params,std::path::Path::new(&mapping))?;
                    }
                    dispatch(&client.with_mcp_operation(&operation),name,&args)
                }) {
                    Ok(text) => json!({"content": [{"type": "text", "text": text}]}),
                    Err(e) => json!({"content": [{"type": "text", "text": format!("Alondra error: {}", explain(&e))}], "isError": true}),
                })
            }
            _ => Err(json!({"code": -32601, "message": format!("unsupported method: {method}")})),
        };
        let resp = match result {
            Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
            Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": e}),
        };
        writeln!(stdout, "{resp}")?;
        stdout.flush()?;
    }
    Ok(())
}

/// A skill's text with its line breaks folded, so a rule wrapped across lines reads as one.
#[cfg(test)]
fn skill_text(role: Role) -> String {
    let text = match role {
        Role::Node => include_str!("../../../plugin/skills/alondra/SKILL.md"),
        Role::Orchestrator => include_str!("../../../plugin-orchestrator/skills/alondra-orchestrator/SKILL.md"),
    };
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_business_tools_require_the_private_root_mapping_and_reject_children() {
        use std::os::unix::fs::PermissionsExt;
        let directory=crate::test_support::Directory::new();std::fs::create_dir_all(&*directory).unwrap();
        let mapping=directory.join("root-thread");
        std::fs::write(&mapping,br#"{"threadId":"root-thread"}"#).unwrap();std::fs::set_permissions(&mapping,std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(require_native_root(&json!({"_meta":{"threadId":"root-thread"}}),&mapping).is_ok());
        assert!(require_native_root(&json!({"_meta":{"threadId":"child-thread"}}),&mapping).is_err());
        assert!(require_native_root(&json!({}),&mapping).is_err());
        let linked=directory.join("linked");std::os::unix::fs::symlink(&mapping,&linked).unwrap();assert!(require_native_root(&json!({"_meta":{"threadId":"root-thread"}}),&linked).is_err());
        std::fs::set_permissions(&mapping,std::fs::Permissions::from_mode(0o644)).unwrap();assert!(require_native_root(&json!({"_meta":{"threadId":"root-thread"}}),&mapping).is_err());
    }

    #[test]
    fn native_mcp_operation_uses_native_ids_across_rpc_reconnection() {
        let metadata = json!({"_meta":{"threadId":"native-thread","sessionId":"native-session","callId":"native-call"}});
        let first = mcp_operation_identity(&metadata,&json!(1),"connection-one").unwrap();
        assert_eq!(first,mcp_operation_identity(&metadata,&json!(999),"connection-two").unwrap());
        let mut another = metadata.clone(); another["_meta"]["callId"] = json!("another-call");
        assert_ne!(first,mcp_operation_identity(&another,&json!(1),"connection-one").unwrap());
        let mut another_thread = metadata; another_thread["_meta"]["threadId"] = json!("another-thread");
        assert_ne!(first,mcp_operation_identity(&another_thread,&json!(1),"connection-one").unwrap());
    }

    #[test]
    fn register_pr_normalizes_the_host_a_node_got_slightly_wrong() {
        for (given, expected) in [
            ("github", "github.com"),
            ("GitHub.com", "github.com"),
            ("https://github.com/", "github.com"),
            ("https://github.com/org/repo/pull/12", "github.com"),
            ("git.example.com", "git.example.com"),
        ] {
            let args = json!({"task_id":"task-a","host":given,"repository":"org/repo",
                "number":12,"head":"0123456789abcdef","base":"main","allowed_methods":["merge"]});
            let envelope = domain_envelope("register_pr", &args, "epoch", "register-one").unwrap();
            assert_eq!(envelope["command"]["host"], expected, "{given}");
            assert_eq!(envelope["command"]["pr_id"], format!("{expected}/org/repo#12"), "{given}");
        }
    }

    #[test]
    fn register_pr_names_a_repository_with_capitals_by_its_lowercase_spelling() {
        for repository in ["acmecorp-private/Parcel", "acmecorp-private/parcel"] {
            let args = json!({"task_id":"task-a","host":"github.com","repository":repository,
                "number":65,"head":"0123456789abcdef","base":"main","allowed_methods":["merge"]});
            let envelope = domain_envelope("register_pr", &args, "epoch", "register-one").unwrap();
            assert_eq!(envelope["command"]["repository"], "acmecorp-private/parcel", "{repository}");
            assert_eq!(envelope["command"]["pr_id"], "github.com/acmecorp-private/parcel#65", "{repository}");
        }
    }

    #[test]
    fn register_pr_tool_constructs_a_typed_command() {
        let args = json!({"data_epoch":"epoch","command_id":"register-one","expected_revision":4,
            "task_id":"task-a","pr_id":"pr-12","host":"github.com","repository":"org/repo",
            "number":12,"head":"0123456789abcdef","base":"main","allowed_methods":["merge"]});
        let envelope = domain_envelope("register_pr", &args, "epoch", "register-one").unwrap();
        assert_eq!(envelope["command"]["kind"], "register_pr");
        assert_eq!(envelope["command"]["pr_id"], "github.com/org/repo#12", "Alondra names the PR, not the node");
        assert_eq!(envelope["command"]["task_id"], "task-a");
        assert_eq!(envelope["protocol_version"], 2);
        assert!(domain_envelope("register_pr", &json!({"task_id":"task-a"}), "epoch", "register-one").is_err());
        let catalog = tools(Role::Node, "User");
        assert!(catalog.as_array().unwrap().iter().any(|tool| tool["name"] == "register_pr"));
        assert!(catalog.as_array().unwrap().iter().all(|tool| tool["name"] != "domain_command"));
        assert!(catalog.as_array().unwrap().iter().all(|tool| tool["name"] != "complete_task"));
        assert!(instructions(Role::Node, "User").contains("Alondra merges your one PR: never merge it yourself or ask User to authorize a merge"));
        assert!(instructions(Role::Node, "Ana").contains("Your terminal does not reach Ana: these tools do"));
        for role in [Role::Node, Role::Orchestrator] {
            assert!(instructions(role, "Ana").contains("and your replies in this terminal too: the apps show your last one"));
            assert!(instructions(role, "Ana").contains("Ana cannot open a file from the app"));
        }
        assert!(instructions(Role::Node, "Ana").contains("deliver with present_result and is_final true"));
        assert!(instructions(Role::Node, "Ana").contains("never install, restart or replace the Alondra daemon or CLI"));
        assert!(instructions(Role::Node, "Ana").contains("Use request_plan_mode"));
        assert!(tools(Role::Node,"Ana").as_array().unwrap().iter().any(|tool| tool["name"]=="request_plan_mode"));
        assert!(!tools(Role::Orchestrator,"Ana").as_array().unwrap().iter().any(|tool| tool["name"]=="request_plan_mode"));
        let skill = skill_text(Role::Node);
        assert!(skill.contains("A task owns exactly one PR for its whole life"));
        assert!(skill.contains("`ask` with the decision in `body` and the approval options, then at once `link_interaction_subject`"));
    }

    #[test]
    fn the_node_leaves_a_usage_limit_to_alondra_instead_of_waiting_it_out() {
        assert!(instructions(Role::Node, "Ana").contains("Never wait out a usage limit yourself: end your turn."));
        let catalog = tools(Role::Node, "Ana");
        let wait = catalog.as_array().unwrap().iter().find(|t| t["name"] == "wait_for_machine").unwrap();
        assert!(wait["description"].as_str().unwrap().contains("never wait for it with sleep, cron or a background loop either: end your turn, and Alondra wakes you when Claude answers again"));
        assert_eq!(wait["inputSchema"]["required"], json!(["resource", "reason"]));
        assert_eq!(wait["inputSchema"]["properties"]["resource"]["enum"], json!(["memory", "disk", "cpu", "build_queue"]),
            "no resource names the subscription's usage");
        assert!(skill_text(Role::Node).contains("When a turn ends on a usage limit (\"You've hit your session limit\"), never wait for it yourself: no `sleep`, no cron (`CronCreate`), no loop in the background. End your turn; Alondra waits out the limit and wakes you when Claude answers again."), "the node skill says so too");
    }

    #[test]
    fn partial_launch_plan_tools_are_orchestrator_only_and_keep_the_mission_out_of_changes() {
        let catalog = tools(Role::Orchestrator, "Ana");
        let node = tools(Role::Node, "Ana");
        for name in ["get_launch_plan", "update_launch_plan"] {
            assert!(!node.as_array().unwrap().iter().any(|tool| tool["name"] == name));
            assert!(catalog.as_array().unwrap().iter().any(|tool| tool["name"] == name));
        }
        let update = catalog.as_array().unwrap().iter().find(|tool| tool["name"] == "update_launch_plan").unwrap();
        let changes = &update["inputSchema"]["properties"]["changes"];
        assert_eq!(changes["additionalProperties"], false);
        assert_eq!(changes["required"], json!([]));
        for field in ["model", "effort", "reasoning_effort", "approach", "tags", "reason"] {
            assert!(changes["properties"][field].is_object(), "{field}");
        }
        for field in ["name", "mission", "cwd", "role", "project"] {
            assert!(changes["properties"][field].is_null(), "{field}");
        }
        assert!(skill_text(Role::Orchestrator).contains("update_launch_plan({task_id, changes: {model:"));
        assert!(skill_text(Role::Orchestrator).contains("get_launch_plan({task_id})"));
    }

    #[test]
    fn the_node_has_cleanup_done_and_the_orchestrator_has_set_launch_plan_and_no_candidates() {
        fn names(role: Role) -> HashSet<String> { tools(role, "Ana").as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_owned()).collect() }
        let (node, orchestrator) = (names(Role::Node), names(Role::Orchestrator));
        assert!(node.contains("cleanup_done") && !orchestrator.contains("cleanup_done"));
        assert!(orchestrator.contains("set_launch_plan") && !node.contains("set_launch_plan"));
        assert!(!orchestrator.contains("candidates") && !node.contains("candidates"));
        let catalog = tools(Role::Node, "Ana");
        let cleanup = catalog.as_array().unwrap().iter().find(|tool| tool["name"] == "cleanup_done").unwrap();
        assert_eq!(cleanup["inputSchema"]["required"], json!(["task_id"]));
        let orchestrator_catalog = tools(Role::Orchestrator, "Ana");
        let plan = orchestrator_catalog.as_array().unwrap().iter().find(|tool| tool["name"] == "set_launch_plan").unwrap();
        assert_eq!(plan["inputSchema"]["required"], json!(["task_id", "launch"]));
        let send = orchestrator_catalog.as_array().unwrap().iter().find(|tool| tool["name"] == "send").unwrap();
        assert!(send["inputSchema"]["properties"].get("model").is_none(), "send takes no model");
        let create = orchestrator_catalog.as_array().unwrap().iter().find(|tool| tool["name"] == "create_task").unwrap();
        assert_eq!(create["inputSchema"]["properties"]["launch"]["required"], json!(["name", "cwd"]));
        assert!(create["inputSchema"]["required"].as_array().unwrap().contains(&json!("launch")), "a task is never created without its plan");
        let (method, path, body) = command_request("cleanup_done", &json!({"task_id": "t-1"}), "epoch").unwrap();
        assert_eq!((method, path.as_str()), ("POST", "/tasks/t-1/cleanup"));
        assert_eq!(body["data_epoch"], "epoch");
        let envelope = domain_envelope("cleanup_done", &json!({"task_id": "t-1"}), "epoch", "one").unwrap();
        assert_eq!(envelope["command"]["kind"], "confirm_cleanup");
    }

    #[test]
    fn only_the_node_waits_for_an_mcp_server_and_its_wait_is_a_typed_command() {
        fn names(role: Role) -> HashSet<String> { tools(role, "Ana").as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_owned()).collect() }
        assert!(names(Role::Node).contains("wait_for_mcp") && !names(Role::Orchestrator).contains("wait_for_mcp"));
        let (method, path, body) = command_request("wait_for_mcp", &json!({"task_id": "t-1", "server": "linear"}), "epoch").unwrap();
        assert_eq!((method, path.as_str(), body["server"].as_str()), ("PUT", "/tasks/t-1/mcp-wait", Some("linear")));
        let envelope = domain_envelope("wait_for_mcp", &json!({"task_id": "t-1", "server": "linear"}), "epoch", "one").unwrap();
        assert_eq!(envelope["command"]["kind"], "wait_for_mcp");
        assert!(instructions(Role::Node, "Ana").contains("A missing MCP server: wait_for_mcp."));
        assert!(skill_text(Role::Node).contains("Never call its `mcp__<server>__authenticate`"));
    }

    #[test]
    fn only_the_node_declares_work_after_the_merge_and_a_call_without_reason_ends_it() {
        fn names(role: Role) -> HashSet<String> { tools(role, "Ana").as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_owned()).collect() }
        assert!(names(Role::Node).contains("post_merge_work") && !names(Role::Orchestrator).contains("post_merge_work"));
        let (method, path, body) = command_request("post_merge_work", &json!({"task_id": "t-1", "reason": "release.sh"}), "epoch").unwrap();
        assert_eq!((method, path.as_str(), body["reason"].as_str()), ("PUT", "/tasks/t-1/post-merge-work", Some("release.sh")));
        let envelope = domain_envelope("post_merge_work", &json!({"task_id": "t-1"}), "epoch", "one").unwrap();
        assert_eq!(envelope["command"], json!({"kind": "declare_post_merge_work", "task_id": "t-1"}));
    }

    #[test]
    fn a_wait_after_the_merge_names_tasks_and_pull_requests_alondra_resolves() {
        let args = json!({"task_id": "t-1", "reason": "publish beta 59", "waits_for": ["t-fix", "Bitomule/Alondra#1142", "https://github.com/bitomule/alondra/pull/7"]});
        let (_, _, body) = command_request("post_merge_work", &args, "epoch").unwrap();
        assert_eq!(body["waits_for"], json!([
            {"kind": "task", "task_id": "t-fix"},
            {"kind": "pull_request", "repository": "bitomule/alondra", "number": 1142},
            {"kind": "pull_request", "repository": "bitomule/alondra", "number": 7},
        ]));
        let tool = tools(Role::Node, "Ana").as_array().unwrap().iter().find(|t| t["name"] == "post_merge_work").cloned().unwrap();
        assert!(tool["description"].as_str().unwrap().contains("waits_for"), "{tool}");
    }

    #[test]
    fn create_task_carries_its_launch_plan_in_the_typed_command() {
        let args = json!({"task_id": "t-2", "request_id": "r-1", "title": "Next", "review_prs": false, "dependencies": ["t-1"], "stack": null,
            "launch": {"name": "app/next", "cwd": "/tmp", "model": "sonnet", "reason": "clear"}});
        let (method, path, body) = command_request("create_task", &args, "epoch").unwrap();
        assert_eq!((method, path.as_str()), ("POST", "/requests/r-1/tasks"));
        assert_eq!(body["launch"], json!({"name": "app/next", "cwd": "/tmp", "model": "sonnet", "reason": "clear"}));
        assert!(body.get("kind").is_none() && body["task_id"] == "t-2");
        let envelope = domain_envelope("create_task", &args, "epoch", "one").unwrap();
        assert_eq!(envelope["command"]["launch"]["name"], "app/next", "the domain sees the plan");
    }

    #[test]
    fn launch_and_every_launch_plan_take_the_tasks_tags() {
        let catalog = tools(Role::Orchestrator, "Ana");
        let tool = |name: &str| catalog.as_array().unwrap().iter().find(|tool| tool["name"] == name).cloned().unwrap();
        for tags in [tool("launch")["inputSchema"]["properties"]["tags"].clone(), tool("create_task")["inputSchema"]["properties"]["launch"]["properties"]["tags"].clone(),
            tool("set_launch_plan")["inputSchema"]["properties"]["launch"]["properties"]["tags"].clone()] {
            assert_eq!(tags["required"], json!(["kind"]));
            assert_eq!(tags["properties"]["kind"]["enum"], json!(["feature", "fix", "refactor", "tests", "docs", "migration", "design", "review", "release", "research"]));
            assert_eq!(tags["properties"]["areas"]["items"]["enum"], json!(["ui", "daemon", "data", "build"]));
            assert_eq!(tags["properties"]["risks"]["items"]["enum"], json!(["concurrency", "security", "data_migration", "unclear_spec", "user_visible"]));
        }
        let args = json!({"task_id": "t-1", "request_id": "r-1", "title": "Docs", "review_prs": false, "dependencies": [], "stack": null,
            "launch": {"name": "app/docs", "cwd": "/tmp", "tags": {"kind": "docs", "areas": ["ui"]}}});
        let envelope = domain_envelope("create_task", &args, "epoch", "one").unwrap();
        assert_eq!(envelope["command"]["launch"]["tags"], json!({"kind": "docs", "areas": ["ui"]}), "the domain keeps the plan's tags");
    }

    #[test]
    fn the_machine_answer_shows_the_available_models_table_before_the_rest() {
        let text = machine_text(json!({"available_models": "Available models: model | efforts\nclaude/sonnet | medium\n", "machine": {"memory": "fine"}})).unwrap();
        assert!(text.starts_with("Available models: model | efforts\nclaude/sonnet | medium\n\n{"), "{text}");
        assert!(!text.contains("available_models") && text.contains("\"memory\": \"fine\""), "{text}");
        let without = machine_text(json!({"available_models": null, "machine": {}})).unwrap();
        assert!(without.starts_with('{') && !without.contains("available_models"), "{without}");
    }

    /// Claude Code passes the model at most 2 KB of a server's instructions and of each tool's
    /// description, and cuts the rest with «[truncated]».
    #[test]
    fn instructions_and_every_tool_description_fit_what_claude_code_passes_on() {
        const LIMIT: usize = 2048;
        let user = "Alexandra Montgomery";
        for role in [Role::Node, Role::Orchestrator] {
            let text = instructions(role, user);
            assert!(text.len() <= LIMIT, "instructions: {} bytes", text.len());
            for tool in tools(role, user).as_array().unwrap() {
                let description = tool["description"].as_str().unwrap();
                assert!(description.len() <= LIMIT, "{}: {} bytes", tool["name"], description.len());
            }
        }
    }

    #[test]
    fn the_orchestrator_instructions_say_one_node_per_task_and_never_mention_reuse() {
        let text = instructions(Role::Orchestrator, "Ana");
        for old in ["candidates", "model_mismatch", "fits", "free node"] {
            assert!(!text.contains(old), "{old}");
        }
        assert!(text.contains("A node works on one Task for its whole life and closes with it"));
        assert!(text.contains("each with its launch plan"));
        assert!(skill_text(Role::Orchestrator).contains("`create_task` refuses a task without `launch` (`name`, `cwd`"));
        let node = skill_text(Role::Node);
        assert!(node.contains("call `cleanup_done`") && node.contains("When Alondra tells you your task was cancelled, stop"));
    }

    #[test]
    fn domain_producer_capability_has_the_existing_bearer_format() {
        assert!(validate_domain_bearer(&"a".repeat(64)).is_ok());
        for invalid in ["a".repeat(63), "A".repeat(64), "g".repeat(64), " ".repeat(64)] {
            assert!(validate_domain_bearer(&invalid).is_err());
        }
    }

    #[test]
    fn catalogs_expose_typed_commands_by_actor_without_internal_or_user_operations() {
        fn names(role: Role) -> HashSet<String> { tools(role,"User").as_array().unwrap().iter().map(|t|t["name"].as_str().unwrap().to_owned()).collect() }
        let node=names(Role::Node); let orchestrator=names(Role::Orchestrator);
        for c in [&node,&orchestrator] { for hidden in ["domain_command","register_agent","observe_agent","authorize_merge","mark_read","submit_feedback","accept_delivery","observe_pr_state","create_schedule"] { assert!(!c.contains(hidden),"{hidden}"); } }
        for n in ["register_pr","present_result","create_document","record_verdict"] { assert!(node.contains(n),"{n}"); }
        assert!(orchestrator.contains("set_relations") && !node.contains("set_relations"), "only the orchestrator relates tasks");
        assert!(!orchestrator.contains("record_verdict"), "only a node gives a verdict");
        for n in ["complete_task","claim_merge_authorization"] { assert!(!node.contains(n),"{n}: Alondra merges and completes"); }
        for n in ["create_request","close_request","create_task","assign_task","set_dependencies","link_task_document"] { assert!(orchestrator.contains(n),"{n}"); }
        for n in ["create_task","close_request","link_task_document"] { assert!(!node.contains(n),"{n}"); }
        for n in ["present_result","complete_task","register_pr","claim_merge_authorization"] { assert!(!orchestrator.contains(n),"{n}"); }
        for n in ["launch","send","interrupt","machine","daemon_log","schedule_create","tasks_in_progress","node"] { assert!(orchestrator.contains(n)); assert!(!node.contains(n)); }
        assert!(node.contains("wait_for_machine")); assert!(!orchestrator.contains("wait_for_machine"));
        let open_cards=|role| tools(role,"Ana").as_array().unwrap().iter().find(|t|t["name"]=="open_cards").cloned().expect("open_cards");
        assert_eq!(open_cards(Role::Node)["inputSchema"]["required"],json!(["task_id"]));
        assert_eq!(open_cards(Role::Orchestrator)["inputSchema"]["required"],json!([]));
        let orchestrator_catalog=tools(Role::Orchestrator,"Ana");
        let launch=orchestrator_catalog.as_array().unwrap().iter().find(|t|t["name"]=="launch").unwrap();
        assert_eq!(launch["inputSchema"]["required"],json!(["task"])); assert!(launch["inputSchema"]["properties"].get("request_id").is_none());
        assert_eq!(launch["inputSchema"]["properties"]["replace"]["type"],"boolean");
        assert!(launch["description"].as_str().unwrap().contains("replace: true"), "launch says how to move a live task to another model");
        for name in ["launch","send"] {
            let tool=orchestrator_catalog.as_array().unwrap().iter().find(|t|t["name"]==name).unwrap();
            assert_eq!(tool["inputSchema"]["properties"]["approach"]["enum"],json!(["solo","subagents","workflow"]),"{name}");
            assert_eq!(tool["inputSchema"]["properties"]["reason"]["type"],"string","{name}");
        }
        let node_catalog=tools(Role::Node,"Ana");
        let pr=node_catalog.as_array().unwrap().iter().find(|t|t["name"]=="register_pr").unwrap();
        assert_eq!(pr["inputSchema"]["properties"]["number"]["type"],"integer"); assert!(pr["inputSchema"]["properties"].get("command").is_none());
        let ask=node_catalog.as_array().unwrap().iter().find(|t|t["name"]=="ask").unwrap();
        assert_eq!(ask["inputSchema"]["properties"]["scope"]["properties"]["kind"]["const"],"task");
        assert_eq!(ask["inputSchema"]["properties"]["attachments"]["type"],"array");
        for tool in node_catalog.as_array().unwrap().iter().chain(orchestrator_catalog.as_array().unwrap()) {
            for internal in ["data_epoch", "command_id", "expected_revision"] {
                assert!(tool["inputSchema"]["properties"].get(internal).is_none(),
                    "{internal} leaked through {}", tool["name"]);
            }
        }
        let answer=node_catalog.as_array().unwrap().iter().find(|t|t["name"]=="answer_question").unwrap();
        assert_eq!(answer["inputSchema"]["properties"]["inputs"]["additionalProperties"]["oneOf"][0]["properties"]["kind"]["const"],"text");
    }

    #[test]
    fn the_tasks_in_progress_and_node_answers_read_through_the_8ccd131_summaries() {
        let answer = json!({
            "nodes": [{"session_id": "agent-a", "name": "Instance A", "short_id": "", "project": "App", "state": "working", "alive": true, "in_daemon": false}],
            "open_questions": [{"id": "q", "title": "Ship?", "node_name": "Instance A", "to_orchestrator": false, "action": false, "session_id": "agent-a"}],
            "reports": [{"id": "info", "title": "Found it", "kind": "notice", "read": false, "node_name": "Instance A", "session_id": "agent-a"}],
            "orchestrator_messages": [{"id": 3, "text": "New request", "handed_at": null}],
            "requests": [{"id": "r", "state": "working", "text": "Make the editor readable"}],
            "tasks": [{"id": "t1", "title": "Colours", "state": "running", "request": "r", "depends_on": [], "assignee": {"session_id": "agent-a"}}],
            "machine": {"cpus": 10}, "usage": null,
        });
        let summary = tasks_in_progress_summary(&answer, false);
        assert_eq!(summary["nodes"][0]["name"], "Instance A");
        assert_eq!(summary["open_questions"][0]["id"], "q");
        assert_eq!(summary["unread_reports"][0]["id"], "info");
        assert_eq!(summary["orchestrator_messages"][0]["id"], 3);
        assert_eq!(summary["requests"][0]["tasks"][0]["node"], "Instance A");
        let node = json!({"node": {"name": "Instance A", "state": "working", "alive": true},
            "questions": [{"id": "q", "title": "Ship?", "status": "open", "blocking": false}, {"id": "old", "title": "Before", "status": "answered"}],
            "reports": [{"id": "info", "title": "Found it", "kind": "notice", "read": false}],
            "messages": [{"id": 4, "from": "alondra", "text": "Answer", "handed_at": null}], "messages_held_for_turn_end": 1, "timeline": []});
        let view = node_view(&node, Page::Recent);
        assert_eq!(view["node"]["name"], "Instance A");
        assert_eq!(view["open_questions"].as_array().unwrap().len(), 1);
        assert_eq!(view["messages"][0]["id"], 4);
        assert_eq!(view["messages_held_for_turn_end"], 1);
    }

    #[test]
    fn catalogs_and_instructions_leave_merging_to_alondra() {
        for role in [Role::Node, Role::Orchestrator] {
            let catalog = tools(role, "User");
            assert!(catalog.as_array().unwrap().iter().all(|tool|
                tool["name"] != "domain_cursor" && tool["name"] != "merge_authorization"));
            let instructions = instructions(role, "User");
            assert!(!instructions.contains("domain_cursor"));
            assert!(!instructions.contains("data_epoch") && !instructions.contains("expected_revision"));
            assert!(!instructions.contains("claim_merge_authorization") && !instructions.contains("complete_task"));
            assert!(instructions.contains("never process jargon, steps Alondra takes itself"));
        }
    }

    #[test]
    fn the_domain_catalog_is_selected_before_a_bearer_handoff() {
        let catalog = tools(Role::Orchestrator, "User");
        let tools = catalog.as_array().unwrap();
        let ask = tools.iter().find(|tool| tool["name"] == "ask").unwrap();
        assert!(ask["inputSchema"]["properties"].get("interaction_id").is_some());
        assert!(ask["inputSchema"]["properties"].get("command").is_none());
        assert!(tools.iter().all(|tool| tool["name"] != "report" && tool["name"] != "define"));
    }

    #[test]
    fn each_server_dispatch_rejects_the_other_servers_mutations() {
        let client = Client::new(CALL_TIMEOUT);
        assert!(call_node(&client, "create_task", &json!({})).is_err());
        assert!(call_orchestrator(&client, "present_result", &json!({})).is_err());
        assert!(call_node(&client, "domain_command", &json!({})).is_err());
        assert!(call_orchestrator(&client, "authorize_merge", &json!({})).is_err());
    }

    #[test]
    fn nodes_are_told_to_ask_choices_before_delivering_never_inside_the_result() {
        let node = tools(Role::Node, "David");
        let present = node.as_array().unwrap().iter().find(|tool| tool["name"] == "present_result").unwrap();
        assert!(present["description"].as_str().unwrap()
            .contains("When the result needs David to choose anything, ask it with ask before delivering; never leave choices only in the content"));
        assert!(skill_text(Role::Node).contains("ask it with `ask` before you deliver: each choice a question with the option you recommend"));
    }

    #[test]
    fn named_dispatch_builds_a_strict_typed_envelope() {
        let valid = json!({"data_epoch":"epoch","command_id":"one","expected_revision":7,
            "task_id":"task-a","interaction_id":"delivery-a","body":"Done",
            "targets":[{"kind":"text","text":"Done"}],"is_final":true});
        let envelope = domain_envelope("present_result", &valid, "epoch", "one").unwrap();
        assert_eq!(envelope["protocol_version"], 2);
        assert_eq!(envelope["command"]["kind"], "present_result");
        assert_eq!(envelope["command"]["task_id"], "task-a");
        assert!(
            domain_envelope(
                "present_result",
                &json!({"data_epoch":"epoch","command_id":"x",
            "expected_revision":0,"task_id":"t","interaction_id":"i","body":"x",
            "targets":[],"is_final":true,"surprise":true}),
                "epoch",
                "x",
            )
            .is_err()
        );
        assert!(domain_envelope("domain_command", &valid, "epoch", "one").is_err());
    }

    #[test]
    fn a_final_result_carries_the_plan_s_points_with_their_evidence() {
        let mark = json!({"item":"1","evidence":[{"kind":"commit","value":"3f2a1c9"},{"kind":"test","value":"bazel test //crates/core:alondra_core_test","detail":"713 passed"}]});
        let args = |mark: Value| json!({"task_id":"task-a","interaction_id":"delivery-a","need":"Review the fix","body":"Done",
            "content":{"kind":"report","body":"Done"},"targets":[{"kind":"text","text":"Done"}],"is_final":true,"checklist":[mark]});
        let envelope = domain_envelope("present_result", &args(mark), "epoch", "one").unwrap();
        assert_eq!(envelope["command"]["checklist"][0]["evidence"][1]["detail"], "713 passed");
        assert!(domain_envelope("present_result", &args(json!({"item":"1","evidence":[{"kind":"vibes","value":"trust me"}]})), "epoch", "one").is_err());
        let catalog = tools(Role::Node, "Ana");
        let schema = catalog.as_array().unwrap().iter().find(|tool| tool["name"] == "present_result").unwrap();
        let kinds = &schema["inputSchema"]["properties"]["checklist"]["items"]["properties"]["evidence"]["items"]["properties"]["kind"]["enum"];
        assert_eq!(kinds, &json!(["commit", "test", "command", "screenshot", "pr", "file", "link"]));
    }

    #[test]
    fn a_presented_pr_may_leave_its_id_and_head_for_alondra_to_fill() {
        let args = json!({"task_id":"task-a","interaction_id":"delivery-a","need":"Review the fix","body":"Done",
            "content":{"kind":"report","body":"Done"},
            "targets":[{"kind":"pull_request"}],"is_final":true});
        let envelope = domain_envelope("present_result", &args, "epoch", "one").unwrap();
        assert_eq!(envelope["command"]["targets"][0], json!({"kind":"pull_request"}));
    }

    #[test]
    fn separate_servers_advertise_distinct_method_catalogs() {
        let node = tools(Role::Node, "User");
        let orchestrator = tools(Role::Orchestrator, "User");
        fn names(tools: &Value) -> HashSet<&str> {
            tools.as_array().unwrap().iter()
                .filter_map(|tool| tool["name"].as_str()).collect()
        }
        let node_names = names(&node);
        let orchestrator_names = names(&orchestrator);
        assert!(!node_names.contains("tasks_in_progress"));
        assert!(!node_names.contains("launch"));
        assert!(orchestrator_names.contains("tasks_in_progress"));
        assert!(orchestrator_names.contains("launch"));
    }

    /// `launch` and `send` with a task wait for a start under way, then launch: the 30 s every
    /// call had gave up first, and the orchestrator's retry made a second node.
    #[test]
    fn a_call_that_may_wait_for_a_launch_outlasts_it_and_the_one_before_it() {
        for tool in ["launch", "send"] {
            assert!(call_timeout(tool) > alondra_core::LAUNCH_TIMEOUT * 2, "{tool}: {:?}", call_timeout(tool));
        }
        assert_eq!(call_timeout("ask"), CALL_TIMEOUT);
        assert_eq!(call_timeout("create_task"), CALL_TIMEOUT);
    }

    #[test]
    fn the_configured_name_reaches_every_text_a_node_reads() {
        for role in [Role::Node, Role::Orchestrator] {
            let text = format!("{} {}", instructions(role, "Mostafa"), tools(role, "Mostafa"));
            assert!(text.contains("Mostafa") && !text.contains("David"), "{text}");
        }
    }

    const SAID: &str = "Phase 1 is merged and deployed; the TUI, the Mac and the iPhone show notices inside their PR card.";
    const ASKED: &str = "Approved, start with phase 1 and open the PR when it builds and its checks pass.";

    fn every_text(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::Array(a) => a.iter().for_each(|x| every_text(x, out)),
            Value::Object(o) => o.values().for_each(|x| every_text(x, out)),
            _ => out.extend(is_text(v).cloned()),
        }
    }

    fn assert_no_text_repeats(v: &Value) {
        let mut all = vec![];
        every_text(v, &mut all);
        let mut seen = HashSet::new();
        for t in &all {
            assert!(seen.insert(t), "repeated: {t}\n{v:#}");
        }
    }

    fn event(seq: u64, body: Value) -> Value {
        json!({"seq": seq, "at": seq * 1000, "event": body})
    }

    fn heartbeat(seq: u64) -> Value {
        event(seq, json!({"type": "daemon_seen", "session_id": "s", "short_id": "s", "state": "idle", "last_text": SAID, "last_prompt": ASKED, "updated_at": seq}))
    }

    /// A long-lived node as the daemon returns it: newest first, the watcher's heartbeats each
    /// repeating the last message, and one question, report and message of each kind.
    fn detail(timeline: Vec<Value>) -> Value {
        let mut timeline = timeline;
        timeline.reverse();
        json!({
            "node": {"name": "alondra/plan", "short_id": "s", "session_id": "s", "state": "working", "alive": true,
                     "last_message": SAID, "last_prompt": ASKED, "turn": 7},
            "questions": [
                {"id": 1, "title": "Which phase goes first, the card rules or stacked PRs?", "status": "answered", "asked_at": 1},
                {"id": 2, "title": "Test the new card on the iPhone and say whether it reads well", "status": "open", "action": true, "steps": ["Open the inbox"], "asked_at": 2}
            ],
            "reports": [
                {"id": 3, "title": "Phase 0 is done and every card now carries its node's mission", "kind": "final", "read": true, "created_at": 3},
                {"id": 4, "title": "The stacked PR support still waits on its last fix from review", "kind": "notice", "read": false, "created_at": 4}
            ],
            "messages": [
                {"id": 5, "from": "user", "text": "Start with phase 1 once the checks of the previous PR are green", "handed_at": 5},
                {"id": 6, "from": "orchestrator", "text": "Report kind final when the PR is open, with its link in links", "handed_at": null}
            ],
            "messages_held_for_turn_end": 0,
            "timeline": timeline
        })
    }

    fn long_lived() -> Vec<Value> {
        let mut t = vec![event(1, json!({"type": "hook", "event": "UserPromptSubmit", "prompt": ASKED}))];
        t.extend((2..32).map(heartbeat));
        t.push(event(32, json!({"type": "hook", "event": "Stop", "last_message": SAID})));
        t.push(event(33, json!({"type": "question_asked", "id": 2, "title": "Test the new card on the iPhone and say whether it reads well"})));
        t
    }

    #[test]
    fn node_gives_the_last_message_once_and_collapses_heartbeats() {
        let v = node_view(&detail(long_lived()), Page::Recent);
        let text = v.to_string();
        assert_eq!(text.matches(SAID).count(), 1, "{v:#}");
        assert_no_text_repeats(&v);
        let events = v["recent_events"].as_array().unwrap();
        assert_eq!(events.iter().filter(|e| e["type"] == "daemon_seen").count(), 1, "30 identical heartbeats are one event: {v:#}");
        assert_eq!(events.iter().map(|e| e["seq"].as_u64().unwrap()).collect::<Vec<_>>(), vec![1, 2, 32, 33], "oldest first");
        assert_eq!(v["open_questions"].as_array().unwrap().iter().map(|q| q["id"].as_u64().unwrap()).collect::<Vec<_>>(), vec![2]);
        assert_eq!(v["open_questions"][0]["action"], true, "actions are open questions too");
        assert_eq!(v["reports"].as_array().unwrap().len(), 1, "only the unread report");
        assert_eq!(v["messages"].as_array().unwrap().len(), 1, "only the message not yet typed in");
        assert_eq!(v["hidden"], json!({"closed_questions": 1, "read_reports": 1, "delivered_messages": 1, "see_them_with": "node with history: true"}));
    }

    #[test]
    fn node_shows_a_heartbeat_again_when_something_in_it_changes() {
        let mut t = long_lived();
        t.push(event(34, json!({"type": "daemon_seen", "state": "working", "last_text": SAID, "updated_at": 34})));
        let v = node_view(&detail(t), Page::Recent);
        let seen: Vec<&Value> = v["recent_events"].as_array().unwrap().iter().filter(|e| e["type"] == "daemon_seen").collect();
        assert_eq!(seen.len(), 2, "{v:#}");
        assert_eq!(seen[1]["state"], "working");
        assert_no_text_repeats(&v);
    }

    #[test]
    fn node_keeps_only_the_newest_distinct_events() {
        let t: Vec<Value> = (1..=40).map(|i| event(i, json!({"type": "hook", "event": "PreToolUse", "tool_summary": format!("Bash cargo test --package alondra --lib mcp::tests number {i}")}))).collect();
        let v = node_view(&detail(t), Page::Recent);
        let seqs: Vec<u64> = v["recent_events"].as_array().unwrap().iter().map(|e| e["seq"].as_u64().unwrap()).collect();
        assert_eq!(seqs, (29..=40).collect::<Vec<_>>());
        assert_eq!(v["hidden"]["earlier_events"], true);
    }

    #[test]
    fn history_reaches_every_question_report_message_and_event() {
        let v = node_view(&detail(long_lived()), Page::History { before: None });
        assert_no_text_repeats(&v);
        let ids = |k: &str| v[k].as_array().unwrap().iter().map(|x| x["id"].as_u64().unwrap()).collect::<Vec<_>>();
        assert_eq!(ids("closed_questions"), vec![1]);
        assert_eq!(ids("open_questions"), vec![2]);
        assert_eq!(ids("reports"), vec![3, 4]);
        assert_eq!(ids("messages"), vec![5, 6]);
        assert_eq!(v["events"].as_array().unwrap().len(), 4);
        assert!(v.get("hidden").is_none() && v.get("earlier_events").is_none(), "{v:#}");
    }

    #[test]
    fn history_pages_back_through_events_the_daemon_has() {
        let page = Page::History { before: None };
        let mut t: Vec<Value> = (1..=page.window() as u64)
            .map(|i| event(i, json!({"type": "hook", "event": "PreToolUse", "tool_summary": format!("Read crates/alondra/src/mcp.rs lines {i} to {}", i + 40)})))
            .collect();
        t.push(event(1000, json!({"type": "hook", "event": "Stop", "last_message": SAID})));
        let first = node_view(&detail(t.split_off(t.len() - page.window())), page);
        let cursor = first["events"][0]["seq"].as_u64().unwrap();
        assert_eq!(first["events"].as_array().unwrap().len(), page.shown());
        assert_eq!(first["earlier_events"]["see_them_with"], format!("node with before: {cursor}"));

        let older: Vec<Value> = t.into_iter().chain((2..cursor).map(|i| event(i, json!({"type": "hook", "event": "PreToolUse", "tool_summary": format!("Read crates/alondra/src/mcp.rs lines {i} to {}", i + 40)})))).filter(|e| e["seq"].as_u64().unwrap() < cursor).collect();
        let next = node_view(&detail(older), Page::History { before: Some(cursor) });
        assert_eq!(next["node"], json!({"name": "alondra/plan", "short_id": "s"}), "a later page does not repeat the node");
        assert!(next.get("reports").is_none() && next.get("closed_questions").is_none());
        assert_eq!(next["events"].as_array().unwrap().last().unwrap()["seq"], cursor - 1, "it continues right where the first page stopped");
        assert_no_text_repeats(&next);
    }

    fn overview() -> Value {
        let node = |sid: &str, state: &str, alive: bool, in_daemon: bool| json!({"session_id": sid, "name": sid, "short_id": sid, "state": state, "alive": alive, "in_daemon": in_daemon, "last_message": SAID});
        json!({
            "nodes": [
                node("running", "working", true, true),
                node("dormant", "waiting_answer", false, true),
                node("gone-owing", "unclear", false, false),
                node("gone", "unclear", false, false),
                node("gone-too", "working", false, false),
                node("finished", "done", false, true),
            ],
            "open_questions": [{"id": 9, "session_id": "gone-owing", "title": "Keep the old branch?", "detail": SAID, "status": "open"}],
            "reports": [{"id": 8, "session_id": "running", "title": "Done", "kind": "final", "read": false, "body": SAID}],
            "orchestrator_messages": [
                {"id": 1, "text": "[Alondra] Stalled node: gone (gone) ended its turn twice", "handed_at": 5},
                {"id": 2, "text": "[Alondra] Stalled node: running (running) ended its turn twice", "handed_at": null}
            ],
            "machine": {"load": [1.0, 1.0, 1.0]}
        })
    }

    #[test]
    fn tasks_in_progress_lists_the_nodes_that_matter_and_counts_the_rest() {
        let v = tasks_in_progress_summary(&overview(), false);
        let names: Vec<&str> = v["nodes"].as_array().unwrap().iter().map(|n| n["name"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["running", "dormant", "gone-owing"], "a gone node that still owes the user an answer stays");
        assert_eq!(v["hidden"], json!({"done_nodes": 1, "gone_nodes": 2, "see_them_with": "tasks_in_progress with all: true"}));
        assert_eq!(v["orchestrator_messages"].as_array().unwrap().iter().map(|m| m["id"].as_u64().unwrap()).collect::<Vec<_>>(), vec![2], "already typed in: not again");
        assert!(!v.to_string().contains(SAID), "tasks_in_progress carries titles, not bodies: {v:#}");
    }

    #[test]
    fn tasks_in_progress_with_all_lists_every_node_and_message() {
        let v = tasks_in_progress_summary(&overview(), true);
        assert_eq!(v["nodes"].as_array().unwrap().len(), 6);
        assert_eq!(v["orchestrator_messages"].as_array().unwrap().len(), 2);
        assert!(v.get("hidden").is_none());
    }

    #[test]
    fn tasks_in_progress_lists_open_requests_with_their_task_trees() {
        let o = json!({
            "nodes": [{"session_id": "s1", "name": "app/model", "state": "working", "alive": true}],
            "requests": [
                {"id": "r-1", "state": "defining", "text": "Menu by sections"},
                {"id": "r-2", "state": "in_progress", "text": "x".repeat(200)},
                {"id": "r-3", "state": "done", "text": "Old"},
                {"id": "r-s1", "state": "in_progress", "synthetic": true},
            ],
            "tasks": [
                {"id": "t-5", "request": "r-2", "title": "Model", "state": "in_review", "depends_on": [], "assignee": {"session_id": "s1"}},
                {"id": "t-6", "request": "r-2", "title": "UI", "state": "waiting_dependency", "depends_on": ["t-5"], "assignee": null},
            ]
        });
        let f = tasks_in_progress_summary(&o, false);
        assert_eq!(f["requests"][0], json!({"id": "r-1", "state": "defining", "text": "Menu by sections", "tasks": []}), "a request to define");
        assert_eq!(f["requests"][1]["tasks"], json!([
            {"id": "t-5", "title": "Model", "state": "in_review", "node": "app/model"},
            {"id": "t-6", "title": "UI", "state": "waiting_dependency", "depends_on": ["t-5"]},
        ]));
        assert_eq!(f["requests"][1]["text"].as_str().unwrap().chars().count(), 161);
        assert_eq!(f["requests"].as_array().unwrap().len(), 2, "done and made-up requests are left out");
        assert_eq!(f["hidden"], json!({"done_requests": 1, "see_them_with": "tasks_in_progress with all: true"}));
        assert_eq!(tasks_in_progress_summary(&o, true)["requests"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn tasks_in_progress_lists_what_a_node_runs_only_when_it_runs_something() {
        let o = json!({"nodes": [
            {"name": "a", "state": "working", "alive": true, "background": [{"kind": "shell", "label": "until pgrep", "started_at": 1}]},
            {"name": "b", "state": "working", "alive": true, "background": []}
        ]});
        let f = tasks_in_progress_summary(&o, false);
        assert_eq!(f["nodes"][0]["background"][0]["label"], "until pgrep");
        assert!(f["nodes"][1].get("background").is_none());
    }
}

#[cfg(test)]
mod card_tool_tests {
    use super::*;
    use alondra_core::domain::DomainError;

    fn tool(role: Role, name: &str) -> Value {
        tools(role, "Ana").as_array().unwrap().iter().find(|t| t["name"] == name)
            .unwrap_or_else(|| panic!("{name} is missing")).clone()
    }

    fn required(tool: &Value) -> Vec<String> {
        tool["inputSchema"]["required"].as_array().unwrap().iter().map(|r| r.as_str().unwrap().to_owned()).collect()
    }

    fn place() -> Value {
        json!({"interaction_id":"card-1","scope":{"kind":"task","task_id":"t-1"},"recipient":{"kind":"user"}})
    }

    fn with(extra: Value) -> Value {
        let mut args = place();
        args.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        args
    }

    fn built(name: &str, args: Value) -> Value {
        domain_envelope(name, &args, "epoch", "id").unwrap_or_else(|e| panic!("{name}: {e}"))["command"].clone()
    }

    const OPTIONS: &str = r#"[{"id":"a","label":"Atlas","inputs":[]},{"id":"b","label":"Borealis","inputs":[]}]"#;

    fn options() -> Value {
        serde_json::from_str(OPTIONS).unwrap()
    }

    #[test]
    fn each_card_tool_requires_the_words_of_its_card() {
        let table: [(&str, &[&str]); 7] = [
            ("ask", &["why", "blocking"]),
            ("request_action", &["need", "why", "verify", "blocking"]),
            ("request_input", &["need", "why", "kind", "blocking"]),
            ("request_test", &["need", "where", "what"]),
            ("present_result", &["need", "task_id", "targets", "is_final"]),
            ("withdraw", &["interaction_id"]),
            ("answer_clarification", &["id"]),
        ];
        for (name, fields) in table {
            let found = required(&tool(Role::Node, name));
            for field in fields {
                assert!(found.iter().any(|f| f == field), "{name} must require {field}: {found:?}");
            }
        }
        for name in ["ask", "request_action", "request_input", "request_test", "present_result"] {
            let properties = &tool(Role::Node, name)["inputSchema"]["properties"];
            assert_eq!(properties["need"]["maxLength"], 90, "{name}");
            assert_eq!(properties["need"]["minLength"], 1, "{name}");
        }
        let ask = tool(Role::Node, "ask");
        let properties = &ask["inputSchema"]["properties"];
        assert_eq!(properties["why"]["maxLength"], 240);
        assert_eq!(properties["content"]["required"], json!(["kind", "body"]));
        assert_eq!(properties["content"]["properties"]["summary"]["maxLength"], 60);
        for field in ["recommended", "blocking", "blocks", "replaces", "options"] {
            assert!(properties.get(field).is_some(), "ask lacks {field}");
        }
        assert!(tool(Role::Node, "request_action")["inputSchema"]["properties"].get("steps").is_some());
        assert_eq!(tool(Role::Node, "request_input")["inputSchema"]["properties"]["kind"]["enum"], json!(["file", "text", "secret"]));
    }

    #[test]
    fn the_catalogs_have_no_way_to_authorize_and_the_orchestrator_alone_raises_problems() {
        for role in [Role::Node, Role::Orchestrator] {
            for hidden in ["request_authorization", "authorize", "authorize_merge"] {
                assert!(tools(role, "Ana").as_array().unwrap().iter().all(|t| t["name"] != hidden), "{hidden}");
            }
        }
        let problem = tool(Role::Orchestrator, "raise_problem");
        assert_eq!(required(&problem), ["interaction_id", "scope", "need", "why", "options"]);
        assert!(tools(Role::Node, "Ana").as_array().unwrap().iter().all(|t| t["name"] != "raise_problem"));
        assert!(call_node(&Client::new(CALL_TIMEOUT), "raise_problem", &json!({})).is_err());
        for name in ["ask", "request_action", "request_input", "publish_information", "withdraw", "withdraw_question", "answer_clarification"] {
            tool(Role::Orchestrator, name);
        }
        assert!(tool(Role::Node, "withdraw_question")["description"].as_str().unwrap().contains("Alias of withdraw"));
    }

    #[test]
    fn each_tool_descriptions_say_when_to_use_it_and_where_long_text_goes() {
        for name in ["ask", "request_action", "request_input", "request_test", "present_result"] {
            let description = tool(Role::Node, name)["description"].as_str().unwrap().to_owned();
            assert!(description.contains("Ana"), "{name} names the user");
        }
        assert!(tool(Role::Node, "ask")["description"].as_str().unwrap().contains("Prefer a recommendation"));
        assert!(tool(Role::Node, "ask")["description"].as_str().unwrap().contains("never in why"));
        assert!(tool(Role::Node, "ask")["inputSchema"]["properties"]["why"]["description"].as_str().unwrap().contains("Long text goes in content"));
        let node = instructions(Role::Node, "Ana");
        for tool in ["request_input", "request_test", "replaces"] {
            assert!(node.contains(tool), "{tool}");
        }
        assert!(node.contains("never a permission") && node.contains("do not proceed"));
        let skill = skill_text(Role::Node);
        assert!(skill.contains("`answer_clarification`") && skill.contains("`withdraw`") && skill.contains("never in `why`"));
        let orchestrator = instructions(Role::Orchestrator, "Ana");
        assert!(orchestrator.contains("close_request (headline and answer)") && !orchestrator.contains("before close_request") && orchestrator.contains("raise_problem"));
    }

    #[test]
    fn every_card_is_decided_from_the_card_alone_never_from_the_chat() {
        let cards = [(Role::Node, "ask"), (Role::Node, "request_action"), (Role::Node, "present_result"), (Role::Orchestrator, "ask"), (Role::Orchestrator, "request_action"), (Role::Orchestrator, "publish_information")];
        for (role, name) in cards {
            let text = tool(role, name)["description"].as_str().unwrap().to_owned();
            assert!(text.contains("Every card is decided without reading your chat: Ana sees only the card"), "{text}");
            assert!(text.contains("the proposed text and the current one") && text.contains("never a summary"), "{text}");
            assert!(text.contains("Each option says what happens if it is chosen, and the recommended one why"), "{text}");
            assert!(text.contains("Never refer to the chat") && text.contains("whether someone who sees only this card can decide"), "{text}");
        }
        for role in [Role::Node, Role::Orchestrator] {
            assert!(instructions(role, "Ana").contains("Every card is decided without reading your chat: Ana sees only the card"));
            let skill = skill_text(role);
            assert!(skill.contains("Every card is decided from the card alone") && skill.contains("promotional text for Quill"));
        }
    }

    #[test]
    fn the_orchestrator_skill_chooses_models_from_the_table_and_never_by_brand() {
        let skill = skill_text(Role::Orchestrator);
        for rule in ["Give every task `tags` on `launch` and in every launch plan", "`machine` starts with the Available models table",
            "or one the request text names (\"use Codex\"), is the model", "Calling a model worse needs at least 5 tasks of that kind for both",
            "A task tagged `concurrency`, `security` or `data_migration` takes the model with the best measured results for its kind",
            "Never by brand reputation", "`reason` cites the row"] {
            assert!(skill.contains(rule), "{rule}");
        }
        let lower = skill.to_lowercase();
        for brand_rule in ["opus, effort", ", sonnet)", "one sonnet task", "alondra models"] {
            assert!(!lower.contains(brand_rule), "{brand_rule}");
        }
    }

    #[test]
    fn plans_and_launches_name_the_model_with_its_provider() {
        let skill = skill_text(Role::Orchestrator);
        assert!(skill.contains("Name the model with its provider in every launch plan and `launch`: `claude/opus`, `codex/gpt-…`"), "the skill says how a plan names its model");
        for model in [tool(Role::Orchestrator, "launch")["inputSchema"]["properties"]["model"]["description"].clone(),
            tool(Role::Orchestrator, "create_task")["inputSchema"]["properties"]["launch"]["properties"]["model"]["description"].clone(),
            tool(Role::Orchestrator, "set_launch_plan")["inputSchema"]["properties"]["launch"]["properties"]["model"]["description"].clone()] {
            let text = model.as_str().unwrap();
            assert!(text.contains("claude/opus") && text.contains("refused when both providers offer it"), "{text}");
        }
    }

    #[test]
    fn a_notice_says_what_it_means_for_the_user_and_never_answers_claude_or_maintenance() {
        let notice = tool(Role::Orchestrator, "publish_information")["description"].as_str().unwrap().to_owned();
        assert!(notice.contains("only when knowing it helps Ana") && notice.contains("never script names, task ids or internal steps"), "{notice}");
        assert!(notice.contains("An answer to a message from Claude or a maintenance message never becomes a notice for Ana"), "{notice}");
        let result = tool(Role::Node, "present_result")["description"].as_str().unwrap().to_owned();
        assert!(result.contains("what the result means for Ana in plain words, never script names, task ids or internal steps"), "{result}");
    }

    #[test]
    fn a_node_has_no_notice_tool_and_is_told_what_to_use_instead() {
        assert!(tools(Role::Node, "Ana").as_array().unwrap().iter().all(|t| t["name"] != "publish_information"));
        tool(Role::Orchestrator, "publish_information");
        let client = Client::new(CALL_TIMEOUT);
        let refused = call_node(&client, "publish_information", &with(json!({"need":"Done","body":"All green"}))).unwrap_err().to_string();
        assert!(refused.starts_with("forbidden: Nodes don't send notices."), "{refused}");
        assert!(refused.contains("use ask") && refused.contains("present_result"), "{refused}");
        for role in [Role::Node, Role::Orchestrator] {
            assert!(tools(role, "Ana").as_array().unwrap().iter().all(|t| t["name"] != "propose"));
        }
        for refused in [call_node(&client, "propose", &json!({})), call_orchestrator(&client, "propose", &json!({}))] {
            let refused = refused.unwrap_err().to_string();
            assert!(refused.starts_with("forbidden: Nodes don't send notices.") && refused.contains("use ask"), "{refused}");
        }
        let text = call_node(&client, "answer_clarification", &json!({"id":"card-1","text":"Because"})).unwrap_err().to_string();
        assert!(text.starts_with("forbidden: Nodes don't send notices."), "{text}");
        let node = tool(Role::Node, "answer_clarification");
        assert!(node["inputSchema"]["properties"].get("text").is_none());
        assert!(tool(Role::Orchestrator, "answer_clarification")["inputSchema"]["properties"].get("text").is_some());
    }

    #[test]
    fn a_notice_or_result_sent_with_only_a_body_carries_it_as_the_cards_content() {
        let body = "Ejecútalo una vez en el terminal del Mac:\n\n```\ncp ~/.claude.json ~/.claude.json.bak && python3 - <<'PY'\nprint(\"Confiados\")\nPY\n```\n\nSi clonas repos nuevos, vuelve a ejecutarlo.";
        let notice = built("publish_information", with(json!({"need":"Pega este comando en el terminal","body":body})));
        assert_eq!(notice["card"]["content"], json!({"kind":"report","body":body}));
        assert_eq!(notice["body"], body);
        let result = built("present_result", json!({"task_id":"t-1","interaction_id":"card-1","need":"Read the answer","targets":[{"kind":"text","text":"done"}],"is_final":true,"body":body}));
        assert_eq!(result["card"]["content"], json!({"kind":"report","body":body}));
        let headline_only = built("publish_information", with(json!({"need":"The build is green","body":"The build is green"})));
        assert!(headline_only["card"].get("content").is_none(), "a body that repeats the headline adds nothing");
        let own = built("publish_information", with(json!({"need":"Run it","body":body,"content":{"kind":"command","body":"ls"}})));
        assert_eq!(own["card"]["content"], json!({"kind":"command","body":"ls"}), "the node's own content wins");
    }

    #[test]
    fn each_card_tool_builds_the_card_of_its_kind() {
        let cases = [
            ("ask", with(json!({"need":"Pick a name","why":"The release needs one","options":options(),"blocking":true})), "ask_question", "decide"),
            ("request_action", with(json!({"need":"Rotate the key","why":"It leaked","verify":"I call the API","blocking":true})), "request_action", "do"),
            ("request_input", with(json!({"need":"Send the API key","why":"The build needs it","kind":"secret","destination":{"file":".env","variable":"API_KEY"},"blocking":true})), "ask_question", "provide"),
            ("request_test", with(json!({"need":"Try the login","where":"Build 41","what":"Sign in twice"})), "ask_question", "test"),
            ("publish_information", with(json!({"need":"The build is green","body":"All 400 tests pass"})), "publish_information", "know"),
            ("present_result", json!({"task_id":"t-1","interaction_id":"card-1","need":"Review the parser","targets":[{"kind":"text","text":"done"}],"is_final":true,"content":{"kind":"report","body":"Done"}}), "present_result", "review"),
            ("present_result", json!({"task_id":"t-1","interaction_id":"card-1","need":"Phase 1 is in","targets":[{"kind":"text","text":"done"}],"is_final":false}), "present_result", "know"),
        ];
        for (name, args, command_kind, card_kind) in cases {
            let command = built(name, args);
            assert_eq!(command["kind"], command_kind, "{name}");
            assert_eq!(command["card"]["kind"], card_kind, "{name}");
            for flat in ["need", "why", "where", "what", "size", "steps", "verify"] {
                assert!(command.get(flat).is_none(), "{name} leaks {flat} outside the card");
            }
            let envelope = domain_envelope(name, &with(json!({})), "epoch", "id");
            assert!(envelope.is_err(), "{name} without its fields must not build");
        }
    }

    #[test]
    fn a_problem_carries_its_cause_on_the_card() {
        let command = built("raise_problem", json!({
            "interaction_id":"card-1","scope":{"kind":"task","task_id":"t-1"},
            "need":"Launch of the parser failed","why":"Nothing starts the task","options":options(),"cause":"launch:t-12"
        }));
        assert_eq!(command["card"]["cause"], "launch:t-12");
        assert!(command.get("cause").is_none());
    }

    #[test]
    fn a_card_carries_what_the_node_said_and_nothing_invented() {
        let command = built("ask", with(json!({
            "need":"Pick a name","why":"The release needs one","options":options(),"blocking":false,
            "recommended":"a","replaces":"old-card","blocks":[{"kind":"pull_request","pr_id":"pr-1"}],
            "content":{"kind":"options","summary":"2 names","body":"Atlas is shorter"}
        })));
        let card = &command["card"];
        assert_eq!((card["need"].as_str(), card["why"].as_str(), card["recommended"].as_str(), card["replaces"].as_str()),
            (Some("Pick a name"), Some("The release needs one"), Some("a"), Some("old-card")));
        assert_eq!(card["blocks"][0]["pr_id"], "pr-1");
        assert_eq!(card["content"]["summary"], "2 names");
        assert_eq!(command["body"], "Atlas is shorter", "the long part is the body, not a derivation of it");

        let without = built("ask", with(json!({"options":options(),"blocking":false,"body":"A long text\n\nthat used to become the need"})));
        assert_eq!((without["card"]["need"].as_str(), without["card"]["why"].as_str()), (Some(""), Some("")),
            "a missing need is not read from the body: alondrad refuses it with the field and the fix");
        assert!(without["card"].get("content").is_none());

        let parsed: alondra_core::domain::CommandEnvelope = serde_json::from_value(
            domain_envelope("ask", &with(json!({"need":"Pick a name","why":"Why","options":options(),"blocking":false})), "epoch", "id").unwrap(),
        ).unwrap();
        assert!(matches!(parsed.command, alondra_core::domain::Command::AskQuestion { .. }));
    }

    #[test]
    fn an_ask_with_several_questions_is_one_group_of_questions_each_with_its_own_options() {
        let args = with(json!({
            "why":"The release needs all three","blocking":true,
            "questions":[
                {"question":"Which name?","options":options(),"recommended":"a","why":"It goes on the tag"},
                {"question":"Ship it today?","options":[{"id":"y","label":"Yes (Recommended)","inputs":[]},{"id":"n","label":"No","inputs":[]}]},
            ]
        }));
        let envelope = domain_envelope("ask", &args, "epoch", "id").unwrap();
        assert_eq!(envelope["command"]["kind"], "ask_questions");
        assert_eq!(resource_target("ask", &args).unwrap(), ("POST", "/question-groups".to_owned()));
        let command = &envelope["command"];
        assert!(command.get("card").is_none() && command.get("options").is_none(), "no card of its own");
        let questions = command["questions"].as_array().unwrap();
        assert_eq!((questions[0]["need"].as_str(), questions[0]["why"].as_str(), questions[0]["recommended"].as_str()),
            (Some("Which name?"), Some("It goes on the tag"), Some("a")));
        assert_eq!(questions[1]["options"][0]["label"], "Yes", "the mark is taken off the label");
        assert_eq!(questions[1]["recommended"], "y", "and becomes the recommendation");
        let parsed: alondra_core::domain::CommandEnvelope = serde_json::from_value(envelope).unwrap();
        let alondra_core::domain::Command::AskQuestions { questions, blocking, .. } = parsed.command else { panic!("ask questions") };
        assert_eq!((questions.len(), blocking), (2, true));
    }

    #[test]
    fn a_suggestion_is_a_question_for_the_orchestrator_with_its_fixed_answers() {
        assert!(!required(&tool(Role::Node, "suggest")).contains(&"recipient".to_owned()));
        assert!(tools(Role::Orchestrator, "Ana").as_array().unwrap().iter().all(|t| t["name"] != "suggest"));
        let built = built("suggest", with(json!({"what":"The README names the old flag","where":"README.md:12",
            "why_not_mine":"This task only touches the CLI","detail":"Seen while reading the docs"})));
        assert_eq!(domain_envelope("suggest", &with(json!({"what":"w","where":"x","why_not_mine":"y"})), "epoch", "id").unwrap()["command"]["kind"], "ask_question");
        assert_eq!(built["recipient"], json!({"kind":"orchestrator"}), "never the user, whatever the node sent");
        assert_eq!(built["blocking"], false);
        assert_eq!(built["card"]["kind"], "propose");
        assert_eq!(built["card"]["need"], "The README names the old flag");
        assert_eq!(built["card"]["why"], "This task only touches the CLI");
        assert_eq!(built["card"]["content"]["body"], "Where: README.md:12\n\nSeen while reading the docs");
        let ids: Vec<&str> = built["options"].as_array().unwrap().iter().map(|o| o["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["yours", "discarded", "new_task"]);
        let missing = domain_envelope("suggest", &with(json!({"what":"w","why_not_mine":"y"})), "epoch", "id").unwrap_err().to_string();
        assert!(missing.contains("what, where and why_not_mine are required"), "{missing}");
    }

    #[test]
    fn the_orchestrator_settles_a_suggestion_with_one_typed_decision() {
        assert!(tools(Role::Orchestrator, "Ana").as_array().unwrap().iter().any(|t| t["name"] == "settle_suggestion"));
        assert!(tools(Role::Node, "Ana").as_array().unwrap().iter().all(|t| t["name"] != "settle_suggestion"));
        let args = json!({"interaction_id":"p-1","decision":{"kind":"new_task","task_id":"t-readme","request_id":"r-readme",
            "title":"Update the README flag","launch":{"name":"readme","cwd":"/tmp"}}});
        let (method, path, body) = command_request("settle_suggestion", &args, "epoch").unwrap();
        assert_eq!((method, path.as_str()), ("POST", "/interactions/p-1/settlement"));
        assert_eq!(body["decision"]["task_id"], "t-readme");
        let parsed: alondra_core::domain::CommandEnvelope = serde_json::from_value(domain_envelope("settle_suggestion", &args, "epoch", "id").unwrap()).unwrap();
        assert!(matches!(parsed.command, alondra_core::domain::Command::SettleSuggestion {
            decision: alondra_core::domain::SuggestionDecision::NewTask { .. }, .. }));
        let words = json!({"interaction_id":"p-1","decision":{"kind":"later","text":"It goes as a separate task"}});
        assert!(domain_envelope("settle_suggestion", &words, "epoch", "id").unwrap_err().to_string().starts_with("invalid_input"));
        let reasonless = json!({"interaction_id":"p-1","decision":{"kind":"discard"}});
        assert!(domain_envelope("settle_suggestion", &reasonless, "epoch", "id").is_err());
    }

    #[test]
    fn the_orchestrator_settles_a_held_plan_with_one_typed_outcome() {
        assert!(tools(Role::Orchestrator, "Ana").as_array().unwrap().iter().any(|t| t["name"] == "settle_plan_review"));
        assert!(tools(Role::Node, "Ana").as_array().unwrap().iter().all(|t| t["name"] != "settle_plan_review"));
        let args = json!({"task_id":"t-c","review_id":"q-1","outcome":{"kind":"plan_changed","launch":{"name":"phase-c","cwd":"/tmp","mission":"Use 512 px prints"},
            "reason":"The user chose 512 px prints"}});
        let (method, path, body) = command_request("settle_plan_review", &args, "epoch").unwrap();
        assert_eq!((method, path.as_str()), ("POST", "/tasks/t-c/plan-review"));
        assert_eq!(body["outcome"]["launch"]["mission"], "Use 512 px prints");
        let parsed: alondra_core::domain::CommandEnvelope = serde_json::from_value(domain_envelope("settle_plan_review", &args, "epoch", "id").unwrap()).unwrap();
        assert!(matches!(parsed.command, alondra_core::domain::Command::SettlePlanReview {
            outcome: alondra_core::domain::PlanReviewOutcome::PlanChanged { .. }, .. }));
        let reasonless = json!({"task_id":"t-c","review_id":"q-1","outcome":{"kind":"unchanged"}});
        assert!(domain_envelope("settle_plan_review", &reasonless, "epoch", "id").is_err());
        let kept = json!({"task_id":"t-c","review_id":"q-1","outcome":{"kind":"later","reason":"Ask the user"}});
        assert!(domain_envelope("settle_plan_review", &kept, "epoch", "id").unwrap_err().to_string().starts_with("invalid_input"));
    }

    #[test]
    fn the_orchestrator_settles_a_stopped_node_with_one_closed_outcome() {
        assert!(tools(Role::Orchestrator, "Ana").as_array().unwrap().iter().any(|t| t["name"] == "settle_stall"));
        assert!(tools(Role::Node, "Ana").as_array().unwrap().iter().all(|t| t["name"] != "settle_stall"));
        let args = json!({"case_id":"stalled-t-1-turn-2","task_id":"t","outcome":{"kind":"instruct","text":"Present PR #12"}});
        let (method, path, body) = command_request("settle_stall", &args, "epoch").unwrap();
        assert_eq!((method, path.as_str()), ("POST", "/tasks/t/stall-settlement"));
        assert_eq!(body["outcome"]["text"], "Present PR #12");
        let parsed: alondra_core::domain::CommandEnvelope = serde_json::from_value(domain_envelope("settle_stall", &args, "epoch", "id").unwrap()).unwrap();
        assert!(matches!(parsed.command, alondra_core::domain::Command::SettleStall {
            outcome: alondra_core::domain::StallOutcome::Instruct { .. }, .. }));
        let words = json!({"case_id":"stalled-t-1-turn-2","task_id":"t","outcome":{"kind":"ask_the_user","text":"What next?"}});
        assert!(domain_envelope("settle_stall", &words, "epoch", "id").unwrap_err().to_string().starts_with("invalid_input"), "no free-text outcome");
        let resultless = json!({"case_id":"stalled-t-1-turn-2","task_id":"t","outcome":{"kind":"close"}});
        assert!(domain_envelope("settle_stall", &resultless, "epoch", "id").is_err());
    }

    #[test]
    fn one_question_in_questions_is_the_single_decision_and_a_label_never_carries_the_recommendation() {
        let args = with(json!({"need":"Pick","why":"Why","blocking":true,
            "questions":[{"question":"Which name?","options":options(),"recommended":"b"}]}));
        let folded = built("ask", args.clone());
        assert_eq!((folded["card"]["recommended"].as_str(), folded["options"].as_array().map(Vec::len)), (Some("b"), Some(2)));
        assert_eq!(domain_envelope("ask", &args, "epoch", "id").unwrap()["command"]["kind"], "ask_question");
        let marked = built("ask", with(json!({"need":"Pick","why":"Why","blocking":true,
            "options":[{"id":"a","label":"Atlas (Recommended)","inputs":[]},{"id":"b","label":"Borealis","inputs":[]}]})));
        assert_eq!(marked["options"][0]["label"], "Atlas");
        assert_eq!(marked["card"]["recommended"], "a");
        let chosen = built("ask", with(json!({"need":"Pick","why":"Why","blocking":true,"recommended":"b",
            "options":[{"id":"a","label":"Atlas (Recommended)","inputs":[]},{"id":"b","label":"Borealis","inputs":[]}]})));
        assert_eq!(chosen["card"]["recommended"], "b", "the node's own choice wins");
        let two = || json!([{"question":"A?","options":options()},{"question":"B?","options":options()}]);
        for bad in [
            json!({"why":"w","blocking":true,"options":options(),"questions":two()}),
            json!({"why":"w","blocking":true,"questions":"A?"}),
            json!({"why":"w","blocking":true,"questions":[{"question":"A?","options":options(),"hint":"x"},{"question":"B?","options":options()}]}),
            json!({"why":"w","blocking":true,"replaces":"old","questions":two()}),
        ] {
            assert!(domain_envelope("ask", &with(bad.clone()), "epoch", "id").is_err(), "{bad}");
        }
        let schema = tool(Role::Node, "ask")["inputSchema"]["properties"]["questions"].clone();
        assert_eq!((schema["maxItems"].as_u64(), schema["items"]["required"].clone()), (Some(4), json!(["question", "options"])));
    }

    #[test]
    fn the_fixed_choices_of_the_card_tools() {
        let ids = |command: &Value| command["options"].as_array().unwrap().iter().map(|o| o["id"].as_str().unwrap().to_owned()).collect::<Vec<_>>();
        let test = built("request_test", with(json!({"need":"Try it","where":"Build 41","what":"Sign in"})));
        assert_eq!(ids(&test), ["works", "fails"]);
        assert_eq!(test["blocking"], false);
        assert_eq!(test["card"]["content"]["body"], "Where: Build 41\nWhat to look at: Sign in");
        let input = built("request_input", with(json!({"need":"Send the key","why":"Needed","kind":"file","blocking":true})));
        assert_eq!(ids(&input), ["skip"], "the value comes as the answer written on the card");
        assert!(input["card"].get("secret_destination").is_none());
        let secret = built("request_input", with(json!({"need":"Send the key","why":"Needed","kind":"secret","blocking":true,"destination":{"file":".env","variable":"STRIPE_KEY"}})));
        assert_eq!(secret["card"]["secret_destination"], json!({"file":".env","variable":"STRIPE_KEY"}));
        let action = built("request_action", with(json!({"need":"Rotate","why":"Leaked","verify":"I call it","blocking":false,"steps":["Open the console","Rotate it"]})));
        assert_eq!(ids(&action), ["done", "cannot"]);
        assert_eq!(action["card"]["content"], json!({"kind":"steps","body":"1. Open the console\n2. Rotate it"}));
        let alone = built("request_action", with(json!({"need":"Rotate","why":"Leaked","blocking":false,"content":{"kind":"report","body":"The key leaked in the log."}})));
        assert_eq!(alone["card"]["content"], json!({"kind":"report","body":"The key leaked in the log."}));
        let both = built("request_action", with(json!({"need":"Open the ports","why":"Holepunch test","blocking":false,
            "content":{"kind":"report","body":"Relay at 203.0.113.7\n"},"steps":["Open UDP 4433","Open UDP 4434"]})));
        assert_eq!(both["card"]["content"], json!({"kind":"report","body":"Relay at 203.0.113.7\n\n1. Open UDP 4433\n2. Open UDP 4434"}),
            "steps sent beside content are never dropped");
        let command = built("request_action", with(json!({"need":"Run it","why":"Probe","blocking":false,
            "content":{"kind":"command","body":"nc -u 203.0.113.7 4433"},"steps":["Run it on the Mac"]})));
        assert_eq!(command["card"]["content"], json!({"kind":"report","body":"```\nnc -u 203.0.113.7 4433\n```\n\n1. Run it on the Mac"}),
            "a command keeps its fence when steps follow it");
        assert_eq!(action["card"]["verify"], "I call it");
        for card in [&test, &input, &action] {
            assert!(card["options"].as_array().unwrap().iter().all(|option| option.get("inputs").is_none()),
                "an option is a button that answers when pressed: {card}");
        }
    }

    #[test]
    fn a_card_carries_the_markdown_files_it_lists_and_never_reads_a_mention() {
        let pointer = "Plan completo: docs/fast-connection-plan.md (PR #975, sin fusionar).\n\nPASOS:\n0. Medir.";
        let mentioned = built("ask", with(json!({"need":"Aprueba el plan de conexión rápida","why":"Son 8 tareas","options":options(),"blocking":false,
            "content":{"kind":"document","body":pointer}})));
        assert_eq!(mentioned["card"]["content"]["body"], pointer, "a mention is never read nor refused");
        assert!(mentioned["card"].get("files").is_none());
        let refused = domain_envelope("ask", &with(json!({"need":"Aprueba el plan","why":"Son 8 tareas","options":options(),"blocking":false,
            "files":["docs/fast-connection-plan.md"]})), "epoch", "id").unwrap_err().to_string();
        assert!(refused.starts_with("invalid_input: files: docs/fast-connection-plan.md cannot be read"), "{refused}");
        let outside = domain_envelope("ask", &with(json!({"need":"Aprueba","why":"x","options":options(),"blocking":false,"files":["/etc/a.md"]})), "epoch", "id").unwrap_err().to_string();
        assert!(outside.contains("not a Markdown file of your worktree"), "{outside}");
        let worktree = tempfile::tempdir().unwrap();
        std::fs::create_dir(worktree.path().join("docs")).unwrap();
        std::fs::write(worktree.path().join("docs/fast-connection-plan.md"), "# Plan\n\n- [ ] Medir").unwrap();
        std::fs::write(worktree.path().join("notes.md"), "Notas").unwrap();
        let files = vec!["notes.md".to_owned(), "docs/fast-connection-plan.md".to_owned()];
        assert_eq!(with_files(pointer, &files, Some(worktree.path())).unwrap(),
            format!("{pointer}\n\n---\n\n## notes.md\n\nNotas\n\n---\n\n## docs/fast-connection-plan.md\n\n### Plan\n\n- ☐ Medir"));
        assert_eq!(with_files("", &files[..1], Some(worktree.path())).unwrap(), "---\n\n## notes.md\n\nNotas");
        assert_eq!(with_files("Nada que incluir", &[], Some(worktree.path())).unwrap(), "Nada que incluir");
        for name in ["ask", "request_action", "request_input", "request_test", "present_result"] {
            let files = &tool(Role::Node, name)["inputSchema"]["properties"]["files"];
            assert!(files["description"].as_str().unwrap().contains("a path you mention in need, why, content or plan is never read"), "{name}: {files}");
        }
    }

    #[test]
    fn a_node_is_told_that_finishing_is_present_result_with_the_whole_report_never_an_ask() {
        let node = instructions(Role::Node, "Ana");
        assert!(node.contains("Use ask only for a real decision whose options change what you do next"), "{node}");
        let skill = skill_text(Role::Node);
        assert!(skill.contains("It never announces that your work is ready, never shows a report or a plan for the user to read, and never points at a file"));
        assert!(skill.contains("written out, never a path to the file that holds it"));
        assert!(skill.contains("An `ask` that says the work is done is not a delivery"));
        let ask = tool(Role::Node, "ask")["description"].as_str().unwrap().to_owned();
        assert!(ask.contains("never to say your work is ready, to show a report or plan to read, or to point at a file; finished work goes out with present_result"), "{ask}");
        let result = tool(Role::Node, "present_result")["description"].as_str().unwrap().to_owned();
        assert!(result.contains("Finishing is always present_result, never an ask."), "{result}");
        assert!(result.contains("written out in its body, never a path or a pasted document: Ana cannot open files from the app"), "{result}");
        let content = tool(Role::Node, "present_result")["inputSchema"]["properties"]["content"]["description"].as_str().unwrap_or_default().to_owned();
        assert!(content.contains("A separate document (a plan, a design, a spec) is never pasted here"), "{content}");
        assert!(skill.contains("a document it goes with (a plan, a design, a spec) is never pasted"));
    }

    #[test]
    fn content_a_node_calls_a_plan_is_a_report_whatever_its_body() {
        let kind_of = |body: &str| built("ask", with(json!({"need":"Fix the badge","why":"Stuck","options":options(),"blocking":false,"content":{"kind":"plan","body":body}})))["card"]["content"]["kind"].clone();
        assert_eq!(kind_of("- Relay: add a push type\n- Daemon: send it\n- App: lower the badge"), "report");
        assert_eq!(kind_of("1. a\n2. b\nNotas"), "report");
        assert_eq!(kind_of("1. Relay\n2. Daemon"), "report");
        assert_eq!(kind_of("Today the push raises the number, nothing lowers it."), "report");
        let asked = built("ask", with(json!({"need":"Pick","why":"Why","options":options(),"blocking":false,"content":{"kind":"plan","body":"Prose only"}})));
        assert_eq!(asked["card"]["content"]["kind"], "report");
        let kept = built("ask", with(json!({"need":"Fix","why":"Stuck","options":options(),"blocking":false,"content":{"kind":"diagnosis","body":"- a"}})));
        assert_eq!(kept["card"]["content"]["kind"], "diagnosis");
        assert!(!content_schema()["properties"]["kind"]["enum"].as_array().unwrap().contains(&json!("plan")));
    }

    #[test]
    fn arguments_that_cannot_make_a_card_are_refused_before_anything_is_sent() {
        for (name, args, said) in [
            ("request_input", with(json!({"need":"n","why":"w","kind":"photo","blocking":true})), "kind must be file, text or secret"),
            ("request_input", with(json!({"need":"n","why":"w","kind":"secret","blocking":true})), "destination is required for kind secret"),
            ("request_input", with(json!({"need":"n","why":"w","kind":"text","blocking":true,"destination":{"file":".env","variable":"K"}})), "destination goes only with kind secret"),
            ("request_test", with(json!({"need":"n"})), "where and what are required"),
            ("ask", with(json!({"need":"n","why":"w","options":options(),"blocking":false,"surprise":1})), "surprise"),
            ("ask", json!({"need":"n"}), "interaction_id"),
        ] {
            let error = domain_envelope(name, &args, "epoch", "id").unwrap_err().to_string();
            assert!(error.starts_with("invalid_input") && error.contains(said), "{name}: {error}");
        }
    }

    #[test]
    fn withdraw_is_one_command_under_both_names() {
        for name in ["withdraw", "withdraw_question"] {
            let envelope = domain_envelope(name, &json!({"interaction_id":"card-1"}), "epoch", "id").unwrap();
            assert_eq!(envelope["command"]["kind"], "withdraw");
            assert_eq!(resource_target(name, &envelope["command"]).unwrap(), ("POST", "/interactions/card-1/withdrawal".to_owned()));
        }
        assert_eq!(resource_target("raise_problem", &json!({})).unwrap().1, "/problems");
        assert_eq!(resource_target("request_input", &json!({})).unwrap().1, "/questions");
    }

    #[test]
    fn a_native_call_reconnection_keeps_its_durable_command_identity() {
        let args=json!({"task_id":"t-1"});
        let (_,_,first)=scoped_command_request("cleanup_done",&args,"epoch",Some("ab12")).unwrap();
        let (_,_,reconnected)=scoped_command_request("cleanup_done",&args,"epoch",Some("ab12")).unwrap();
        let (_,_,other)=scoped_command_request("cleanup_done",&args,"epoch",Some("cd34")).unwrap();
        assert_eq!(first,reconnected);
        assert_ne!(first["command_id"],other["command_id"]);
    }

    #[test]
    fn a_retried_card_keeps_its_command_id_and_another_card_does_not() {
        let ask = |id: &str| with(json!({"interaction_id":id}));
        assert_eq!(command_id("ask", &ask("card-1")), command_id("ask", &ask("card-1")));
        assert_ne!(command_id("ask", &ask("card-1")), command_id("ask", &ask("card-2")));
        assert_ne!(command_id("ask", &ask("card-1")), command_id("request_input", &ask("card-1")));
        assert_ne!(command_id("start_task", &json!({"task_id":"t"})), command_id("start_task", &json!({"task_id":"t"})),
            "only a card has an id of its own to retry by");
        assert_ne!(command_id("ask", &ask(&"x".repeat(300))), command_id("ask", &ask(&"x".repeat(300))));
    }

    #[test]
    fn a_refusal_says_what_where_and_how_and_is_never_a_permission() {
        let mut refused = DomainError::new("invalid_input", "card.why: At most 240 characters: long text goes in card.content.body, never in why");
        refused.subject = Some("card.why".into());
        let text = explain(&anyhow::Error::new(crate::client::Refused(refused)));
        assert!(text.contains("invalid_input") && text.contains("card.why") && text.contains("long text goes in card.content.body"), "{text}");
        assert!(text.contains("never an answer or a permission") && text.contains("same interaction_id"), "{text}");
        let conflict = explain(&anyhow::Error::new(crate::client::Refused(DomainError::new("idempotency_conflict", "request ID reused"))));
        assert!(conflict.contains("new interaction_id") && conflict.contains("replaces"), "{conflict}");
    }

    #[test]
    fn a_daemon_that_does_not_answer_is_never_a_success() {
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };
        let client = Client::at(format!("http://127.0.0.1:{port}"));
        let args = with(json!({"need":"Pick a name","why":"Why","options":options(),"blocking":true}));
        let error = submit(&client, &"a".repeat(64), "ask", &args).unwrap_err();
        let text = explain(&error);
        assert!(text.starts_with("don't proceed: could not reach Alondra;") && text.contains("unconfirmed") && text.contains("durable outcome") && !text.contains("Nothing was sent"), "{text}");
        assert!(text.contains("never blindly") && text.contains("permission") && !text.contains("Nothing was sent"), "{text}");
    }

    #[test]
    fn a_clarification_is_settled_by_a_replacement_or_by_words_never_both_or_neither() {
        let client = Client::new(CALL_TIMEOUT);
        for (args, said) in [
            (json!({}), "id is required"),
            (json!({"id":"card-1"}), "exactly one of replacement"),
            (json!({"id":"card-1","text":"Because","replacement":{"tool":"ask","arguments":{}}}), "exactly one of replacement"),
            (json!({"id":"card-1","replacement":{"tool":"complete_task","arguments":{}}}), "replacement.tool must be one of"),
            (json!({"id":"card-1","text":"Because"}), "interaction_id is required with text"),
        ] {
            let error = answer_clarification(&client, &args).unwrap_err().to_string();
            assert!(error.starts_with("invalid_input") && error.contains(said), "{args}: {error}");
        }
    }

    #[test]
    fn a_result_and_a_document_carry_their_relative_images_and_the_node_hears_which_stayed_out() {
        use sha2::Digest;
        let hex = |bytes: &[u8]| sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect::<String>();
        let dir = crate::test_support::Directory::new();
        let (cwd, home) = (dir.join("worktree"), dir.join("home"));
        std::fs::create_dir_all(cwd.join("docs")).unwrap();
        std::fs::create_dir_all(home.join("files")).unwrap();
        std::fs::write(cwd.join("docs/shot.png"), b"png").unwrap();
        let route = format!("/files?path=files/images/{}.png", hex(b"png"));
        let result = json!({"task_id":"t-1","content":{"kind":"report","body":"![s](docs/shot.png) ![g](docs/gone.png)"}});
        let (carried, notice) = with_relative_images("present_result", &result, &cwd, &home);
        assert_eq!(carried["content"]["body"], format!("![s]({route}) ![g](docs/gone.png)"));
        assert!(notice.unwrap().contains("docs/gone.png (there is no such file in your working folder)"));
        std::fs::write(home.join("files/doc.md"), "![s](docs/shot.png)").unwrap();
        let digest = hex(b"![s](docs/shot.png)");
        let document = json!({"task_id":"t-1","document_id":"d","content_reference":"files/doc.md","content_digest":digest});
        let (carried, notice) = with_relative_images("create_document", &document, &cwd, &home);
        let stored = std::fs::read_to_string(carried["content_reference"].as_str().unwrap()).unwrap();
        assert_eq!(stored, format!("![s]({route})"));
        assert_eq!(carried["content_digest"], hex(stored.as_bytes()));
        assert_eq!(notice, None);
    }

    /// Every schema a tool publishes is spelled out where it is used, with its type: Claude Code
    /// sends a property without one (an empty schema, a `$ref`) as text.
    fn explicit(schema: &Value, at: &str, problems: &mut Vec<String>) {
        const KNOWN: [&str; 17] = ["type", "properties", "required", "additionalProperties", "items", "oneOf", "enum", "const",
            "description", "default", "minItems", "maxItems", "minLength", "maxLength", "minimum", "maximum", "pattern"];
        let Some(fields) = schema.as_object() else { return problems.push(format!("{at}: not a schema")) };
        if let Some(unknown) = fields.keys().find(|key| !KNOWN.contains(&key.as_str())) {
            problems.push(format!("{at}: {unknown}"));
        }
        if !fields.contains_key("type") {
            problems.push(format!("{at}: no type"));
        }
        let is_object = |t: &Value| t == "object" || t.as_array().is_some_and(|ts| ts.contains(&json!("object")));
        if fields.get("type").is_some_and(is_object) && !fields.contains_key("properties") && !fields.contains_key("additionalProperties") && !fields.contains_key("oneOf") {
            problems.push(format!("{at}: an object that says nothing of its fields"));
        }
        for (key, property) in fields.get("properties").and_then(Value::as_object).into_iter().flatten() {
            explicit(property, &format!("{at}.{key}"), problems);
        }
        for (i, branch) in fields.get("oneOf").and_then(Value::as_array).into_iter().flatten().enumerate() {
            explicit(branch, &format!("{at}|{i}"), problems);
        }
        if let Some(items) = fields.get("items") {
            explicit(items, &format!("{at}[]"), problems);
        }
        if let Some(extra @ Value::Object(_)) = fields.get("additionalProperties") {
            explicit(extra, &format!("{at}{{}}"), problems);
        }
    }

    #[test]
    fn every_tool_schema_is_inline_explicit_and_typed() {
        for role in [Role::Node, Role::Orchestrator] {
            let mut problems = vec![];
            for tool in tools(role, "User").as_array().unwrap() {
                explicit(&tool["inputSchema"], tool["name"].as_str().unwrap(), &mut problems);
            }
            assert!(problems.is_empty(), "{problems:#?}");
        }
    }

    #[test]
    fn calls_that_fit_the_schema_pass_untouched() {
        let node = tools(Role::Node, "User");
        let ask = json!({"interaction_id":"q-1","scope":{"kind":"task","task_id":"t-1"},"recipient":{"kind":"user"},
            "need":"Pick the icon","why":"It ships in the next beta","blocking":true,"options":[{"id":"a","label":"A"},{"id":"b","label":"B"}],
            "recommended":"a","content":{"kind":"report","body":"Both icons"},"replaces":null});
        assert_eq!(check_arguments(&node, "ask", &ask), Ok(()));
        let orchestrator = tools(Role::Orchestrator, "User");
        let stack_cleared = json!({"task_id":"t-1","dependencies":[],"stack":null});
        assert_eq!(check_arguments(&orchestrator, "set_dependencies", &stack_cleared), Ok(()));
        let without_stack = json!({"task_id":"t-1","dependencies":[]});
        assert_eq!(check_arguments(&orchestrator, "set_dependencies", &without_stack), Ok(()), "a field that takes null may be left out");
        let to_the_orchestrator = json!({"interaction_id":"q-2","scope":{"kind":"orchestrator","request_id":null},"recipient":{"kind":"orchestrator"},
            "why":"x","blocking":false,"options":[]});
        assert_eq!(check_arguments(&orchestrator, "ask", &to_the_orchestrator), Ok(()));
        assert_eq!(check_arguments(&node, "no_such_tool", &json!("anything")), Ok(()), "an unknown tool is refused by its dispatch");
        let mut with_metadata = ask.clone();
        for key in PROTOCOL_METADATA {
            with_metadata[key] = json!(1);
        }
        assert_eq!(check_arguments(&node, "ask", &with_metadata), Ok(()), "what Alondra supplies itself is dropped, never refused");
    }

    #[test]
    fn a_union_says_which_kinds_it_takes_and_a_corrected_card_is_checked_against_its_own_tool() {
        let node = tools(Role::Node, "User");
        let ask = |recipient: Value| json!({"interaction_id":"q","scope":{"kind":"task","task_id":"t"},"recipient":recipient,"why":"x","blocking":true});
        let wrong = check_arguments(&node, "ask", &ask(json!({"kind":"everyone"}))).unwrap_err();
        assert!(wrong.contains("`recipient.kind` must be one of user, orchestrator, agent, got \"everyone\""), "{wrong}");
        let typo = check_arguments(&node, "ask", &ask(json!({"type":"user"}))).unwrap_err();
        assert!(typo.contains("missing `recipient.kind`: one of user, orchestrator, agent"), "{typo}");
        let clarified = json!({"id":"card-1","replacement":{"tool":"ask","arguments":{"interaction_id":"q","scope":{"kind":"task","task_id":"t"},
            "recipient":{"kind":"user"},"why":"x","blocking":true,"content":"the log"}}});
        let refused = check_arguments(&node, "answer_clarification", &clarified).unwrap_err();
        assert!(refused.starts_with("replacement: invalid_input: the arguments of ask") && refused.contains("`content` must be an object, got a string"), "{refused}");
    }

    #[test]
    fn a_command_alondra_cannot_build_never_names_its_internal_fields() {
        let error = domain_envelope("present_result", &json!({"task_id":"t","interaction_id":"r","is_final":true,"need":"Review it","targets":[{"kind":"text","text":"x"}],"title":"T"}), "epoch", "c").unwrap_err().to_string();
        assert!(error.contains("unknown field `title`") && !error.contains("expected one of") && !error.contains("card"), "{error}");
        let error = domain_envelope("present_result", &json!({"task_id":"t","interaction_id":"r","is_final":true,"targets":[],"card":"{\"need\":\"Review it\"}"}), "epoch", "c").unwrap_err().to_string();
        assert!(error.contains("present_result has no field card: need, why and content go at the top level"), "{error}");
    }
}
