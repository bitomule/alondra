use super::{
    task_state::{self, Block},
    BuildError, Confirm, Fact, FactKind, Facts, Icon, Item, Markdown, Notice, RenderContext,
    Style, Tone, Trailing, View, action_id::Verb, common, text::{t, tf},
};
use crate::domain::{DomainState, ModelChooser, NodeEnd, Request, RequestId, Task, TaskId, TaskState};
use crate::model_label::model_family;
use std::collections::{BTreeMap, BTreeSet};

/// A task's own screen says its state with the fact that explains it, unless what it does now has its own lines.
fn detail_state(task: &Task, context: &RenderContext<'_>) -> String {
    let presentation = task_state::present(task, context);
    match (&presentation.fact, presentation.block) {
        (Some(fact), Block::ForYou | Block::Waiting) => format!("{} · {fact}", presentation.state),
        (Some(fact), _) if presentation.fact_in_state => format!("{} · {fact}", presentation.state),
        _ => presentation.state,
    }
}

fn effort_label(effort: &str, lang: super::Lang) -> String {
    common::effort_label(effort, lang).unwrap_or(effort).into()
}

fn relaunch_action(task: &Task, context: &RenderContext<'_>) -> Option<super::Action> {
    if !common::node_closed(task, context) {
        return None;
    }
    let label = t!(context.lang, "Relaunch", "Relanzar");
    let mut action = common::action(Verb::Relaunch, &task.id.0, context.domain.revision, label, Style::Primary)?;
    action.key = (context.client == super::Client::Tui).then(|| "l".into());
    Some(action)
}

/// The open tasks a live node is working on now, each with that node.
pub(super) fn in_progress<'a>(context: &'a RenderContext<'_>) -> impl Iterator<Item = (&'a Task, &'a crate::domain::Agent)> {
    context.domain.agents.values()
        .filter(|agent| super::task_node::on_open_task(agent, context))
        .filter_map(|agent| common::current_task(context, &agent.id)
            .filter(|task| !task.archived && !task.state.terminal())
            .or_else(|| super::task_node::node_tasks(&agent.id, context).find(|task| !task.state.terminal()))
            .map(|task| (task, agent)))
}

pub(super) fn task_row(task: &Task, context: &RenderContext<'_>) -> super::Row {
    presented_row(task, &task_state::present(task, context), context)
}

/// One task as the list draws it: its state in colour, its title, and one fact with since when it holds. The iPhone
/// reads the fact from the row's `activity` line, the Mac beside the state, the TUI under the title.
fn presented_row(task: &Task, presentation: &task_state::Presentation, context: &RenderContext<'_>) -> super::Row {
    let mut row = common::row(&task.id.0, &format!("task:{}", task.id), presentation.icon, task.title.clone(),
        Some(presentation.state.clone()));
    row.struck = task.state == TaskState::Cancelled;
    row.badges.push(super::Label { text: presentation.state.clone(), tone: presentation.tone, elapsed: None });
    row.badges.extend(common::scheduled_mark(task, context));
    row.badges.extend(task.project.as_ref().map(|project| super::Label { text: project.clone(), tone: Tone::Neutral, elapsed: None }));
    row.badges.extend(common::elapsed_mark(task, false, context));
    row.actions.extend(relaunch_action(task, context));
    row.actions.extend(super::pacing::task_actions(task, context));
    let since = presentation.since.clone().map(|at| Trailing::Time { at, format: super::TimeFormat::Relative });
    match context.client {
        super::Client::Tui => {
            row.subtitle = presentation.fact.clone();
            row.trailing = since;
            return row;
        }
        super::Client::Ios => {
            if let Some(fact) = &presentation.fact {
                let mut line = common::row("activity", "node", Icon::Notice, fact.clone(), None);
                line.link = None;
                line.trailing = since;
                row.children.push(line);
            }
        }
        _ => {
            row.subtitle = presentation.fact.clone();
            let count = waiting_count(task, context);
            row.trailing = (count > 0).then(|| Trailing::Text {
                text: match count {
                    1 => t!(context.lang, "1 question", "1 pregunta").into(),
                    _ => tf!(context.lang, "{count} questions", "{count} preguntas"),
                },
            });
            return row;
        }
    }
    row.trailing = Some(Trailing::Count {
        value: waiting_count(task, context) as u32,
    });
    row
}

fn waiting_count(task: &Task, context: &RenderContext<'_>) -> usize {
    context.reading(&task.id).map_or(0, |read| read.waiting.len())
}

/// A request's tasks, by id.
fn tasks_of<'a>(request: &RequestId, context: &RenderContext<'a>) -> Vec<&'a Task> {
    context.index.tasks_of_request(request).iter().filter_map(|id| context.domain.tasks.get(id)).collect()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Bucket {
    Active,
    Finished,
    Cleared,
}

fn bucket(request: &Request, tasks: &[&Task]) -> Bucket {
    if request.cleared || (!tasks.is_empty() && tasks.iter().all(|task| task.archived)) {
        Bucket::Cleared
    } else if request.closed || (!tasks.is_empty() && tasks.iter().all(|task| task.state.terminal())) {
        Bucket::Finished
    } else {
        Bucket::Active
    }
}

/// Where one task is listed: with its whole request when the request is completed; otherwise a task still
/// under way stays with the active request and the ones that ended fold away at the bottom. A cleared task
/// is in no list.
fn task_bucket(task: &Task, request: Bucket) -> Bucket {
    match request {
        _ if task.archived => Bucket::Cleared,
        Bucket::Active if task.state.terminal() => Bucket::Finished,
        whole => whole,
    }
}

/// A message to the orchestrator is listed only once it made tasks: until then, and if it never does, it is
/// in no section. A request answered without tasks is in none either: its card in the inbox holds the answer.
fn request_buckets(domain: &DomainState) -> BTreeMap<&RequestId, Bucket> {
    let mut tasks: BTreeMap<&RequestId, Vec<&Task>> = BTreeMap::new();
    for task in domain.tasks.values() {
        tasks.entry(&task.request_id).or_default().push(task);
    }
    domain.requests.values()
        .filter_map(|request| {
            let own = tasks.get(&request.id).map_or(&[][..], Vec::as_slice);
            (!own.is_empty() || !(request.message || request.answered())).then(|| (&request.id, bucket(request, own)))
        })
        .collect()
}

const TITLE_LIMIT: usize = 60;

/// What the user wrote, short enough to read as a title: its first sentence, cut at a word.
pub(super) fn summary(body: &str) -> String {
    let line = body.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or_default();
    let sentence = line.char_indices()
        .find(|(at, c)| matches!(c, '.' | '?' | '!') && line[at + c.len_utf8()..].starts_with(' '))
        .map_or(line, |(at, c)| &line[..at + c.len_utf8()]);
    let sentence = sentence.trim_end_matches('.');
    if sentence.chars().count() <= TITLE_LIMIT {
        return sentence.to_owned();
    }
    let cut: String = sentence.chars().take(TITLE_LIMIT).collect();
    let words = cut.rsplit_once(' ').map_or(cut.as_str(), |(head, _)| head);
    format!("{}…", words.trim_end_matches([',', ';', ':', ' ']))
}

/// How the Tasks list places Tasks: under the Task they follow in their own Request, or, when that Task belongs
/// to another Request still in the active list, under it there.
struct Forest<'a> {
    domain: &'a DomainState,
    by_request: BTreeMap<&'a RequestId, Vec<&'a Task>>,
    same_parent: BTreeMap<&'a TaskId, &'a Task>,
    nested_under: BTreeMap<&'a TaskId, &'a Task>,
    dependents: BTreeMap<&'a TaskId, Vec<&'a Task>>,
    /// Requests still under way whose ended tasks this section lists apart from the rest.
    split: BTreeSet<&'a RequestId>,
}

