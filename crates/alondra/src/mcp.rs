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

fn instructions(role: Role, user: &str) -> String {
    let language = match crate::i18n::lang() {
        crate::i18n::Lang::Es => "Spanish",
        crate::i18n::Lang::En => "English",
    };
    let common = format!(
        "Alondra manages durable task state. The user is {user}; write user-facing content in {language}. Use the Alondra tools for durable changes; Alondra supplies their protocol metadata internally. A published result is not task completion. Reading, accepting a delivery, authorizing a merge, claiming its execution and completing a task are separate durable transitions. An old session without a daemon-issued capability must resume with the current plugin; public session IDs and process ancestry never grant task authority."
    );
    match role {
        Role::Node => format!(
            "{common} A node works on one Task at a time: work only on your assigned Task; its id is in your mission, and your subagents work on it too. Your terminal does not reach {user}: these tools do, and Alondra types {user}'s answers into this session. `ask` puts one decision in front of {user}; end your turn after it and the answer arrives here. `request_action` hands {user} something to do outside Alondra. A plan goes through plan mode: EnterPlanMode, then ExitPlanMode, which {user} approves or sends back with comments; never write a plan as a document or ask about it. For another document {user} must approve, create_document, then ask, then link_interaction_subject on that question with the document_review revision: {user} reviews the whole document with comments there, so never ask a separate question about it. When you are done, deliver with present_result and is_final true: a file or a final message left in this session never reaches {user}. While you work on your own there is nothing to send. Call register_pr with its explicit task_id, canonical host/repository/number, observed head and base; one Task owns at most one PR for its lifetime. For a report or answer without code, no PR is required. When {user} accepts your final result, Alondra completes the Task itself (a PR delivery once its presented head is confirmed merged); there is nothing left to call. complete_task is only for a Request with auto-merge, after you merged its PR yourself. When Alondra tells you that a PR merge is authorized, call claim_merge_authorization; the daemon verifies the authorization, remote PR head and base before allowing the merge. Never infer authorization or invoke merge without it. Ending a Claude turn does not complete a task. Keep stack retarget and dependency checks. Technical permission requests remain separate from task questions. When you stop only because the machine lacks resources (memory, a build queue, a usage limit), say so with wait_for_machine. Withdraw a question of yours that no longer needs an answer with withdraw_question."
        ),
        Role::Orchestrator => format!(
            "{common} Define cohesive Tasks under Requests with create_task. A code Task should fit one PR; reports and answers may have none. Assign each Task and associate its outputs explicitly. A node works on one Task at a time. Start work on a Task with launch (a new node for that Task, with the options its Request was asked for) or with send to a free node whose finished work it continues; a node still on another Task is refused, so launch a new one and tell it in its mission what the busy node is doing when their work touches; interrupt stops a node stuck in one call, and machine shows capacity and usage before a launch. Retitle a Task with retitle_task, withdraw a question you asked that no longer needs an answer with withdraw_question, and close a Request that needs no Task with close_request (answer: what you told the user in one line; withdrawn: true when the user withdrew it; title: a short name). Create durable scheduled work for the user with schedule_create. fleet shows the live nodes and what waits on someone, node one node in detail, and candidates the free live nodes a Task or project could go to, with the busy ones apart. Finish an invocation after handling its durable batch; that is not Task completion. Never infer a Task, PR owner, User acceptance or completion from hook text. The daemon does not automatically merge PRs."
        ),
    }
}


