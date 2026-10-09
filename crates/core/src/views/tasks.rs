use super::{
    BuildError, Confirm, Fact, Facts, Icon, Item, Link, LocalLink, Markdown, Notice, RenderContext,
    Style, Tone, Trailing, View, action_id::Verb, common, text::{t, tf},
};
use crate::domain::{DomainState, ModelChooser, NodeEnd, Request, RequestId, Task, TaskId, TaskState};
use std::collections::{BTreeMap, BTreeSet};

/// The task's state as a short line. A task that waits says what for, whatever it waits for.
fn state(task: &Task, context: &RenderContext<'_>) -> String {
    if let Some(paused) = common::paused(task, context) {
        return common::paused_label(paused, context.lang);
    }
    if let Some(queued) = common::queued(task, context) {
        return common::queued_label(queued, context.lang);
    }
    let plain = plain_state(task, context);
    if plain == t!(context.lang, "Waiting", "Esperando")
        && let crate::domain::Attention::Wait(reason) = context.domain.attention(task)
    {
        return wait_text(&reason, context);
    }
    if plain == t!(context.lang, "Not started", "Sin empezar") {
        return not_started(task, context);
    }
    if task.state == TaskState::Running && plain == t!(context.lang, "Working", "Trabajando") {
        match node(task, context) {
            Some(node) if node.reconnecting => return t!(context.lang, "Reconnecting", "Reconectando").into(),
            Some(node) if node.disconnected => return t!(context.lang, "No connection", "Sin conexión").into(),
            Some(node) if node.quiet => return t!(context.lang, "No activity", "Sin actividad").into(),
            _ => {}
        }
    }
    plain.into()
}

/// Why a task that has not started has not, as a fact: what refused its last launch, the task it
/// waits for, or when Alondra tries again; otherwise Alondra starts it when there is room.
fn not_started(task: &Task, context: &RenderContext<'_>) -> String {
    let lang = context.lang;
    if let Some(super::live::LiveStartWait::Retry { at }) = context.live.start_waits.get(&task.id.0) {
        return tf!(lang, "Didn't start: trying again at {at}", "No arrancó: reintento a las {at}", at = at);
    }
    let Some(refusal) = context.live.launch_refusals.get(&task.id.0) else {
        let pending = task.dependencies.iter().chain(&task.delivered_dependencies)
            .filter_map(|id| context.domain.tasks.get(id))
            .find(|dependency| !dependency.state.terminal());
        return match pending {
            Some(dependency) => wait_text(&crate::domain::WaitReason::BlockedBy { task: dependency.id.clone() }, context),
            None => match context.live.start_waits.get(&task.id.0) {
                Some(super::live::LiveStartWait::Room) if context.live.usage_exhausted.is_some() =>
                    t!(lang, "Won't start: Claude usage ran out", "No arranca: se acabó el uso de Claude").into(),
                Some(super::live::LiveStartWait::Room) => t!(lang, "Starting", "Arrancando").into(),
                Some(super::live::LiveStartWait::Retry { .. }) | None =>
                    t!(lang, "Waiting for the orchestrator to launch it", "Esperando a que el orquestador la lance").into(),
            },
        };
    };
    match (refusal.reason.as_str(), refusal.detail.as_deref()) {
        ("workspace_untrusted", Some(folder)) => tf!(lang, "Won't start: Claude Code doesn't trust {folder}",
            "No arranca: Claude Code no confía en {folder}", folder = folder),
        ("workspace_untrusted", None) => t!(lang, "Won't start: Claude Code doesn't trust its folder",
            "No arranca: Claude Code no confía en su carpeta").into(),
        ("usage_exhausted", _) => t!(lang, "Won't start: Claude usage ran out", "No arranca: se acabó el uso de Claude").into(),
        ("disk_low", _) => t!(lang, "Won't start: the disk is almost full", "No arranca: el disco está casi lleno").into(),
        (_, Some(error)) => tf!(lang, "Didn't start: {error}", "No arrancó: {error}", error = error.lines().next().unwrap_or(error)),
        _ => t!(lang, "Didn't start", "No arrancó").into(),
    }
}

/// What Alondra sees of the task's node now: reconnecting, without a connection, or quiet for
/// long enough to say so (the row's time says how long).
fn node<'a>(task: &Task, context: &RenderContext<'a>) -> Option<&'a super::live::LiveNode> {
    task.agent_id.as_ref().and_then(|id| context.live.nodes.iter().find(|node| node.agent_id == id.0))
}