impl<'a> Forest<'a> {
    /// The tasks one section lists, each request's tree made only of its tasks in that section.
    fn new(domain: &'a DomainState, section: Bucket, buckets: &BTreeMap<&'a RequestId, Bucket>) -> Self {
        let mut by_request: BTreeMap<&RequestId, Vec<&Task>> = BTreeMap::new();
        for task in domain.tasks.values()
            .filter(|task| buckets.get(&task.request_id).is_some_and(|whole| task_bucket(task, *whole) == section))
        {
            by_request.entry(&task.request_id).or_default().push(task);
        }
        let listed: BTreeSet<&TaskId> = by_request.values().flatten().map(|task| &task.id).collect();
        let active: BTreeSet<&RequestId> = buckets.iter()
            .filter(|(_, whole)| section == Bucket::Active && **whole == Bucket::Active)
            .map(|(id, _)| *id)
            .collect();
        let mut same_parent = BTreeMap::new();
        let mut nested_under = BTreeMap::new();
        let mut dependents: BTreeMap<&TaskId, Vec<&Task>> = BTreeMap::new();
        for tasks in by_request.values() {
            for task in tasks {
                let parent = task.dependencies.iter().filter(|id| **id != task.id)
                    .filter_map(|id| tasks.iter().position(|candidate| candidate.id == *id).map(|position| (position, tasks[position])))
                    .max_by_key(|(position, _)| *position)
                    .map(|(_, parent)| parent);
                if let Some(parent) = parent {
                    same_parent.insert(&task.id, parent);
                    continue;
                }
                if !active.contains(&task.request_id) { continue; }
                let across = task.dependencies.iter().filter_map(|id| domain.tasks.get(id))
                    .find(|dependency| dependency.request_id != task.request_id
                        && active.contains(&dependency.request_id)
                        && listed.contains(&dependency.id)
                        && !Self::reaches(domain, dependency, &task.id));
                if let Some(parent) = across {
                    nested_under.insert(&task.id, parent);
                    dependents.entry(&parent.id).or_default().push(task);
                }
            }
        }
        let split = by_request.keys().copied()
            .filter(|id| section != Bucket::Active && buckets.get(id) == Some(&Bucket::Active))
            .collect();
        Self { domain, by_request, same_parent, nested_under, dependents, split }
    }

    fn reaches(domain: &DomainState, from: &Task, target: &TaskId) -> bool {
        let mut seen = BTreeSet::new();
        let mut stack = vec![from];
        while let Some(task) = stack.pop() {
            if !seen.insert(&task.id) { continue; }
            if task.dependencies.contains(target) { return true; }
            stack.extend(task.dependencies.iter().filter_map(|id| domain.tasks.get(id)));
        }
        false
    }

    fn tasks_of(&self, request: &RequestId) -> &[&'a Task] {
        self.by_request.get(request).map_or(&[], Vec::as_slice)
    }

    fn is_away(&self, task: &Task) -> bool {
        let mut current = task;
        for _ in 0..=self.same_parent.len() {
            if self.nested_under.contains_key(&current.id) { return true; }
            match self.same_parent.get(&current.id) {
                Some(parent) => current = parent,
                None => return false,
            }
        }
        false
    }

    fn other_requests_dependencies(&self, task: &Task) -> Vec<&'a Task> {
        task.dependencies.iter().filter_map(|id| self.domain.tasks.get(id))
            .filter(|dependency| dependency.request_id != task.request_id)
            .collect()
    }

    fn request_title(&self, request_id: &RequestId) -> Option<String> {
        let request = self.domain.requests.get(request_id)?;
        request.title.clone().or_else(|| match self.tasks_of(request_id) {
            [task] => Some(task.title.clone()),
            _ => None,
        })
    }
}

fn request_row(request: &Request, forest: &Forest<'_>, context: &RenderContext<'_>) -> super::Row {
    let tasks = forest.tasks_of(&request.id);
    let title = forest.request_title(&request.id).unwrap_or_else(|| summary(&request.body));
    let split = forest.split.contains(&request.id);
    let headline = (!split).then(|| request_state(request, tasks, context));
    let id = if split { format!("{}:ended", request.id) } else { request.id.0.clone() };
    let mut row = common::row(&id, &format!("request:{}", request.id), Icon::Mission, title, headline.map(Into::into));
    row.trailing = context
        .live
        .request_times
        .get(&request.id.0)
        .map(|at| Trailing::Time {
            at: at.clone(),
            format: super::TimeFormat::Relative,
        });
    if let Some(project) = context.index.request_project(&request.id) {
        row.badges.push(super::Label { text: project.into(), tone: Tone::Neutral, elapsed: None });
    }
    if let Some(task) = tasks.iter().max_by_key(|task| task.revision) {
        row.link = Some(super::Link::View { key: format!("task:{}", task.id) });
    } else if context.client != super::Client::Tui {
        row.link = None;
    }
    let mut children = task_tree(tasks, forest, context);
    if context.client != super::Client::Ios && !split {
        match tasks {
            [task] => {
                let (icon, _) = common::task_presentation(task.state);
                let presented = task_state::present(task, context);
                row.icon = icon;
                row.struck = task.state == TaskState::Cancelled;
                row.subtitle = None;
                row.badges.insert(0, super::Label { text: presented.state, tone: presented.tone, elapsed: None });
                row.badges.extend(common::scheduled_mark(task, context));
                row.badges.extend(common::elapsed_mark(task, false, context));
                row.actions.extend(relaunch_action(task, context));
                row.actions.extend(super::pacing::task_actions(task, context));
                row.children = match children.as_mut_slice() {
                    [only] if only.id == task.id.0 => std::mem::take(&mut only.children),
                    _ => children,
                };
                return row;
            }
            [] => {}
            _ if context.client == super::Client::Tui => {
                let all = tasks_of(&request.id, context);
                let done = all.iter().filter(|task| task.state.terminal()).count();
                row.subtitle = None;
                row.badges.insert(0, super::Label {
                    text: tf!(context.lang, "{done} of {total}", "{done} de {total}", done = done, total = all.len()),
                    tone: Tone::Neutral,
                    elapsed: None,
                });
            }
            _ => {}
        }
    }
    row.children = children;
    row
}