fn tools(role: Role, user: &str) -> Value {
    let mut list = match role {
        Role::Node => node_domain_tools(user),
        Role::Orchestrator => orchestrator_domain_tools(user),
    };
    if role == Role::Node {
        list.push(json!({"name": "wait_for_machine", "description": "Declare that you are stopped waiting for machine resources (RAM, build queue, usage limit).", "inputSchema": {"type": "object", "properties": {"reason": {"type": "string"}}, "required": ["reason"]}}));
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
            "options": {"type": "object", "description": "Alondra launch choices, e.g. pr: review or auto_merge, effort: normal"},
            "disallowed_tools": {"type": "array", "items": {"type": "string"}, "description": "Optional Claude tool deny patterns"}
        }, "required": ["title", "text", "project", "frequency", "hour", "minute"]}}));
    }
    if role == Role::Orchestrator {
        list.extend([
            json!({"name": "fleet", "description": format!("Fleet summary: unfinished nodes that are running, dormant or owe {user} something, with what they run in the background (subagents and shells; `suspect` when possibly hung); open questions, unread reports, undelivered messages for you, and the machine."), "inputSchema": {"type": "object", "properties": {"all": {"type": "boolean", "description": "Add finished and gone nodes"}}}}),
            json!({"name": "node", "description": "A node's state, tool in progress, last message, open questions and actions, unread reports, pending messages and recent events.", "inputSchema": {"type": "object", "properties": {"node": {"type": "string", "description": "Name, short id or session_id"}, "history": {"type": "boolean", "description": "Add every question, report and message, and 100 events"}, "before": {"type": "integer", "description": "The 100 events before this seq"}}, "required": ["node"]}}),
            json!({"name": "machine", "description": "Machine capacity (memory, load, CPU, disk) and the subscription's usage: the week, today against its slice, and whether today is over pace.", "inputSchema": {"type": "object", "properties": {}}}),
            json!({"name": "send", "description": "Send a message to a node; Alondra types it into the node's session (waking it if it is dormant). With task, the message is about that task: the node is told which one, a task no node works on yet becomes this node's, and a done task is reopened. A node works on one task at a time: a task for a node still on another is refused (agent_busy); launch a new node for it.", "inputSchema": {"type": "object", "properties": {"node": {"type": "string"}, "text": {"type": "string"}, "task": {"type": "string", "description": "The id of the task (t-…) the message is about"}}, "required": ["node", "text"]}}),
            json!({"name": "interrupt", "description": "Press Esc in a node's session to stop the call it is stuck in, for when Alondra reports a node inside one call that looks hung; `send` then moves it on.", "inputSchema": {"type": "object", "properties": {"node": {"type": "string", "description": "Name, short id or session_id"}}, "required": ["node"]}}),
            json!({"name": "launch", "description": format!("Launch a new node with the Alondra plugin. With task, the node is launched for that task (one whose start failed, or whose node is gone) and gets its brief unless you pass a prompt. Refused while today is over its slice of the weekly subscription usage (see `machine`); over_pace_ok launches anyway once {user} has said so."), "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}, "cwd": {"type": "string"}, "prompt": {"type": "string", "description": "The node's mission; optional with task"}, "task": {"type": "string", "description": "The id of the task (t-…) the node is launched for"}, "title": {"type": "string", "description": format!("A short name for the task {user} reads (a few words); without it the task goes by the node's name")}, "model": {"type": "string", "description": "Only to override the model the request's effort sets"}, "request_id": {"type": "string", "description": format!("Without task, the request_id of {user}'s request this launch serves: Alondra adds the options {user} chose to the node's mission")}, "effort": {"type": "string", "enum": ["light", "normal", "hard"], "description": format!("Choose independently for this task when the request's effort is auto or there is no request: light for bounded work with a known approach; normal for ordinary implementation and investigation (the starting point); hard only for a concrete unresolved difficulty named in the mission. Request size or importance does not justify hard. An effort {user} chose in the request is binding and ignores this")}, "over_pace_ok": {"type": "boolean", "description": format!("true only when {user} approved launching over today's usage slice")}}, "required": ["name", "cwd"]}}),
            json!({"name": "candidates", "description": "The live nodes a task, or work in a repo or project, could go to: the ones in that project or repo, or that worked on tasks it depends on, that depend on it, or of the same request, each with why. A node works on one task at a time: `candidates` are the free ones; `busy` lists the ones still on another task, with that task, so a new node's mission can say what they are doing.", "inputSchema": {"type": "object", "properties": {"task": {"type": "string", "description": "A task id (t-…)"}, "repo": {"type": "string", "description": "A project name or owner/repo, when there is no task yet"}}}}),
        ]);
    }
    for tool in &mut list {
        if tool["name"] == "launch" {
            tool["description"] = json!(format!(
                "Launch a new node with the Alondra plugin for a Task no connected node works on: it gets the Task's mission (its title and its Request) unless you pass a prompt, the options {user} chose for the Request, its domain capability, and the Task. Refused while today is over its slice of the weekly subscription usage (see `machine`); over_pace_ok launches anyway once {user} has said so."
            ));
            let properties = tool["inputSchema"]["properties"]
                .as_object_mut()
                .expect("launch properties");
            properties.remove("request_id");
            tool["inputSchema"]["required"] = json!(["name", "cwd", "task"]);
        }
    }
    Value::Array(list)
}

fn domain_tool(name: &str, description: &str, fields: Value, required: &[&str]) -> Value {
    let mut properties = serde_json::Map::new();
    properties.extend(fields.as_object().expect("properties object").clone());
    let mut required_fields = Vec::new();
    required_fields.extend_from_slice(required);
    json!({"name":name,"description":description,"inputSchema":{"type":"object","properties":properties,"required":required_fields,"additionalProperties":false}})
}

fn interaction_fields() -> Value {
    json!({
        "interaction_id":{"type":"string"},
        "scope":{"oneOf":[
            {"type":"object","properties":{"kind":{"const":"task"},"task_id":{"type":"string"}},"required":["kind","task_id"],"additionalProperties":false},
            {"type":"object","properties":{"kind":{"const":"orchestrator"},"request_id":{"type":["string","null"]}},"required":["kind","request_id"],"additionalProperties":false}
        ]},
        "recipient":{"oneOf":[
            {"type":"object","properties":{"kind":{"const":"user"}},"required":["kind"],"additionalProperties":false},
            {"type":"object","properties":{"kind":{"const":"orchestrator"}},"required":["kind"],"additionalProperties":false},
            {"type":"object","properties":{"kind":{"const":"agent"},"agent_id":{"type":"string"}},"required":["kind","agent_id"],"additionalProperties":false}
        ]},
        "body":{"type":"string"},"blocking":{"type":"boolean"},
        "options":{"type":"array","items":{"type":"object","properties":{
            "id":{"type":"string"},"label":{"type":"string"},"description":{"type":"string"},
            "inputs":{"type":"array","items":{"type":"object","properties":{
                "id":{"type":"string"},"kind":{"enum":["text","boolean","integer"]},"required":{"type":"boolean"}
            },"required":["id","kind","required"],"additionalProperties":false}}
        },"required":["id","label","inputs"],"additionalProperties":false}},
        "attachments":{"type":"array","items":{"type":"object","properties":{"id":{"type":"string"},"name":{"type":"string"},"content_digest":{"type":"string","pattern":"^[0-9a-f]{64}$"},"content_reference":{"type":"string"},"bytes":{"type":"integer","minimum":0}},"required":["id","name","content_digest","content_reference","bytes"],"additionalProperties":false}}
    })
}

fn input_value_schema() -> Value {
    json!({"type":"object","additionalProperties":{"oneOf":[
        {"type":"object","properties":{"kind":{"const":"text"},"value":{"type":"string"}},"required":["kind","value"],"additionalProperties":false},
        {"type":"object","properties":{"kind":{"const":"boolean"},"value":{"type":"boolean"}},"required":["kind","value"],"additionalProperties":false},
        {"type":"object","properties":{"kind":{"const":"integer"},"value":{"type":"integer"}},"required":["kind","value"],"additionalProperties":false}
    ]}})
}

fn node_domain_tools(user: &str) -> Vec<Value> {
    let mut interaction = interaction_fields();
    interaction["scope"] = json!({"type":"object","properties":{"kind":{"const":"task"},"task_id":{"type":"string"}},"required":["kind","task_id"],"additionalProperties":false});
    let mut tools = vec![];
    for (name, kind, description, fields, required) in [
        (
            "ask",
            "ask_question",
            format!(
                "Ask {user} or the orchestrator a durable question. Explain what each choice means."
            ),
            json!({"interaction_id":interaction["interaction_id"],"scope":interaction["scope"],"recipient":interaction["recipient"],"body":interaction["body"],"blocking":interaction["blocking"],"options":interaction["options"],"attachments":interaction["attachments"]}),
            vec![
                "interaction_id",
                "scope",
                "recipient",
                "body",
                "blocking",
                "options",
            ],
        ),
        (
            "request_action",
            "request_action",
            "Request a durable action choice from the recipient.".into(),
            json!({"interaction_id":interaction["interaction_id"],"scope":interaction["scope"],"recipient":interaction["recipient"],"body":interaction["body"],"blocking":interaction["blocking"],"options":interaction["options"],"attachments":interaction["attachments"]}),
            vec![
                "interaction_id",
                "scope",
                "recipient",
                "body",
                "blocking",
                "options",
            ],
        ),
    ] {
        let _ = kind;
        tools.push(domain_tool(name, &description, fields, &required));
    }
    let attachment = json!({"type":"object","properties":{"id":{"type":"string"},"name":{"type":"string"},"content_digest":{"type":"string","pattern":"^[0-9a-f]{64}$"},"content_reference":{"type":"string"},"bytes":{"type":"integer","minimum":0}},"required":["id","name","content_digest","content_reference","bytes"],"additionalProperties":false});
    let target = json!({"type":"array","items":{"oneOf":[
        {"type":"object","properties":{"kind":{"const":"text"},"text":{"type":"string"}},"required":["kind","text"],"additionalProperties":false},
        {"type":"object","properties":{"kind":{"const":"document_revision"},"document_id":{"type":"string"},"revision_id":{"type":"string"}},"required":["kind","document_id","revision_id"],"additionalProperties":false},
        {"type":"object","properties":{"kind":{"const":"pull_request"},"pr_id":{"type":"string"},"head":{"type":"string"}},"required":["kind","pr_id","head"],"additionalProperties":false}
    ]}});
    let simple = [
        (
            "start_task",
            "start_task",
            "Start the Task the orchestrator assigned you after your previous one ended, once its dependencies allow.",
            json!({"task_id":{"type":"string"}}),
            vec!["task_id"],
        ),
        (
            "publish_information",
            "publish_information",
            "Send a durable update to the user or orchestrator.",
            json!({"interaction_id":interaction["interaction_id"],"scope":interaction["scope"],"recipient":interaction["recipient"],"body":interaction["body"],"attachments":{"type":"array","items":attachment}}),
            vec!["interaction_id", "scope", "recipient", "body"],
        ),
        (
            "withdraw_question",
            "withdraw_question",
            "Withdraw a question or action request you authored that no longer needs an answer.",
            json!({"interaction_id":{"type":"string"}}),
            vec!["interaction_id"],
        ),
        (
            "answer_question",
            "answer_question",
            "Answer a question addressed to this node.",
            json!({"interaction_id":{"type":"string"},"option_id":{"type":"string"},"inputs":input_value_schema(),"attachments":{"type":"array","items":attachment}}),
            vec!["interaction_id", "option_id", "inputs"],
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
            json!({"task_id":{"type":"string"},"pr_id":{"type":"string"},"host":{"type":"string"},"repository":{"type":"string"},"number":{"type":"integer","minimum":1},"head":{"type":"string"},"base":{"type":"string"},"allowed_methods":{"type":"array","items":{"type":"string","enum":["merge","squash","rebase"]},"minItems":1}}),
            vec![
                "task_id",
                "pr_id",
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
            json!({"interaction_id":{"type":"string"},"subject":{"oneOf":[{"type":"object","properties":{"kind":{"const":"pull_request"},"pr_id":{"type":"string"}},"required":["kind","pr_id"],"additionalProperties":false},{"type":"object","properties":{"kind":{"const":"document_review"},"document_id":{"type":"string"},"revision_id":{"type":"string"}},"required":["kind","document_id","revision_id"],"additionalProperties":false}]}}),
            vec!["interaction_id", "subject"],
        ),
        (
            "present_result",
            "present_result",
            "Present a result to the user. Final results still need explicit user acceptance.",
            json!({"task_id":{"type":"string"},"interaction_id":{"type":"string"},"body":{"type":"string"},"targets":target,"is_final":{"type":"boolean"},"attachments":{"type":"array","items":attachment}}),
            vec!["task_id", "interaction_id", "body", "targets", "is_final"],
        ),
        (
            "publish_stack_readiness",
            "publish_stack_readiness",
            "Report whether your presented PR is ready for its dependent stack.",
            json!({"task_id":{"type":"string"},"presentation_id":{"type":"string"},"pr_id":{"type":"string"},"head":{"type":"string"},"ready":{"type":"boolean"}}),
            vec!["task_id", "presentation_id", "pr_id", "head", "ready"],
        ),
        (
            "complete_task",
            "complete_task",
            "Complete your Task after you merged its PR under auto-merge. An accepted final delivery completes its Task on its own.",
            json!({"task_id":{"type":"string"},"presentation_id":{"type":"string"}}),
            vec!["task_id", "presentation_id"],
        ),
        (
            "claim_merge_authorization",
            "claim_merge_authorization",
            "Confirm the remote head and base for the merge the user just authorized. The daemon resolves and validates the matching authorization internally.",
            json!({"remote_head":{"type":"string"},"remote_base":{"type":"string"},"base_verified":{"type":"boolean","const":true}}),
            vec!["remote_head", "remote_base", "base_verified"],
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

fn orchestrator_domain_tools(user: &str) -> Vec<Value> {
    let interaction = interaction_fields();
    let options = interaction["options"].clone();
    let recipient = interaction["recipient"].clone();
    let scope = interaction["scope"].clone();
    let mut tools = vec![];
    let simple = [
        (
            "create_request",
            "Create a durable Request from the user's new work.",
            json!({"request_id":{"type":"string"},"body":{"type":"string"},"auto_merge":{"type":"boolean","description":"True only if the user explicitly chose automatic PR merge"}}),
            vec!["request_id", "body"],
        ),
        (
            "close_request",
            "Close a Request that needs no Task. State the answer or mark it withdrawn.",
            json!({"request_id":{"type":"string"},"answer":{"type":"string"},"withdrawn":{"type":"boolean"},"title":{"type":"string"}}),
            vec!["request_id"],
        ),
        (
            "create_task",
            "Create a cohesive Task under an open Request and optionally assign a free node (a node works on one Task at a time).",
            json!({"task_id":{"type":"string"},"request_id":{"type":"string"},"title":{"type":"string"},"agent_id":{"type":["string","null"]},"review_prs":{"type":"boolean"},"dependencies":{"type":"array","items":{"type":"string"}},"stack":{"type":["object","null"],"properties":{"upstream_task":{"type":"string"},"presentation_id":{"type":"string"},"pr_id":{"type":"string"},"head":{"type":"string"}},"required":["upstream_task","presentation_id","pr_id","head"],"additionalProperties":false}}),
            vec![
                "task_id",
                "request_id",
                "title",
                "review_prs",
                "dependencies",
                "stack",
            ],
        ),
        (
            "assign_task",
            "Assign an active Task to a registered node that has no other active Task.",
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
            "Set a Task's dependencies and optional stack relationship.",
            json!({"task_id":{"type":"string"},"dependencies":{"type":"array","items":{"type":"string"}},"stack":{"type":["object","null"],"properties":{"upstream_task":{"type":"string"},"presentation_id":{"type":"string"},"pr_id":{"type":"string"},"head":{"type":"string"}},"required":["upstream_task","presentation_id","pr_id","head"],"additionalProperties":false}}),
            vec!["task_id", "dependencies", "stack"],
        ),
        (
            "start_task",
            "Mark a Task started once its dependencies are ready.",
            json!({"task_id":{"type":"string"}}),
            vec!["task_id"],
        ),
        (
            "publish_information",
            "Send the user or a node a durable update.",
            json!({"interaction_id":interaction["interaction_id"],"scope":scope,"recipient":recipient,"body":interaction["body"],"attachments":interaction["attachments"]}),
            vec!["interaction_id", "scope", "recipient", "body"],
        ),
        (
            "ask",
            "Ask the user or a node a durable question; explain each choice.",
            json!({"interaction_id":interaction["interaction_id"],"scope":scope,"recipient":recipient,"body":interaction["body"],"blocking":interaction["blocking"],"options":options,"attachments":interaction["attachments"]}),
            vec![
                "interaction_id",
                "scope",
                "recipient",
                "body",
                "blocking",
                "options",
            ],
        ),
        (
            "request_action",
            "Request a durable action choice from the user or a node.",
            json!({"interaction_id":interaction["interaction_id"],"scope":scope,"recipient":recipient,"body":interaction["body"],"blocking":interaction["blocking"],"options":options,"attachments":interaction["attachments"]}),
            vec![
                "interaction_id",
                "scope",
                "recipient",
                "body",
                "blocking",
                "options",
            ],
        ),
        (
            "withdraw_question",
            "Withdraw a question or action request you authored that no longer needs an answer.",
            json!({"interaction_id":{"type":"string"}}),
            vec!["interaction_id"],
        ),
        (
            "answer_question",
            "Answer a question addressed to the orchestrator.",
            json!({"interaction_id":{"type":"string"},"option_id":{"type":"string"},"inputs":input_value_schema(),"attachments":interaction["attachments"]}),
            vec!["interaction_id", "option_id", "inputs"],
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
    let _ = user;
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
        "start_task"
        | "publish_information"
        | "ask"
        | "request_action"
        | "withdraw_question"
        | "answer_question"
        | "create_document"
        | "revise_document"
        | "register_pr"
        | "link_interaction_subject"
        | "present_result"
        | "publish_stack_readiness"
        | "complete_task"
        | "claim_merge_authorization"
        | "wait_for_machine" => execute_tool(client, name, args),
        _ => anyhow::bail!("incompatible_protocol: this tool is unavailable to the node MCP"),
    }
}

fn call_orchestrator(client: &Client, name: &str, args: &Value) -> Result<String> {
    match name {
        "create_request"
        | "close_request"
        | "create_task"
        | "assign_task"
        | "retitle_task"
        | "set_dependencies"
        | "start_task"
        | "publish_information"
        | "ask"
        | "request_action"
        | "withdraw_question"
        | "answer_question"
        | "link_task_document"
        | "cancel_task"
        | "reopen_task"
        | "launch"
        | "send"
        | "interrupt"
        | "machine"
        | "schedule_create"
        | "fleet"
        | "node"
        | "candidates" => execute_tool(client, name, args),
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
            waiting = Client::new(longer);
            &waiting
        }
    };
    Ok(match name {
        "claim_merge_authorization" => {
            let bearer = domain_bearer()?;
            let capabilities = client.domain_get("/capabilities", &bearer)?;
            let data_epoch = capabilities.get("data_epoch").and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("incompatible_protocol: daemon returned no data epoch"))?;
            let mut body = args.as_object().cloned()
                .ok_or_else(|| anyhow::anyhow!("tool arguments must be an object"))?;
            body.insert("data_epoch".into(), Value::String(data_epoch.to_owned()));
            body.insert("command_id".into(), Value::String(crate::client::new_id("mcp")));
            body.insert("expected_revision".into(), Value::from(0));
            let response = client.domain_call(
                "POST",
                "/merge-authorizations/claim",
                &bearer,
                Some(&Value::Object(body)),
            )?;
            response.get("message").and_then(Value::as_str)
                .unwrap_or("Merge confirmed.").to_owned()
        }
        "ask"
        | "request_action"
        | "publish_information"
        | "withdraw_question"
        | "answer_question"
        | "create_request"
        | "close_request"
        | "create_task"
        | "assign_task"
        | "retitle_task"
        | "set_dependencies"
        | "start_task"
        | "create_document"
        | "revise_document"
        | "link_task_document"
        | "register_pr"
        | "link_interaction_subject"
        | "present_result"
        | "publish_stack_readiness"
        | "complete_task"
        | "cancel_task"
        | "reopen_task" => {
            let bearer = domain_bearer()?;
            let capabilities = client.domain_get("/capabilities", &bearer)?;
            let data_epoch = capabilities.get("data_epoch").and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("incompatible_protocol: daemon returned no data epoch"))?;
            let envelope = domain_envelope(name, args, data_epoch, &crate::client::new_id("mcp"))?;
            let (method, path) = resource_target(name, &envelope["command"])?;
            let mut body = envelope["command"].clone();
            body.as_object_mut().expect("typed command object").remove("kind");
            for key in ["data_epoch", "command_id", "expected_revision"] {
                body[key] = envelope[key].clone();
            }
            let response = client.domain_call(method, &path, &bearer, Some(&body))?;
            response.get("notice").and_then(Value::as_str)
                .unwrap_or("Recorded.").to_owned()
        }
        "launch" | "send" | "interrupt" => {
            let bearer = domain_bearer()?;
            serde_json::to_string_pretty(&client.domain_post(
                &format!("/orchestrator/{name}"),
                &bearer,
                args,
            )?)?
        }
        "schedule_create" => {
            let bearer = domain_bearer()?;
            serde_json::to_string_pretty(&client.domain_post(
                "/orchestrator/schedules",
                &bearer,
                args,
            )?)?
        }
        "fleet" => {
            let bearer = domain_bearer()?;
            serde_json::to_string_pretty(&fleet_summary(
                &client.domain_get("/orchestrator/fleet", &bearer)?,
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
        "candidates" => {
            let bearer = domain_bearer()?;
            let query: Vec<String> = ["task", "repo"]
                .iter()
                .filter_map(|k| {
                    args.get(*k)
                        .and_then(Value::as_str)
                        .filter(|v| !v.trim().is_empty())
                        .map(|v| format!("{k}={}", urlencode(v)))
                })
                .collect();
            serde_json::to_string_pretty(&client.domain_get(
                &format!("/orchestrator/candidates?{}", query.join("&")),
                &bearer,
            )?)?
        }
        "wait_for_machine" => {
            let bearer = domain_bearer()?;
            let reason = args.get("reason").and_then(Value::as_str).unwrap_or("");
            client.domain_post(
                "/agents/waiting-machine",
                &bearer,
                &json!({"reason": reason}),
            )?;
            "Noted: you are waiting for the machine.".to_string()
        }
        "machine" => {
            let bearer = domain_bearer()?;
            serde_json::to_string_pretty(&client.domain_get("/orchestrator/machine", &bearer)?)?
        }
        other => anyhow::bail!("unknown tool: {other}"),
    })
}

pub(crate) fn resource_target(name: &str, command: &Value) -> Result<(&'static str, String)> {
    let value = |key: &str| -> Result<&str> {
        command.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("{name} requires {key}"))
    };
    let id = |key: &str| -> Result<String> { Ok(urlencode(value(key)?)) };
    let target = match name {
        "ask" | "ask_question" => ("POST", "/questions".to_owned()),
        "request_action" => ("POST", "/action-requests".to_owned()),
        "publish_information" => ("POST", "/messages".to_owned()),
        "withdraw_question" => ("POST", format!("/interactions/{}/withdrawal", id("interaction_id")?)),
        "answer_question" => ("POST", format!("/interactions/{}/answers", id("interaction_id")?)),
        "mark_read" => ("PUT", format!("/interactions/{}/read-state", id("interaction_id")?)),
        "submit_feedback" => ("POST", format!("/interactions/{}/feedback", id("presentation_id")?)),
        "accept_delivery" => ("POST", format!("/interactions/{}/acceptance", id("presentation_id")?)),
        "authorize_merge" => ("POST", format!("/interactions/{}/merge-authorizations", id("presentation_id")?)),
        "create_request" => ("POST", "/requests".to_owned()),
        "close_request" => ("POST", format!("/requests/{}/close", id("request_id")?)),
        "create_task" => ("POST", format!("/requests/{}/tasks", id("request_id")?)),
        "assign_task" => ("PUT", format!("/tasks/{}/assignee", id("task_id")?)),
        "retitle_task" => ("PATCH", format!("/tasks/{}", id("task_id")?)),
        "set_dependencies" => ("PUT", format!("/tasks/{}/dependencies", id("task_id")?)),
        "start_task" => ("POST", format!("/tasks/{}/start", id("task_id")?)),
        "create_document" => ("POST", format!("/tasks/{}/documents", id("task_id")?)),
        "revise_document" => ("PUT", format!("/tasks/{}/documents/{}", id("task_id")?, id("document_id")?)),
        "decide_document_review" => ("PUT", format!("/tasks/{}/documents/{}/review", id("task_id")?, id("document_id")?)),
        "link_task_document" => ("POST", format!("/tasks/{}/document-links", id("task_id")?)),
        "register_pr" => ("POST", format!("/tasks/{}/pull-requests", id("task_id")?)),
        "link_interaction_subject" => ("PUT", format!("/interactions/{}/subject", id("interaction_id")?)),
        "present_result" => ("POST", format!("/tasks/{}/results", id("task_id")?)),
        "publish_stack_readiness" => ("POST", format!("/tasks/{}/stack-readiness", id("task_id")?)),
        "complete_task" => ("POST", format!("/tasks/{}/completion", id("task_id")?)),
        "cancel_task" => ("POST", format!("/tasks/{}/cancellation", id("task_id")?)),
        "reopen_task" => ("POST", format!("/tasks/{}/reopening", id("task_id")?)),
        "archive_task" => ("POST", format!("/tasks/{}/archival", id("task_id")?)),
        _ => anyhow::bail!("unknown domain resource operation: {name}"),
    };
    Ok(target)
}

fn domain_envelope(name: &str, args: &Value, data_epoch: &str, command_id: &str) -> Result<Value> {
    let kind = match name {
        "ask" => "ask_question",
        "request_action" => "request_action",
        "publish_information" => "publish_information",
        "withdraw_question" => "withdraw_question",
        "answer_question" => "answer_question",
        "create_request" => "create_request",
        "close_request" => "close_request",
        "create_task" => "create_task",
        "assign_task" => "assign_task",
        "retitle_task" => "retitle_task",
        "set_dependencies" => "set_dependencies",
        "start_task" => "start_task",
        "create_document" => "create_document",
        "revise_document" => "revise_document",
        "link_task_document" => "link_task_document",
        "register_pr" => "register_pr",
        "link_interaction_subject" => "link_interaction_subject",
        "present_result" => "present_result",
        "publish_stack_readiness" => "publish_stack_readiness",
        "complete_task" => "complete_task",
        "cancel_task" => "cancel_task",
        "reopen_task" => "reopen_task",
        _ => anyhow::bail!("unknown domain tool: {name}"),
    };
    let mut command = args
        .as_object()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("tool arguments must be an object"))?;
    for key in [
        "protocol_version",
        "data_epoch",
        "command_id",
        "expected_revision",
    ] {
        command.remove(key);
    }
    command.insert("kind".into(), Value::String(kind.into()));
    let envelope = json!({"protocol_version":2,"data_epoch":data_epoch,
        "command_id":command_id,"expected_revision":0,"command":command});
    let _: alondra_core::domain_v2::CommandEnvelope = serde_json::from_value(envelope.clone())?;
    Ok(envelope)
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

/// The fleet the orchestrator acts on: unfinished nodes that are running, dormant, or still owe
/// the user something. `all` adds finished nodes and the ones whose session is gone.
fn fleet_summary(o: &Value, all: bool) -> Value {
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
            let mut row = pick(&n, &["name", "short_id", "project", "state", "state_since", "daemon_needs", "waiting_permission", "alive", "in_daemon", "undeliverable_messages"]);
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
        hidden.insert("see_them_with".into(), json!("fleet with all: true"));
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
                if let Some(d) = t.get("depends_on").filter(|d| d.as_array().is_some_and(|a| !a.is_empty())) {
                    row["depends_on"] = d.clone();
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
        &["name", "short_id", "cwd", "state", "state_since", "alive", "in_daemon", "is_orchestrator", "busy", "tool", "waiting_permission",
          "menu_waiting", "mission", "daemon_needs", "usage_limited", "evidence", "last_prompt", "last_prompt_at", "last_message", "last_message_at"],
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

fn serve_server(
    server_name: &str,
    instructions: String,
    tools: Value,
    dispatch: fn(&Client, &str, &Value) -> Result<String>,
) -> Result<()> {
    let client = Client::new(CALL_TIMEOUT);
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
                Ok(match dispatch(&client, name, &args) {
                    Ok(text) => json!({"content": [{"type": "text", "text": text}]}),
                    Err(e) => json!({"content": [{"type": "text", "text": format!("Alondra error: {e}")}], "isError": true}),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_pr_tool_constructs_a_typed_command() {
        let args = json!({"data_epoch":"epoch","command_id":"register-one","expected_revision":4,
            "task_id":"task-a","pr_id":"pr-a","host":"github.com","repository":"org/repo",
            "number":12,"head":"0123456789abcdef","base":"main","allowed_methods":["merge"]});
        let envelope = domain_envelope("register_pr", &args, "epoch", "register-one").unwrap();
        assert_eq!(envelope["command"]["kind"], "register_pr");
        assert_eq!(envelope["command"]["task_id"], "task-a");
        assert_eq!(envelope["protocol_version"], 2);
        assert!(domain_envelope("register_pr", &json!({"task_id":"task-a"}), "epoch", "register-one").is_err());
        let catalog = tools(Role::Node, "User");
        assert!(catalog.as_array().unwrap().iter().any(|tool| tool["name"] == "register_pr"));
        assert!(catalog.as_array().unwrap().iter().all(|tool| tool["name"] != "domain_command"));
        assert!(catalog.as_array().unwrap().iter().any(|tool| tool["name"] == "complete_task"));
        assert!(instructions(Role::Node, "User").contains("Alondra completes the Task itself"));
        assert!(instructions(Role::Node, "Ana").contains("Your terminal does not reach Ana: these tools do"));
        assert!(instructions(Role::Node, "Ana").contains("deliver with present_result and is_final true"));
        assert!(instructions(Role::Node, "Ana").contains("A plan goes through plan mode: EnterPlanMode, then ExitPlanMode"));
        assert!(instructions(Role::Node, "Ana").contains("create_document, then ask, then link_interaction_subject"));
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
        for n in ["register_pr","present_result","complete_task","create_document","claim_merge_authorization"] { assert!(node.contains(n),"{n}"); }
        for n in ["create_request","close_request","create_task","assign_task","set_dependencies","link_task_document"] { assert!(orchestrator.contains(n),"{n}"); }
        for n in ["create_task","close_request","link_task_document"] { assert!(!node.contains(n),"{n}"); }
        for n in ["present_result","complete_task","register_pr","claim_merge_authorization"] { assert!(!orchestrator.contains(n),"{n}"); }
        for n in ["launch","send","interrupt","machine","schedule_create","fleet","node","candidates"] { assert!(orchestrator.contains(n)); assert!(!node.contains(n)); }
        assert!(node.contains("wait_for_machine")); assert!(!orchestrator.contains("wait_for_machine"));
        let orchestrator_catalog=tools(Role::Orchestrator,"Ana");
        let launch=orchestrator_catalog.as_array().unwrap().iter().find(|t|t["name"]=="launch").unwrap();
        assert_eq!(launch["inputSchema"]["required"],json!(["name","cwd","task"])); assert!(launch["inputSchema"]["properties"].get("request_id").is_none());
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
        let claim=node_catalog.as_array().unwrap().iter().find(|t|t["name"]=="claim_merge_authorization").unwrap();
        assert_eq!(claim["inputSchema"]["properties"]["base_verified"]["const"],true);
        assert!(claim["inputSchema"]["properties"].get("authorization_id").is_none());
        assert!(claim["inputSchema"]["properties"].get("task_id").is_none(),
            "the bearer-bound daemon determines the claim target");
    }

    #[test]
    fn the_v2_fleet_and_node_answers_read_through_the_8ccd131_summaries() {
        let fleet = json!({
            "nodes": [{"session_id": "agent-a", "name": "Instance A", "short_id": "", "project": "App", "state": "working", "alive": true, "in_daemon": false}],
            "open_questions": [{"id": "q", "title": "Ship?", "node_name": "Instance A", "to_orchestrator": false, "action": false, "session_id": "agent-a"}],
            "reports": [{"id": "info", "title": "Found it", "kind": "notice", "read": false, "node_name": "Instance A", "session_id": "agent-a"}],
            "orchestrator_messages": [{"id": 3, "text": "New request", "handed_at": null}],
            "requests": [{"id": "r", "state": "working", "text": "Make the editor readable"}],
            "tasks": [{"id": "t1", "title": "Colours", "state": "running", "request": "r", "depends_on": [], "assignee": {"session_id": "agent-a"}}],
            "machine": {"cpus": 10}, "usage": null,
        });
        let summary = fleet_summary(&fleet, false);
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
    fn catalogs_and_instructions_offer_only_the_merge_claim_path_to_nodes() {
        for role in [Role::Node, Role::Orchestrator] {
            let catalog = tools(role, "User");
            assert!(catalog.as_array().unwrap().iter().all(|tool|
                tool["name"] != "domain_cursor" && tool["name"] != "merge_authorization"));
            let instructions = instructions(role, "User");
            assert!(!instructions.contains("domain_cursor"));
            assert!(!instructions.contains("data_epoch") && !instructions.contains("expected_revision"));
            assert_eq!(instructions.contains("claim_merge_authorization"), role == Role::Node);
        }
    }

    #[test]
    fn v2_catalog_is_selected_before_a_bearer_handoff() {
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
    fn separate_servers_advertise_distinct_method_catalogs() {
        let node = tools(Role::Node, "User");
        let orchestrator = tools(Role::Orchestrator, "User");
        fn names(tools: &Value) -> HashSet<&str> {
            tools.as_array().unwrap().iter()
                .filter_map(|tool| tool["name"].as_str()).collect()
        }
        let node_names = names(&node);
        let orchestrator_names = names(&orchestrator);
        assert!(!node_names.contains("fleet"));
        assert!(!node_names.contains("launch"));
        assert!(orchestrator_names.contains("fleet"));
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
    fn fleet_lists_the_nodes_that_matter_and_counts_the_rest() {
        let v = fleet_summary(&overview(), false);
        let names: Vec<&str> = v["nodes"].as_array().unwrap().iter().map(|n| n["name"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["running", "dormant", "gone-owing"], "a gone node that still owes the user an answer stays");
        assert_eq!(v["hidden"], json!({"done_nodes": 1, "gone_nodes": 2, "see_them_with": "fleet with all: true"}));
        assert_eq!(v["orchestrator_messages"].as_array().unwrap().iter().map(|m| m["id"].as_u64().unwrap()).collect::<Vec<_>>(), vec![2], "already typed in: not again");
        assert!(!v.to_string().contains(SAID), "fleet carries titles, not bodies: {v:#}");
    }

    #[test]
    fn fleet_with_all_lists_every_node_and_message() {
        let v = fleet_summary(&overview(), true);
        assert_eq!(v["nodes"].as_array().unwrap().len(), 6);
        assert_eq!(v["orchestrator_messages"].as_array().unwrap().len(), 2);
        assert!(v.get("hidden").is_none());
    }

    #[test]
    fn the_fleet_lists_open_requests_with_their_task_trees() {
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
        let f = fleet_summary(&o, false);
        assert_eq!(f["requests"][0], json!({"id": "r-1", "state": "defining", "text": "Menu by sections", "tasks": []}), "a request to define");
        assert_eq!(f["requests"][1]["tasks"], json!([
            {"id": "t-5", "title": "Model", "state": "in_review", "node": "app/model"},
            {"id": "t-6", "title": "UI", "state": "waiting_dependency", "depends_on": ["t-5"]},
        ]));
        assert_eq!(f["requests"][1]["text"].as_str().unwrap().chars().count(), 161);
        assert_eq!(f["requests"].as_array().unwrap().len(), 2, "done and made-up requests are left out");
        assert_eq!(f["hidden"], json!({"done_requests": 1, "see_them_with": "fleet with all: true"}));
        assert_eq!(fleet_summary(&o, true)["requests"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn the_fleet_lists_what_a_node_runs_only_when_it_runs_something() {
        let o = json!({"nodes": [
            {"name": "a", "state": "working", "alive": true, "background": [{"kind": "shell", "label": "until pgrep", "started_at": 1, "suspect": true}]},
            {"name": "b", "state": "working", "alive": true, "background": []}
        ]});
        let f = fleet_summary(&o, false);
        assert_eq!(f["nodes"][0]["background"][0]["suspect"], true);
        assert!(f["nodes"][1].get("background").is_none());
    }
}