fn plain_state(task: &Task, context: &RenderContext<'_>) -> &'static str {
    let (domain, lang) = (context.domain, context.lang);
    if let Some(label) = common::instruction_label(task, domain, lang) {
        return label;
    }
    if common::failed_launch(task, domain).is_some() {
        return common::launch_failed_label(lang);
    }
    if common::node_closed(task, context) {
        return common::node_closed_label(lang);
    }
    match task.state {
        TaskState::NotStarted => t!(lang, "Not started", "Sin empezar"),
        TaskState::Running => t!(lang, "Working", "Trabajando"),
        TaskState::NeedsYou => t!(lang, "Needs you", "Te necesita"),
        TaskState::ForYourReview => t!(lang, "In review", "En revisión"),
        TaskState::AwaitingBlocker => t!(lang, "Waiting", "Esperando"),
        TaskState::WaitingDependency => t!(lang, "Waiting", "Esperando"),
        TaskState::Closing => t!(lang, "Closing", "Cerrando"),
        TaskState::Done => t!(lang, "Finished", "Terminado"),
        TaskState::Cancelled => t!(lang, "Cancelled", "Cancelado"),
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

pub(super) fn task_row(task: &Task, context: &RenderContext<'_>) -> super::Row {
    let (icon, tone) = common::task_presentation(task.state);
    let mut row = common::row(
        &task.id.0,
        &format!("task:{}", task.id),
        icon,
        task.title.clone(),
        Some(state(task, context).into()),
    );
    row.struck = task.state == TaskState::Cancelled;
    row.badges.push(super::Label { text: state(task, context).into(), tone });
    row.badges.extend(common::scheduled_mark(task, context.domain, context.lang));
    row.actions.extend(relaunch_action(task, context));
    row.actions.extend(super::pacing::task_actions(task, context));
    if context.client == super::Client::Tui {
        row.subtitle = None;
        row.trailing = context.live.task_history.get(&task.id.0)
            .and_then(|history| history.last())
            .map(|step| Trailing::Time { at: step.at.clone(), format: super::TimeFormat::Relative });
        return row;
    }
    row.trailing = Some(Trailing::Count {
        value: context
            .projection
            .inbox
            .iter()
            .filter(|item| item.pending && item.task_id.as_ref() == Some(&task.id))
            .count() as u32,
    });
    row
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Bucket {
    Active,
    Finished,
    Archived,
}

fn bucket(request: &Request, tasks: &[&Task]) -> Bucket {
    if !tasks.is_empty() && tasks.iter().all(|task| task.archived) {
        Bucket::Archived
    } else if request.closed || (!tasks.is_empty() && tasks.iter().all(|task| task.state.terminal())) {
        Bucket::Finished
    } else {
        Bucket::Active
    }
}

/// Where one task is listed: with its whole request when the request is finished or archived; otherwise
/// a task still under way stays with the active request and the ones that ended fold away at the bottom.
fn task_bucket(task: &Task, request: Bucket) -> Bucket {
    match request {
        Bucket::Active if task.state.terminal() && task.archived => Bucket::Archived,
        Bucket::Active if task.state.terminal() => Bucket::Finished,
        whole => whole,
    }
}

/// A message to the orchestrator is listed only once it made tasks: until then, and if it never does, it is
/// in no section.
fn request_buckets(domain: &DomainState) -> BTreeMap<&RequestId, Bucket> {
    let mut tasks: BTreeMap<&RequestId, Vec<&Task>> = BTreeMap::new();
    for task in domain.tasks.values() {
        tasks.entry(&task.request_id).or_default().push(task);
    }
    domain.requests.values()
        .filter_map(|request| {
            let own = tasks.get(&request.id).map_or(&[][..], Vec::as_slice);
            (!request.message || !own.is_empty()).then(|| (&request.id, bucket(request, own)))
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
    let headline = if split {
        t!(context.lang, "The rest is still under way", "El resto sigue en marcha")
    } else {
        request_state(request, tasks, context)
    };
    let id = if split { format!("{}:ended", request.id) } else { request.id.0.clone() };
    let mut row = common::row(&id, &format!("request:{}", request.id), Icon::Mission, title, Some(headline.into()));
    row.trailing = context
        .live
        .request_times
        .get(&request.id.0)
        .map(|at| Trailing::Time {
            at: at.clone(),
            format: super::TimeFormat::Relative,
        });
    if let Some(project) = tasks.iter().filter_map(|task| task.agent_id.as_ref())
        .filter_map(|id| context.live.nodes.iter().find(|node| node.agent_id == id.0))
        .filter_map(|node| node.project.as_deref()).find(|project| *project != "others") {
        row.badges.push(super::Label { text: project.into(), tone: Tone::Neutral });
    }
    row.link = tasks.iter().max_by_key(|task| task.revision).map(|task| super::Link::View {
        key: format!("task:{}", task.id),
    });
    if request.closed && !request.withdrawn && tasks.is_empty() && request.answer.is_some() {
        row.icon = Icon::Check;
    }
    let mut children = task_tree(tasks, forest, context);
    if context.client != super::Client::Ios && !split {
        match tasks {
            [task] => {
                let (icon, tone) = common::task_presentation(task.state);
                row.icon = icon;
                row.struck = task.state == TaskState::Cancelled;
                row.subtitle = None;
                row.badges.insert(0, super::Label { text: state(task, context).into(), tone });
                row.badges.extend(common::scheduled_mark(task, context.domain, context.lang));
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
                let all: Vec<_> = context.domain.tasks.values().filter(|task| task.request_id == request.id).collect();
                let done = all.iter().filter(|task| task.state.terminal()).count();
                row.subtitle = None;
                row.badges.insert(0, super::Label {
                    text: tf!(context.lang, "{done} of {total}", "{done} de {total}", done = done, total = all.len()),
                    tone: Tone::Neutral,
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
    } else if without_tasks && context.live.orchestrator.as_ref().is_some_and(|o| o.can_retry) {
        t!(context.lang, "Waiting for your decision", "Esperando tu decisión")
    } else if without_tasks {
        t!(context.lang, "The orchestrator is splitting it", "El orquestador la está dividiendo")
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
    if context.client == super::Client::Ios {
        row.children.extend(task.agent_id.as_ref()
            .and_then(|id| context.live.nodes.iter().find(|node| node.agent_id == id.0))
            .and_then(super::fleet::activity));
    }
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

fn merge_order<'a>(domain: &'a DomainState, task: &'a Task) -> Vec<&'a Task> {
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
        let next: Vec<_> = domain.tasks.values()
            .filter(|candidate| frontier.iter().any(|parent| candidate.dependencies.contains(&parent.id)))
            .filter(|candidate| seen.insert(candidate.id.clone())).collect();
        order.extend(next.iter().copied());
        frontier = next;
    }
    order
}

pub(super) fn list(key: &str, context: &RenderContext<'_>) -> Result<View, BuildError> {
    let page = if key == "tasks" {
        None
    } else {
        let tail = key
            .strip_prefix("tasks?section=")
            .ok_or(BuildError::InvalidKey)?;
        let (section, after) = tail.split_once("&after=").ok_or(BuildError::InvalidKey)?;
        if !matches!(section, "active" | "finished" | "archived") || after.parse::<usize>().is_err() {
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
        ("finished", t!(context.lang, "Finished", "Terminadas"), Bucket::Finished),
        ("archived", t!(context.lang, "Archived", "Archivadas"), Bucket::Archived),
    ] {
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
    if context.domain.tasks.values().any(|task| task.state.terminal() && !task.archived)
        && let Some(mut action) = common::action(
        Verb::ArchiveDone,
        "view",
        context.domain.revision,
        t!(context.lang, "Archive Completed", "Archivar terminadas"),
        Style::Secondary,
    ) {
        action.key = (context.client == super::Client::Tui).then(|| "A".into());
        view.sections.push(common::section(
            "actions",
            None,
            vec![Item::Actions(super::ActionBar {
                actions: vec![action],
            })],
        ));
    }
    Ok(view)
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
        W::UpstreamPr { number } => tf!(lang, "Waits for PR #{number} to merge", "Espera a que se fusione la PR #{number}", number = number),
        W::Answer { need } => tf!(lang, "Waits for your answer to “{need}”", "Espera tu respuesta a «{need}»", need = need),
        W::NodeFixing { node, pr } => tf!(lang, "“{node}” is resolving the conflicts on PR #{pr}", "«{node}» está resolviendo los conflictos de la PR #{pr}", node = node, pr = pr),
        W::Merging { pr } => tf!(lang, "Merging PR #{pr}", "Fusionando la PR #{pr}", pr = pr),
        W::Closing => t!(lang, "Closing", "Cerrando").into(),
    }
}

fn now_line(task: &Task, context: &RenderContext<'_>) -> Option<super::Row> {
    let line = |text: String, link: Option<super::Link>| {
        let mut row = common::row("now", "task", Icon::History, text, None);
        row.link = link;
        row
    };
    if let crate::domain::Attention::Wait(reason) = context.domain.attention(task)
        && matches!(
            reason,
            crate::domain::WaitReason::BlockedBy { .. } | crate::domain::WaitReason::UpstreamPr { .. }
                | crate::domain::WaitReason::Answer { .. } | crate::domain::WaitReason::NodeFixing { .. }
        )
    {
        return Some(line(wait_text(&reason, context), None));
    }
    match task.state {
        TaskState::NeedsYou => context
            .projection
            .inbox
            .iter()
            .find(|item| item.pending && item.task_id.as_ref() == Some(&task.id))
            .map(|item| line(
                tf!(context.lang, "question: {title}", "pregunta: {title}", title = item.title),
                Some(super::Link::View { key: super::card::card_key(&item) }),
            )),
        TaskState::AwaitingBlocker => match context.domain.attention(task) {
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
                    "espera a que se fusione el PR #{number}", number = pr.number), None)),
        TaskState::Closing => Some(line(if task.wrap_up.asked_at.is_some() {
            t!(context.lang, "finished; the node cleans up before Alondra closes it",
                "terminada; el nodo limpia antes de que Alondra lo cierre").into()
        } else {
            t!(context.lang, "accepted; Alondra merges its pull request", "aceptada; Alondra fusiona su PR").into()
        }, None)),
        TaskState::NotStarted => common::failed_launch(task, context.domain).map(|error| line(
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
        label: t!(context.lang, "State", "Estado").into(),
        value: state(task, context).into(),
        tone: Some(common::task_presentation(task.state).1),
        link: None,
    }];
    if let Some(agent_id) = &task.agent_id {
        facts.push(Fact {
            label: t!(context.lang, "Node", "Nodo").into(),
            value: common::agent_label(agent_id, context),
            tone: None,
            link: Some(super::Link::View {
                key: format!("node:{agent_id}"),
            }),
        });
    }
    facts.push(Fact {
        label: t!(context.lang, "Request", "Solicitud").into(),
        value: request
            .and_then(|request| request.title.clone())
            .or_else(|| request.filter(|_| context.domain.tasks.values().filter(|other| other.request_id == task.request_id).count() == 1)
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
    view.sections.push(common::section(
        "header",
        None,
        vec![Item::Facts(Facts { items: facts })],
    ));
    if let Some(step) = context.live.task_history.get(id).and_then(|history| history.last()) {
        let mut row = common::row("since", "task", Icon::History, t!(context.lang, "Since", "Desde").into(), None);
        row.link = None;
        row.trailing = Some(Trailing::Time { at: step.at.clone(), format: super::TimeFormat::Relative });
        view.sections.push(common::section("since", None, vec![Item::Row(row)]));
    }
    if let Some(now) = now_line(task, context) {
        view.sections.push(common::section("now", None, vec![Item::Row(now)]));
    }
    if let Some(node) = task.agent_id.as_ref()
        .filter(|agent| !task.state.terminal() && common::current_task(context.domain, agent).is_some_and(|current| current.id == task.id))
        .and_then(|agent| context.live.nodes.iter().find(|node| node.agent_id == agent.0)) {
        view.sections.extend(super::fleet::work_sections(node, context.lang));
    }
    if let Some(history) = context
        .live
        .task_history
        .get(id)
        .filter(|items| !items.is_empty())
    {
        let rows = history
            .iter()
            .map(|step| {
                let title = match step.state.as_str() {
                    "not_started" => t!(context.lang, "Not started", "Sin empezar"),
                    "running" => t!(context.lang, "Working", "Trabajando"),
                    "needs_you" => t!(context.lang, "Needs you", "Te necesita"),
                    "in_review" | "for_your_review" => t!(context.lang, "In review", "En revisión"),
                    "awaiting_blocker" => t!(context.lang, "Waiting", "Esperando"),
                    "waiting_dependency" => t!(context.lang, "Waiting", "Esperando"),
                    "closing" => t!(context.lang, "Closing", "Cerrando"),
                    "done" => t!(context.lang, "Finished", "Terminado"),
                    "cancelled" => t!(context.lang, "Cancelled", "Cancelado"),
                    _ => step.state.as_str(),
                };
                let mut row = common::row(
                    &step.at,
                    "task",
                    Icon::History,
                    title.into(),
                    Some(step.summary.clone()),
                );
                row.link = None;
                row.trailing = Some(Trailing::Time {
                    at: step.at.clone(),
                    format: super::TimeFormat::Relative,
                });
                Item::Row(row)
            })
            .collect();
        view.sections.push(common::section(
            "history",
            Some(t!(context.lang, "History", "Historial").into()),
            rows,
        ));
    }
    let waiting: Vec<_> = context
        .projection
        .inbox
        .iter()
        .filter(|item| item.pending && item.task_id.as_ref() == Some(&task.id))
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
        view.sections.push(common::section(
            "request",
            Some(t!(context.lang, "Request", "Solicitud").into()),
            vec![Item::Markdown(Markdown {
                text: request.body.clone(),
                truncated: false,
                more: None,
                commentable: false,
            })],
        ));
    }
    let prs: Vec<_> = merge_order(context.domain, task).into_iter()
        .flat_map(|owner| context.domain.pull_requests.values().filter(move |pr| pr.task_id == owner.id)
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
            label: t!(context.lang, "Priority", "Prioridad").into(),
            value: super::pacing::priority_text(task.priority, context.lang).into(),
            tone: None,
            link: None,
        }, Fact {
            label: t!(context.lang, "PR handling", "Gestión de PR").into(),
            value: if task.review_prs {
                t!(context.lang, "You review it; accepting it merges it", "La revisas tú; al aceptarla, Alondra la fusiona")
            } else {
                t!(context.lang, "Auto-merge: Alondra merges it once its checks pass", "Merge automático: Alondra la fusiona al pasar los checks")
            }.into(),
            tone: None,
            link: None,
        }];
    if let Some(options) = context.live.task_options.get(id) {
        if let Some(project) = options.project.as_ref().filter(|project| !project.is_empty()) {
            task_options.push(Fact {
                label: t!(context.lang, "Project", "Proyecto").into(),
                value: project.clone(),
                tone: None,
                link: None,
            });
        }
        if let Some(effort) = &options.effort {
            task_options.push(Fact {
                label: t!(context.lang, "Effort", "Esfuerzo").into(),
                value: effort_label(effort, context.lang),
                tone: None,
                link: None,
            });
        }
    }
    if let Some(launched) = &task.model {
        let model = crate::model_label::launched_model_label(&launched.model);
        let value = match launched.chosen_by {
            ModelChooser::Orchestrator => tf!(context.lang, "{model} · chosen by the orchestrator", "{model} · elegido por el orquestador", model = model),
            ModelChooser::User => tf!(context.lang, "{model} · from your Effort choice", "{model} · por tu elección de esfuerzo", model = model),
        };
        task_options.push(Fact { label: t!(context.lang, "Model", "Modelo").into(), value, tone: None, link: None });
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
    if context.client == super::Client::Tui
        && let Some(short_id) = task
            .agent_id
            .as_ref()
            .and_then(|agent| context.live.nodes.iter().find(|node| node.agent_id == agent.0))
            .and_then(|node| node.short_id.as_ref())
    {
        let mut attach = common::row(
            "attach",
            "node",
            Icon::Node,
            t!(context.lang, "Attach", "Conectar").into(),
            None,
        );
        attach.link = Some(Link::Local {
            local: LocalLink::Attach {
                session: short_id.clone(),
            },
        });
        view.sections
            .push(common::section("attach", None, vec![Item::Row(attach)]));
    }
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
                };
                message.attachments = true;
                actions.push(message);
                }
                Err(_) => eprintln!("Alondra omitted an action with an invalid identifier ({:?})", Verb::Msg),
            }
        }
    }
    actions.extend(relaunch_action(task, context));
    actions.extend(super::pacing::task_actions(task, context));
    actions.extend(super::pacing::priority_action(task, context));
    for available in context.domain.task_actions(&task.id).unwrap_or_default() {
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
            "archive_task" => (
                Verb::Archive,
                t!(context.lang, "Archive", "Archivar"),
                Style::Secondary,
                "a",
            ),
            "unarchive_task" => (
                Verb::Unarchive,
                t!(context.lang, "Unarchive", "Desarchivar"),
                Style::Secondary,
                "a",
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
    view.sections.push(common::section(
        "actions",
        None,
        vec![Item::Actions(super::ActionBar { actions })],
    ));
    Ok(view)
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
                label: t!(context.lang, "State", "Estado").into(),
                value: request_state(
                    request,
                    &context.domain.tasks.values().filter(|task| task.request_id == request.id).collect::<Vec<_>>(),
                    context,
                ).into(),
                tone: None,
                link: None,
            }],
        })],
    ));
    let tasks = context
        .domain
        .tasks
        .values()
        .filter(|task| task.request_id == request.id)
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