fn request_state(request: &Request, tasks: &[&Task], context: &RenderContext<'_>) -> &'static str {
    let without_tasks = tasks.is_empty();
    if request.withdrawn {
        t!(context.lang, "Withdrawn", "Retirada")
    } else if request.closed && without_tasks && request.answer.is_some() {
        t!(context.lang, "Answered", "Respondida")
    } else if request.closed || (!without_tasks && tasks.iter().all(|task| task.state.terminal())) {
        t!(context.lang, "Finished", "Terminada")
    } else if without_tasks && orchestrator_asks(context) {
        t!(context.lang, "Waiting for your decision", "Esperando tu decisión")
    } else if without_tasks && context.live.orchestrator.as_ref().is_some_and(|orchestrator| orchestrator.waits_for_sign_in) {
        t!(context.lang, "Waiting for a provider session", "Esperando una sesión de proveedor")
    } else if without_tasks {
        t!(context.lang, "Preparing its tasks", "Preparando sus tareas")
    } else {
        t!(context.lang, "Active", "Activa")
    }
}

fn request_items(request: &Request, forest: &Forest<'_>, context: &RenderContext<'_>) -> Vec<Item> {
    let tasks = forest.tasks_of(&request.id);
    if request.title.is_none() && tasks.len() > 1 {
        return task_tree(tasks, forest, context).into_iter().map(Item::Row).collect();
    }
    vec![Item::Row(request_row(request, forest, context))]
}

fn waits_row(dependent: &Task, parent: &Task, depth: u8, name_dependent: bool, context: &RenderContext<'_>) -> super::Row {
    let mut row = common::row(
        &format!("waits:{}:{}", dependent.id, parent.id),
        &format!("task:{}", dependent.id),
        Icon::Branch,
        tf!(context.lang, "waits for {title}", "espera a {title}", title = parent.title),
        name_dependent.then(|| dependent.title.clone()),
    );
    row.depth = depth;
    row
}

fn mark_origin(row: &mut super::Row, request: &str, context: &RenderContext<'_>) {
    let text = tf!(context.lang, "from: {request}", "de: {request}", request = request);
    match context.client {
        super::Client::Ios => {
            let mut note = common::row("from", "", Icon::Request, text, None);
            note.link = None;
            row.children.push(note);
        }
        super::Client::Tui => row.subtitle = Some(text),
        _ => row.subtitle = Some(match row.subtitle.take() {
            Some(state) => format!("{state} · {text}"),
            None => text,
        }),
    }
}

fn branch(task: &Task, depth: u8, forest: &Forest<'_>, placed: &mut BTreeSet<TaskId>,
    context: &RenderContext<'_>) -> Option<super::Row> {
    if !placed.insert(task.id.clone()) { return None; }
    let mut row = task_row(task, context);
    row.depth = depth;
    if forest.nested_under.contains_key(&task.id)
        && let Some(origin) = forest.request_title(&task.request_id) {
        mark_origin(&mut row, &origin, context);
    }
    let children = forest.tasks_of(&task.request_id).iter()
        .filter(|child| forest.same_parent.get(&child.id).is_some_and(|parent| parent.id == task.id))
        .chain(forest.dependents.get(&task.id).into_iter().flatten())
        .filter_map(|child| branch(child, depth.saturating_add(1), forest, placed, context))
        .collect::<Vec<_>>();
    row.children.extend(children);
    let named = forest.nested_under.get(&task.id).map(|parent| parent.id.clone());
    for parent in forest.other_requests_dependencies(task).into_iter().filter(|parent| Some(&parent.id) != named.as_ref()) {
        row.children.push(waits_row(task, parent, depth.saturating_add(1), false, context));
    }
    Some(row)
}

fn task_tree(tasks: &[&Task], forest: &Forest<'_>, context: &RenderContext<'_>) -> Vec<super::Row> {
    let mut placed = BTreeSet::new();
    let mut rows = Vec::new();
    for task in tasks.iter().filter(|task| !forest.same_parent.contains_key(&task.id)) {
        if forest.nested_under.contains_key(&task.id) {
            rows.extend(forest.other_requests_dependencies(task).into_iter()
                .map(|parent| waits_row(task, parent, 1, tasks.len() > 1, context)));
        } else if let Some(row) = branch(task, 1, forest, &mut placed, context) {
            rows.push(row);
        }
    }
    for task in tasks.iter().filter(|task| !forest.is_away(task)) {
        if let Some(row) = branch(task, 1, forest, &mut placed, context) { rows.push(row); }
    }
    rows
}

fn merge_order<'a>(context: &RenderContext<'a>, task: &'a Task) -> Vec<&'a Task> {
    let domain = context.domain;
    let mut seen = BTreeSet::from([task.id.clone()]);
    fn below<'a>(domain: &'a DomainState, task: &'a Task, seen: &mut BTreeSet<TaskId>, out: &mut Vec<&'a Task>) {
        for id in &task.dependencies {
            if !seen.insert(id.clone()) { continue; }
            if let Some(dependency) = domain.tasks.get(id) {
                below(domain, dependency, seen, out);
                out.push(dependency);
            }
        }
    }
    let mut order = Vec::new();
    below(domain, task, &mut seen, &mut order);
    order.push(task);
    let mut frontier = vec![task];
    while !frontier.is_empty() {
        let mut dependents: Vec<&TaskId> = frontier.iter().flat_map(|parent| context.index.dependents(&parent.id)).collect();
        dependents.sort();
        dependents.dedup();
        let next: Vec<_> = dependents.into_iter().filter_map(|id| domain.tasks.get(id))
            .filter(|candidate| seen.insert(candidate.id.clone())).collect();
        order.extend(next.iter().copied());
        frontier = next;
    }
    order
}

/// One entry of the active list: a task alone, or a request with a real title and several tasks under its header.
enum Entry<'a> {
    Task(&'a Task),
    Group(&'a Request, Vec<&'a Task>),
    /// A request the orchestrator has not split into tasks yet.
    Request(&'a Request),
}

/// The active list in three blocks, For you, In progress and Waiting: one row per task, a header only for a
/// request that is not a message to the orchestrator and has several tasks here. A task that waits on another
/// is in Waiting, saying what it waits for, and a block counts only tasks: the blocks add up to the Tasks tab.
fn active_items(buckets: &BTreeMap<&RequestId, Bucket>, context: &RenderContext<'_>) -> (Vec<Item>, u32) {
    let domain = context.domain;
    let listed = listed_active(domain, buckets);
    let shown: BTreeMap<&TaskId, task_state::Presentation> = listed.iter()
        .map(|task| (&task.id, task_state::present(task, context))).collect();
    let mut by_request: BTreeMap<&RequestId, Vec<&Task>> = BTreeMap::new();
    for task in &listed {
        by_request.entry(&task.request_id).or_default().push(task);
    }
    let grouped = |task: &Task| by_request.get(&task.request_id).filter(|tasks| tasks.len() > 1)
        .and_then(|_| domain.requests.get(&task.request_id))
        .filter(|request| !request.message && request.title.is_some());
    let mut entries: Vec<Entry<'_>> = Vec::new();
    let mut placed = BTreeSet::new();
    for task in &listed {
        match grouped(task) {
            Some(request) if placed.insert(&request.id) => entries.push(Entry::Group(request, by_request[&request.id].clone())),
            Some(_) => {}
            None => entries.push(Entry::Task(task)),
        }
    }
    let mut with_tasks: BTreeSet<&RequestId> = domain.tasks.values().map(|task| &task.request_id).collect();
    for request in domain.requests.values()
        .filter(|request| buckets.get(&request.id) == Some(&Bucket::Active) && with_tasks.insert(&request.id)) {
        entries.push(Entry::Request(request));
    }
    let lead = |entry: &Entry<'_>| -> (Block, Order) {
        let tasks = match entry {
            Entry::Task(task) => vec![*task],
            Entry::Group(_, tasks) => tasks.clone(),
            Entry::Request(request) => {
                let since = context.live.request_times.get(&request.id.0).cloned();
                let block = if orchestrator_asks(context) { Block::ForYou } else { Block::InProgress };
                let presentation = task_state::Presentation::waiting_request(block, since);
                return (block, order(&presentation));
            }
        };
        tasks.iter().map(|task| {
            let presentation = &shown[&task.id];
            (presentation.block, order(presentation))
        }).min().expect("an entry has a task")
    };
    let mut blocks: BTreeMap<Block, Vec<(Entry<'_>, Order)>> = BTreeMap::new();
    for entry in entries {
        let Entry::Group(request, tasks) = entry else {
            let (block, key) = lead(&entry);
            blocks.entry(block).or_default().push((entry, key));
            continue;
        };
        let mut by_block: BTreeMap<Block, Vec<&Task>> = BTreeMap::new();
        for task in tasks {
            by_block.entry(shown[&task.id].block).or_default().push(task);
        }
        for (block, tasks) in by_block {
            if tasks.len() > 1 && block != Block::Waiting {
                let entry = Entry::Group(request, tasks);
                let (_, key) = lead(&entry);
                blocks.entry(block).or_default().push((entry, key));
            } else {
                for task in tasks { blocks.entry(block).or_default().push((Entry::Task(task), order(&shown[&task.id]))); }
            }
        }
    }
    let mut items = Vec::new();
    for (block, mut entries) in blocks {
        entries.sort_by(|(_, left), (_, right)| left.cmp(right));
        let (id, title) = match block {
            Block::ForYou => ("block:for-you", t!(context.lang, "For you", "Para ti")),
            Block::InProgress => ("block:in-progress", t!(context.lang, "In progress", "En marcha")),
            Block::Waiting | Block::Ended => ("block:waiting", t!(context.lang, "Waiting", "En espera")),
        };
        let mut row = common::row(id, "", Icon::Task, title.into(), None);
        row.link = None;
        if block == Block::Waiting {
            row.subtitle = task_state::start_hold(&listed, context);
        }
        let mut count = 0;
        for (entry, _) in entries {
            match entry {
                Entry::Task(task) => {
                    count += 1;
                    let mut line = entry_row(task, 1, &shown, context);
                    if let Some(name) = grouped(task).map(request_name) {
                        append_fact(&mut line, &name, context);
                    }
                    row.children.push(line);
                }
                Entry::Request(request) => {
                    let mut line = common::row(&request.id.0, &format!("request:{}", request.id), Icon::Mission,
                        request_name(request), Some(request_state(request, &[], context).into()));
                    line.depth = 1;
                    line.trailing = context.live.request_times.get(&request.id.0)
                        .map(|at| Trailing::Time { at: at.clone(), format: super::TimeFormat::Relative });
                    line.link = None;
                    row.children.push(line);
                }
                Entry::Group(request, tasks) => {
                    let mut header = group_row(request, &tasks, context);
                    for task in &tasks {
                        count += 1;
                        header.children.push(entry_row(task, 2, &shown, context));
                    }
                    row.children.push(header);
                }
            }
        }
        row.trailing = Some(Trailing::Text { text: count.to_string() });
        items.push(Item::Row(row));
    }
    (items, listed.len() as u32)
}

fn listed_active<'a>(domain: &'a DomainState, buckets: &BTreeMap<&RequestId, Bucket>) -> Vec<&'a Task> {
    domain.tasks.values()
        .filter(|task| buckets.get(&task.request_id).is_some_and(|whole| task_bucket(task, *whole) == Bucket::Active))
        .collect()
}

/// How many tasks Active lists: the Tasks tab's count, and the sum of its blocks' counts.
pub(super) fn active_count(domain: &DomainState) -> u32 {
    listed_active(domain, &request_buckets(domain)).len() as u32
}

/// A request not split yet waits for the user while the orchestrator stopped and its card asking
/// whether to retry is in the inbox; without that card nothing says it waits for the user.
fn orchestrator_asks(context: &RenderContext<'_>) -> bool {
    context.live.orchestrator.as_ref().is_some_and(|orchestrator| orchestrator.can_retry)
        && context.index.asks_outside_tasks()
}

/// Where a task goes inside its block: what needs the user longest waiting first; what works most recently active
/// first; the queue by its place, then the rest most recent first.
fn order(presentation: &task_state::Presentation) -> Order {
    let at = presentation.since.clone();
    match (presentation.block, presentation.queue) {
        (Block::ForYou, _) => (0, 0, at.unwrap_or_else(|| "~".into()), std::cmp::Reverse(String::new())),
        (Block::Waiting, Some(place)) => (0, place, String::new(), std::cmp::Reverse(String::new())),
        _ => (1, 0, String::new(), std::cmp::Reverse(presentation.active_at.clone().or(at).unwrap_or_default())),
    }
}

/// Local ISO times of one Mac compare as text.
type Order = (u8, usize, String, std::cmp::Reverse<String>);

fn request_name(request: &Request) -> String {
    request.title.clone().unwrap_or_else(|| summary(&request.body))
}

/// A waiting line of a group without header names the group after its fact.
fn append_fact(row: &mut super::Row, name: &str, context: &RenderContext<'_>) {
    match context.client {
        super::Client::Ios => match row.children.iter_mut().find(|child| child.id == "activity") {
            Some(line) => line.title = format!("{} · {name}", line.title),
            None => {
                let mut line = common::row("activity", "node", Icon::Notice, name.into(), None);
                line.link = None;
                row.children.push(line);
            }
        },
        _ => row.subtitle = Some(match row.subtitle.take() {
            Some(text) => format!("{text} · {name}"),
            None => name.into(),
        }),
    }
}

fn group_row(request: &Request, tasks: &[&Task], context: &RenderContext<'_>) -> super::Row {
    let mut row = common::row(&format!("group:{}", request.id), &format!("request:{}", request.id), Icon::Mission,
        request_name(request), None);
    row.depth = 1;
    row.trailing = context.live.request_times.get(&request.id.0)
        .map(|at| Trailing::Time { at: at.clone(), format: super::TimeFormat::Relative });
    if let Some(project) = context.index.request_project(&request.id) {
        row.badges.push(super::Label { text: project.into(), tone: Tone::Neutral, elapsed: None });
    }
    row.link = tasks.iter().max_by_key(|task| task.revision).map(|task| super::Link::View { key: format!("task:{}", task.id) });
    if context.client == super::Client::Tui {
        row.badges.insert(0, super::Label {
            text: tf!(context.lang, "{count} tasks", "{count} tareas", count = tasks.len()),
            tone: Tone::Neutral,
            elapsed: None,
        });
    }
    row
}

fn entry_row(task: &Task, depth: u8, shown: &BTreeMap<&TaskId, task_state::Presentation>, context: &RenderContext<'_>) -> super::Row {
    let mut row = presented_row(task, &shown[&task.id], context);
    row.model = task_model_presentation(&task.id, context);
    row.depth = depth;
    row
}

pub(super) fn list(key: &str, context: &RenderContext<'_>) -> Result<View, BuildError> {
    let page = if key == "tasks" {
        None
    } else {
        let tail = key
            .strip_prefix("tasks?section=")
            .ok_or(BuildError::InvalidKey)?;
        let (section, after) = tail.split_once("&after=").ok_or(BuildError::InvalidKey)?;
        if !matches!(section, "active" | "finished") || after.parse::<usize>().is_err() {
            return Err(BuildError::InvalidKey);
        }
        Some((
            section,
            after.parse::<usize>().map_err(|_| BuildError::InvalidKey)?,
        ))
    };
    let mut view = common::view(
        "tasks",
        key,
        Some(t!(context.lang, "Tasks", "Tareas").into()),
        context,
        vec![],
    );
    let mut scheduled = common::section(
        "scheduled",
        None,
        vec![Item::Row(common::row(
            "schedules",
            "schedules",
            Icon::Schedule,
            t!(context.lang, "Scheduled Tasks", "Tareas programadas").into(),
            None,
        ))],
    );
    let scheduled_count = context.domain.schedule.schedules.values()
        .filter(|schedule| schedule.deleted_at.is_none()).count() as u32;
    scheduled.count = (scheduled_count > 0).then_some(scheduled_count);
    view.sections.push(scheduled);
    let buckets = request_buckets(context.domain);
    for (id, title, section) in [
        ("active", t!(context.lang, "Active", "Activas"), Bucket::Active),
        ("finished", t!(context.lang, "Completed", "Completadas"), Bucket::Finished),
    ] {
        if section == Bucket::Active {
            let (items, count) = active_items(&buckets, context);
            let mut active = common::section(id, Some(title.into()), items);
            active.count = Some(count);
            view.sections.push(active);
            continue;
        }
        let forest = Forest::new(context.domain, section, &buckets);
        let requests: Vec<_> = context
            .domain
            .requests
            .values()
            .filter(|request| buckets.get(&request.id) == Some(&section) || !forest.tasks_of(&request.id).is_empty())
            .collect();
        let asked = page.filter(|(part, _)| *part == id);
        let after = asked.map_or(0, |(_, after)| after);
        // Ended work is folded: only its title and count travel, and its rows come when it is opened.
        let folded = id != "active" && asked.is_none();
        let shown: Vec<_> = if folded {
            Vec::new()
        } else {
            requests.iter().skip(after).take(20)
                .flat_map(|request| request_items(request, &forest, context))
                .collect()
        };
        let mut section = common::section(id, Some(title.into()), shown);
        section.count = Some(requests.len() as u32);
        section.collapsed = folded;
        if !requests.is_empty() {
            section.actions.extend(clear_completed(context));
        }
        if folded && !requests.is_empty() {
            section.more = Some(super::More {
                key: format!("tasks?section={id}&after=0"),
                remaining: requests.len() as u32,
            });
        } else if requests.len() > after + 20 {
            section.more = Some(super::More {
                key: format!("tasks?section={id}&after={}", after + 20),
                remaining: (requests.len() - after - 20) as u32,
            });
        }
        view.sections.push(section);
    }
    if context.domain.tasks.is_empty() {
        view.empty = Some(Notice {
            text: t!(context.lang, "No tasks yet", "Aún no hay tareas").into(),
            tone: Tone::Neutral,
            action: None,
        });
    }
    Ok(view)
}

/// The header control that hides everything Completed lists, judged by the rule clearing runs.
fn clear_completed(context: &RenderContext<'_>) -> Option<super::Action> {
    if !context.domain.clear_completed_action().enabled {
        return None;
    }
    let mut action = common::action(
        Verb::ClearCompleted,
        "view",
        context.domain.revision,
        t!(context.lang, "Clear Completed", "Limpiar completadas"),
        Style::Secondary,
    )?;
    action.resolves_card = super::resolves::always_resolves(Verb::ClearCompleted);
    action.key = (context.client == super::Client::Tui).then(|| "A".into());
    Some(action)
}

/// What a task that has no card waits for, as a sentence.
fn wait_text(reason: &crate::domain::WaitReason, context: &RenderContext<'_>) -> String {
    use crate::domain::WaitReason as W;
    let lang = context.lang;
    match reason {
        W::BlockedBy { task } => {
            let title = context.domain.tasks.get(task).map_or(task.0.as_str(), |t| t.title.as_str());
            tf!(lang, "Waits for “{title}”", "Espera a «{title}»", title = title)
        }
        W::UpstreamPr { number } => tf!(lang, "Waits for PR #{number} to merge", "Espera al merge de la PR #{number}", number = number),
        W::Answer { card, need } => {
            let need = context.domain.interactions.get(card).map_or_else(|| need.clone(), |card| super::card::shown_need(card, context.domain, lang));
            tf!(lang, "Waits for your answer to “{need}”", "Espera tu respuesta a «{need}»", need = need)
        }
        W::NodeFixing { node, pr } => tf!(lang, "“{node}” is resolving the conflicts on PR #{pr}", "«{node}» está resolviendo los conflictos de la PR #{pr}", node = node, pr = pr),
        W::Merging { pr } => tf!(lang, "Merging PR #{pr}", "Haciendo merge de la PR #{pr}", pr = pr),
        W::Closing => t!(lang, "Closing", "Cerrando").into(),
    }
}

fn now_line(task: &Task, context: &RenderContext<'_>) -> Option<super::Row> {
    let line = |text: String, link: Option<super::Link>| {
        let mut row = common::row("now", "task", Icon::History, text, None);
        row.link = link;
        row
    };
    if let crate::domain::Attention::Wait(reason) = context.attention(task)
        && matches!(
            reason,
            crate::domain::WaitReason::BlockedBy { .. } | crate::domain::WaitReason::UpstreamPr { .. }
                | crate::domain::WaitReason::Answer { .. } | crate::domain::WaitReason::NodeFixing { .. }
        )
    {
        return Some(line(wait_text(&reason, context), None));
    }
    if let Some(work) = task.wrap_up.post_merge_work.as_ref().filter(|_| !task.state.terminal()) {
        return Some(line(tf!(context.lang, "finishing after the merge: {reason}", "terminando tras el merge: {reason}",
            reason = work.reason), None));
    }
    match task.state {
        TaskState::NeedsYou => context.waiting_cards_of(&task.id).next()
            .map(|item| line(
                tf!(context.lang, "question: {title}", "pregunta: {title}", title = item.title),
                Some(super::Link::View { key: super::card::card_key(&item) }),
            )),
        TaskState::AwaitingBlocker => match context.attention(task) {
            crate::domain::Attention::Wait(reason) => Some(line(wait_text(&reason, context), None)),
            _ => None,
        },
        TaskState::ForYourReview => task.documents.keys().last()
            .and_then(|id| context.domain.documents.get(id))
            .map(|document| line(
                tf!(context.lang, "document: {title}", "documento: {title}", title = document.title), None)),
        TaskState::WaitingDependency => task.stack.as_ref()
            .and_then(|stack| context.domain.pull_requests.get(&stack.pr_id))
            .map(|pr| line(
                tf!(context.lang, "waits for PR #{number} to be merged",
                    "espera al merge del PR #{number}", number = pr.number), None)),
        TaskState::Closing => Some(line(if task.cancelled() {
            if task.agent_id.is_some() && task.wrap_up.node.is_none() {
                t!(context.lang, "cancelled; Alondra closes its node", "cancelada; Alondra cierra su nodo").into()
            } else {
                t!(context.lang, "cancelled; Alondra removes what it left", "cancelada; Alondra retira lo que dejó").into()
            }
        } else if task.wrap_up.asked_at.is_some() {
            t!(context.lang, "finished; the node cleans up before Alondra closes it",
                "terminada; el nodo limpia antes de que Alondra lo cierre").into()
        } else {
            t!(context.lang, "accepted; Alondra merges its pull request", "aceptada; Alondra hace merge de su PR").into()
        }, None)),
        TaskState::NotStarted => common::failed_launch(task, context).map(|error| line(
            if error.is_empty() {
                t!(context.lang, "the scheduled run could not launch its node",
                    "la ejecución programada no pudo lanzar su nodo").into()
            } else {
                tf!(context.lang, "the scheduled run could not launch its node: {error}",
                    "la ejecución programada no pudo lanzar su nodo: {error}", error = error)
            }, None)),
        TaskState::Done | TaskState::Cancelled => task.wrap_up.node.map(|end| line(match end {
            NodeEnd::Closed => t!(context.lang, "node closed", "nodo cerrado"),
            NodeEnd::Gone => t!(context.lang, "node already closed", "el nodo ya estaba cerrado"),
            NodeEnd::KeptByUser => t!(context.lang, "node left open: you chose to keep it", "nodo abierto: elegiste dejarlo"),
            NodeEnd::UsersOwn => t!(context.lang, "node left open: it is your own session", "nodo abierto: es tu propia sesión"),
        }.into(), None)),
        TaskState::Running => None,
    }
}

/// “Send crash report”, on a task whose node failed in a way a report explains: it opens the
/// report's preview, whose issue goes as that failure. The terminal is the only client with that
/// preview for now.
fn crash_report_action(task: &Task, context: &RenderContext<'_>) -> Option<super::Action> {
    if context.client != super::Client::Tui || !context.domain.report_failure_action(&task.id).enabled {
        return None;
    }
    let failure = context.domain.task_failure(&task.id)?;
    let mut action = common::view_link("", t!(context.lang, "Send crash report", "Enviar informe del fallo"));
    action.style = Style::Secondary;
    action.role = Some(Style::Secondary.role());
    action.key = Some("!".into());
    action.link = Some(super::Link::Local { local: super::LocalLink::CrashReport { failure: failure.word().into() } });
    Some(action)
}

/// The folder the task works in: the worktree Alondra made for it, else one its node took over or
/// entered; never a subagent's.
fn folder(task: &Task) -> Option<&String> {
    use crate::domain::WorktreeOrigin;
    let rank = |origin: WorktreeOrigin| match origin {
        WorktreeOrigin::Task => Some(0),
        WorktreeOrigin::Inherited => Some(1),
        WorktreeOrigin::Entered => Some(2),
        WorktreeOrigin::Subagent => None,
    };
    task.worktrees.iter().filter_map(|(path, worktree)| rank(worktree.origin).map(|rank| (rank, path))).min().map(|(_, path)| path)
}

pub(super) fn detail(id: &str, context: &RenderContext<'_>) -> Result<View, BuildError> {
    common::id(id)?;
    let task = context
        .domain
        .tasks
        .get(&TaskId(id.into()))
        .ok_or(BuildError::NotFound)?;
    let mut view = common::view(
        "task",
        &format!("task:{id}"),
        Some(task.title.clone()),
        context,
        vec![],
    );
    let request = context.domain.requests.get(&task.request_id);
    let mut facts = vec![Fact {
        kind: Some(FactKind::State),
        label: t!(context.lang, "State", "Estado").into(),
        value: detail_state(task, context),
        tone: Some(task_state::present(task, context).tone),
        link: None,
    }];
    let mut model_readings = vec![];
    if let Some(model) = task_model_presentation(&task.id, context) {
        model_readings.push(Fact { kind: None, label: t!(context.lang, "Requested model", "Modelo solicitado").into(), value: model.requested, tone: None, link: None });
        if let Some(effective) = model.effective { model_readings.push(Fact { kind: None, label: t!(context.lang, "Effective model", "Modelo efectivo").into(), value: effective, tone: None, link: None }); }
        if let Some(pending) = model.pending { model_readings.push(Fact { kind: None, label: t!(context.lang, "Pending model", "Modelo pendiente").into(), value: pending, tone: Some(Tone::Warning), link: None }); }
        if let Some(reason) = model.reason { model_readings.push(Fact { kind: None, label: t!(context.lang, "Reason", "Motivo").into(), value: reason, tone: None, link: None }); }
    }
    facts.push(Fact {
        kind: Some(FactKind::Request),
        label: t!(context.lang, "Request", "Solicitud").into(),
        value: request
            .and_then(|request| request.title.clone())
            .or_else(|| request.filter(|_| context.index.tasks_of_request(&task.request_id).len() == 1)
                .map(|_| task.title.clone()))
            .unwrap_or_else(|| match request {
                Some(request) if !request.message => summary(&request.body),
                Some(_) => t!(context.lang, "Untitled request", "Petición sin título").into(),
                None => t!(context.lang, "Request details unavailable", "Detalles de la solicitud no disponibles").into(),
            }),
        tone: None,
        link: (context.client != super::Client::Ios).then(|| super::Link::View {
            key: format!("request:{}", task.request_id),
        }),
    });
    if context.client == super::Client::Tui {
        facts.retain(|fact| fact.value != task.title);
    }
    let agent = task.agent_id.as_ref()
        .filter(|agent| !task.state.terminal() && common::current_task(context, agent).is_some_and(|current| current.id == task.id))
        .and_then(|agent| context.domain.agents.get(agent));
    let live = agent.and_then(|agent| context.live.nodes.iter().find(|node| node.agent_id == agent.id.0));
    facts.extend(folder(task).map(|path| Fact { kind: Some(FactKind::Folder), label: t!(context.lang, "Folder", "Carpeta").into(), value: path.clone(), tone: None, link: None }));
    view.sections.push(common::section(
        "header",
        None,
        vec![Item::Facts(Facts { items: facts })],
    ));
    model_readings.extend(live.map(|node| super::task_node::node_readings(node, context)).unwrap_or_default());
    if let Some(&cents) = context.live.task_costs.get(&task.id.0) {
        model_readings.push(Fact { kind: None, label: t!(context.lang, "Cost", "Coste").into(), value: super::task_node::cost(cents, context.lang), tone: None, link: None });
    }
    let readings = model_readings;
    if !readings.is_empty() {
        view.sections.push(common::section("readings", None, vec![Item::Facts(Facts { items: readings })]));
    }
    if let Some(mark) = common::elapsed_mark(task, true, context) {
        let title = match mark.elapsed.as_ref().and_then(|elapsed| elapsed.until.as_ref()) {
            Some(_) => t!(context.lang, "Took", "Duró"),
            None => t!(context.lang, "Running for", "En marcha"),
        };
        let mut row = common::row("elapsed", "task", Icon::History, title.into(), None);
        row.link = None;
        row.badges.push(mark);
        view.sections.push(common::section("elapsed", None, vec![Item::Row(row)]));
    }
    if let Some(step) = context.live.task_history.get(id).and_then(|history| history.last()) {
        let mut row = common::row("since", "task", Icon::History, t!(context.lang, "Since", "Desde").into(), None);
        row.link = None;
        row.trailing = Some(Trailing::Time { at: step.at.clone(), format: super::TimeFormat::Relative });
        view.sections.push(common::section("since", None, vec![Item::Row(row)]));
    }
    if let Some(now) = now_line(task, context) {
        view.sections.push(common::section("now", None, vec![Item::Row(now)]));
    }
    view.sections.extend(agent.and_then(|agent| super::task_node::activity_rows(agent, context, Some(&task.id)))
        .map(|rows| common::section("activity", Some(t!(context.lang, "Right now", "Ahora mismo").into()), rows)));
    if let Some(node) = live {
        view.sections.extend(super::task_node::needs_section(node, context));
        view.sections.extend(super::task_node::work_sections(node, context.lang));
        view.sections.extend(super::task_node::background_sections(node, context));
        view.sections.extend(super::task_node::last_message_section(node, context));
    }
    let waiting: Vec<_> = context.waiting_cards_of(&task.id)
        .map(|item| super::card::from_item(item, context, false).map(Item::Card))
        .collect::<Result<Vec<_>, BuildError>>()?;
    if !waiting.is_empty() {
        view.sections.push(common::section(
            "waiting",
            Some(t!(context.lang, "Waiting for You", "Te espera").into()),
            waiting,
        ));
    }
    if let Some(request) = request {
        let mut section = common::section(
            "request",
            Some(t!(context.lang, "Request", "Solicitud").into()),
            vec![Item::Markdown(Markdown {
                text: request.body.clone(),
                truncated: false,
                more: None,
                commentable: false,
            })],
        );
        section.collapsed = true;
        view.sections.push(section);
    }
    let prs: Vec<_> = merge_order(context, task).into_iter()
        .flat_map(|owner| context.index.prs_of_task(&owner.id).iter().filter_map(|id| context.domain.pull_requests.get(id))
            .map(move |pr| (owner, pr)))
        .enumerate()
        .map(|(index, (owner, pr))| {
            let details = context.live.pull_requests.iter().find(|details| details.id == pr.id.0);
            let title = details.and_then(|details| details.title.as_deref())
                .map(str::to_owned).unwrap_or_else(|| format!("PR #{}", pr.number));
            let branches = details.and_then(|details| details.head_ref.as_deref())
                .map(|head| format!("{head} → {}", details.and_then(|details| details.base_ref.as_deref()).unwrap_or(&pr.base)));
            Item::Row(common::row(
                &pr.id.0,
                &format!("pull_request:{}", pr.id),
                Icon::PullRequest,
                format!("{} · {title}", index + 1),
                branches.map(|branches| if owner.id == task.id { branches }
                    else { format!("{} · {branches}", owner.title) }),
            ))
        })
        .collect();
    if !prs.is_empty() {
        view.sections.push(common::section(
            "pull_requests",
            Some(t!(context.lang, "Pull requests", "Solicitudes de cambios").into()),
            prs,
        ));
    }
    let deps: Vec<_> = task
        .dependencies
        .iter()
        .filter_map(|id| context.domain.tasks.get(id))
        .map(|dep| Item::Row(task_row(dep, context)))
        .collect();
    if !deps.is_empty() {
        view.sections.push(common::section(
            "dependencies",
            Some(t!(context.lang, "Dependencies", "Dependencias").into()),
            deps,
        ));
    }
    let mut task_options = vec![Fact {
            kind: Some(FactKind::Priority),
            label: t!(context.lang, "Priority", "Prioridad").into(),
            value: super::pacing::priority_text(task.priority, context.lang).into(),
            tone: None,
            link: None,
        }, Fact {
            kind: None,
            label: t!(context.lang, "PR handling", "Gestión de PR").into(),
            value: if task.review_prs {
                t!(context.lang, "You review it; accepting it merges it", "La revisas tú; al aceptarla, Alondra hace el merge")
            } else {
                t!(context.lang, "Auto-merge: Alondra merges it once its checks pass", "Merge automático: Alondra hace el merge al pasar los checks")
            }.into(),
            tone: None,
            link: None,
        }];
    if let Some(project) = &task.project {
        task_options.push(Fact {
            kind: None,
            label: t!(context.lang, "Project", "Proyecto").into(),
            value: project.clone(),
            tone: None,
            link: None,
        });
    }
    if let Some(options) = context.live.task_options.get(id) {
        if let Some(effort) = &options.effort {
            task_options.push(Fact {
                kind: None,
                label: t!(context.lang, "Effort", "Esfuerzo").into(),
                value: effort_label(effort, context.lang),
                tone: None,
                link: None,
            });
        }
    }
    let running_model = live.and_then(|node| node.model.as_deref());
    if let Some(launched) = &task.model {
        let model = crate::model_label::launched_model_label(&launched.model);
        let mut value = match launched.chosen_by {
            ModelChooser::Orchestrator => tf!(context.lang, "{model} · chosen by the orchestrator", "{model} · elegido por el orquestador", model = model),
            ModelChooser::User => tf!(context.lang, "{model} · from your Effort choice", "{model} · por tu elección de esfuerzo", model = model),
        };
        if let Some(now) = running_model.filter(|now| model_family(now) != model_family(&launched.model)) {
            value.push_str(&tf!(context.lang, " (now: {model})", " (ahora: {model})", model = crate::model_label::model_label(now)));
        }
        task_options.push(Fact { kind: None, label: t!(context.lang, "Model", "Modelo").into(), value, tone: None, link: None });
    }
    if let Some(account) = account_text(task, context) {
        let provider = super::accounts::provider_name(task.provider);
        task_options.push(Fact { kind: None, label: tf!(context.lang, "{provider} account", "Cuenta de {provider}", provider = provider), value: account, tone: None, link: None });
    }
    view.sections.push(common::section(
        "options",
        Some(t!(context.lang, "Options", "Opciones").into()),
        vec![Item::Facts(Facts { items: task_options })],
    ));
    let documents: Vec<_> = task
        .documents
        .iter()
        .filter_map(|(id, _)| context.domain.documents.get(id))
        .map(|doc| {
            Item::Row(common::row(
                &doc.id.0,
                &format!("document:{}", doc.current_revision),
                Icon::Document,
                doc.title.clone(),
                None,
            ))
        })
        .collect();
    if !documents.is_empty() {
        view.sections.push(common::section(
            "documents",
            Some(t!(context.lang, "Documents", "Documentos").into()),
            documents,
        ));
    }
    let revokes = context.index.grant_actions(context.domain, &task.id);
    view.sections.extend(super::allow_for_task::section(task, &revokes, context.lang));
    let mut actions = vec![];
    if let Some(agent_id) = task.agent_id.as_ref().filter(|id| context.domain.agents.contains_key(*id)) {
        if let Some(mut message) = common::action(
            Verb::Msg, &agent_id.0, context.domain.revision,
            t!(context.lang, "Message", "Mensaje"), Style::Secondary,
        ) {
            match super::action_id::ActionId::new(
                Verb::Msg, &agent_id.0, context.domain.revision, Some(task.id.0.clone()),
            ) {
                Ok(id) => {
                message.id = id.to_string();
                message.key = (context.client == super::Client::Tui).then(|| "r".into());
                message.input = super::Input::Text {
                    placeholder: t!(context.lang, "Write to this task's node…", "Escribe al nodo de esta tarea…").into(),
                    required: true,
                    multiline: true,
                    secret: false,
                };
                message.attachments = true;
                actions.push(message);
                }
                Err(_) => { use std::io::Write as _; let _ = writeln!(std::io::stderr(), "Alondra omitted an action with an invalid identifier ({:?})", Verb::Msg); },
            }
        }
    }
    actions.extend(agent.and_then(|agent| super::task_node::interrupt_action(&agent.id.0, context)));
    actions.extend(relaunch_action(task, context));
    actions.extend(super::pacing::task_actions(task, context));
    actions.extend(super::pacing::priority_action(task, context));
    for available in context.index.task_actions(context.domain, &task.id).unwrap_or_default() {
        if !available.enabled {
            continue;
        }
        let (verb, label, style, key) = match available.kind.as_str() {
            "cancel_task" => (
                Verb::Cancel,
                t!(context.lang, "Cancel Task", "Cancelar tarea"),
                Style::Destructive,
                "x",
            ),
            "reopen_task" => (
                Verb::Reopen,
                t!(context.lang, "Reopen", "Reabrir"),
                Style::Secondary,
                "R",
            ),
            _ => continue,
        };
        if let Some(mut action) =
            common::action(verb, id, available.expected_revision, label, style)
        {
            action.key = (context.client == super::Client::Tui).then(|| key.into());
            if verb == Verb::Cancel {
                action.confirm = Some(Confirm {
                    title: label.into(),
                    message: t!(context.lang, "Cancel this task?", "¿Cancelar esta tarea?").into(),
                    button: label.into(),
                });
            }
            actions.push(action);
        }
    }
    actions.extend(move_action(task, context));
    if !task.state.terminal() && (!context.index.model_catalogs().is_empty() || task.selection.is_some())
    {
        actions.push(common::view_link(&format!("task_selection:{id}"), t!(context.lang, "Change model", "Cambiar modelo")));
    }
    actions.extend(crash_report_action(task, context));
    view.sections.push(common::section(
        "actions",
        None,
        vec![Item::Actions(super::ActionBar { actions })],
    ));
    Ok(view)
}

/// «Move to…»: the accounts the task can go on on, each offered only when the rule the move runs
/// accepts it there.
fn move_action(task: &Task, context: &RenderContext<'_>) -> Option<super::Action> {
    let lang = context.lang;
    let options: Vec<super::ActionOption> = context.domain.accounts.iter().filter_map(|account| {
        let key = crate::accounts::AccountKey::new(account.provider, account.id.clone());
        context.domain.move_task_action(&task.id, key.clone()).enabled.then(|| super::ActionOption {
            id: key.wire_id(),
            label: format!("{} · {}", crate::views::accounts::provider_name(account.provider), crate::views::accounts::name(account, lang)),
            description: Some(t!(lang, "A new node there carries on with the same plan, worktree and branch", "Un nodo nuevo allí sigue con el mismo plan, worktree y rama").into()),
            style: Style::Secondary, input: super::Input::None, key: None,
        })
    }).collect();
    if options.is_empty() {
        return None;
    }
    let stamp = context.domain.task_selection_stamp(&task.id)?;
    let only = (options.len() == 1).then(|| options[0].label.clone());
    let label = match &only {
        Some(account) => tf!(lang, "Move to {account}", "Mover a {account}"),
        None => t!(lang, "Move to…", "Mover a…").to_owned(),
    };
    let mut action = common::action(Verb::MoveTask, &task.id.0, 0, &label, Style::Secondary)?;
    action.id = super::action_id::ActionId::new(Verb::MoveTask, &task.id.0, 0, Some(stamp)).ok()?.to_string();
    action.presentation = super::Presentation::Menu;
    action.key = (context.client == super::Client::Tui).then(|| "V".into());
    action.options = options;
    action.confirm = Some(Confirm {
        title: t!(lang, "Move this task?", "¿Mover esta tarea?").into(),
        message: match &only {
            Some(account) => tf!(lang, "Its current node stops now. A new node on {account} carries on from the task's record, in the same worktree and branch.",
                "Su nodo actual se detiene ya. Un nodo nuevo en {account} sigue desde el registro de la tarea, en el mismo worktree y rama."),
            None => t!(lang, "Its current node stops now. A new node on the account you chose carries on from the task's record, in the same worktree and branch.",
                "Su nodo actual se detiene ya. Un nodo nuevo en la cuenta elegida sigue desde el registro de la tarea, en el mismo worktree y rama.").into(),
        },
        button: t!(lang, "Move", "Mover").into(),
    });
    Some(action)
}

pub(super) fn request(id: &str, context: &RenderContext<'_>) -> Result<View, BuildError> {
    common::id(id)?;
    let request = context
        .domain
        .requests
        .get(&crate::domain::RequestId(id.into()))
        .ok_or(BuildError::NotFound)?;
    let mut view = common::view(
        "request",
        &format!("request:{id}"),
        Some(request.title.clone().unwrap_or_else(|| summary(&request.body))),
        context,
        vec![],
    );
    view.sections.push(common::section(
        "facts",
        None,
        vec![Item::Facts(Facts {
            items: vec![Fact {
                kind: None,
                label: t!(context.lang, "State", "Estado").into(),
                value: request_state(
                    request,
                    &tasks_of(&request.id, context),
                    context,
                ).into(),
                tone: None,
                link: None,
            }],
        })],
    ));
    let tasks = tasks_of(&request.id, context).into_iter()
        .map(|task| Item::Row(task_row(task, context)))
        .collect();
    view.sections.push(common::section(
        "tasks",
        Some(t!(context.lang, "Tasks", "Tareas").into()),
        tasks,
    ));
    view.sections.push(common::section(
        "body",
        None,
        vec![Item::Markdown(Markdown {
            text: request.body.clone(),
            truncated: false,
            more: None,
            commentable: false,
        })],
    ));
    if let Some(answer) = request.answer.as_ref().filter(|_| request.closed && !request.withdrawn) {
        view.sections.push(common::section(
            "answer",
            Some(t!(context.lang, "Answer", "Respuesta").into()),
            vec![Item::Markdown(Markdown { text: answer.clone(), truncated: false, more: None, commentable: false })],
        ));
    }
    if !request.closed && !request.withdrawn {
        if let Some(mut action) = common::action(
            Verb::Withdraw,
            id,
            context.domain.revision,
            t!(context.lang, "Withdraw Request", "Retirar solicitud"),
            Style::Destructive,
        ) {
            action.key = (context.client == super::Client::Tui).then(|| "x".into());
            action.confirm = Some(Confirm {
                title: action.label.clone(),
                message: t!(
                    context.lang,
                    "Withdraw this request?",
                    "¿Retirar esta solicitud?"
                )
                .into(),
                button: action.label.clone(),
            });
            view.sections.push(common::section(
                "actions",
                None,
                vec![Item::Actions(super::ActionBar {
                    actions: vec![action],
                })],
            ));
        }
    }
    Ok(view)
}

/// The account the task's node runs on, by the name the user gave it, once there is more than one
/// to tell apart. A task admitted before accounts were recorded runs on the default one.
fn account_text(task: &crate::domain::Task, context: &RenderContext<'_>) -> Option<String> {
    let accounts = context.index.accounts();
    let account = task.account.as_ref()?;
    if accounts.len() < 2 && account.is_default() {
        return None;
    }
    let status = accounts.iter().find(|status| status.provider == task.provider && status.id == *account);
    let name = status.map_or_else(|| account.as_str().to_owned(), |status| status.name.clone());
    Some(match status.map(|status| &status.billing) {
        Some(crate::accounts::AccountBilling::PayPerUse) => tf!(context.lang, "{name} · paid per use", "{name} · de pago por uso", name = name),
        _ => name,
    })
}


fn task_model_presentation(task: &TaskId, context: &RenderContext<'_>) -> Option<super::TaskModelPresentation> {
    let selection = context.index.task_selection(task)?;
    let model_name = |model: &crate::model_selection::ModelKey, effort: Option<&str>| {
        let provider = super::accounts::provider_name(model.provider);
        match effort { Some(effort) => format!("{provider} · {} · {effort}", model.model), None => format!("{provider} · {}", model.model) }
    };
    let requested = selection.requested.as_ref().map(|model| model_name(model, selection.requested_reasoning_effort.as_deref()))
        .unwrap_or_else(|| t!(context.lang, "Automatic", "Automático").into());
    let effective = selection.effective.as_ref().map(|selection| model_name(&selection.model, selection.reasoning_effort.as_deref()));
    let pending = context.index.pending_task_selection(task);
    Some(super::TaskModelPresentation {
        requested, effective,
        pending: pending.map(|pending| format!("{} · {}", model_name(&pending.model.model, pending.model.reasoning_effort.as_deref()), pending.account.account)),
        reason: pending.map(|pending| match pending.reason.as_str() {
            "User changed model selection" | "User requested a selection change" => t!(context.lang, "Model change requested", "Cambio de modelo solicitado").into(),
            _ => pending.reason.clone(),
        }),
    })
}
