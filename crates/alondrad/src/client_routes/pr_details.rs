//! What GitHub says about each open pull request that the domain does not keep: its
//! title, checks and whether it merges cleanly. Refreshed in the background and read from
//! memory, so a client's poll never waits on `gh`.
use crate::locks::Poisonable as _;
use alondra_core::domain::{
    Actor, Command, CommandEnvelope, DeliveryTarget, DomainError, DomainState, Instruction, InteractionPayload,
    PrState, PullRequest, PROTOCOL_VERSION,
};
use alondra_core::pr::Check;
use alondra_core::views::pr_images::{PrImage, Shown};
use crate::pr_images::GitHubImages;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub(crate) type Viewed<'a> = Pin<Box<dyn Future<Output = Option<Value>> + Send + 'a>>;

/// `gh pr view --json`, behind a pointer so a test can answer for GitHub.
pub(crate) trait PrView: Send + Sync + 'static {
    fn view<'a>(&'a self, url: &'a str) -> Viewed<'a>;
}

pub(crate) struct GhView;

/// GitHub as a test sets it: `None` is GitHub not answering.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct FakePrView(pub Mutex<Option<Value>>);

#[cfg(test)]
impl FakePrView {
    pub(crate) fn answer(&self, view: Option<Value>) {
        *self.0.lock().unwrap() = view;
    }
}

#[cfg(test)]
impl PrView for FakePrView {
    fn view<'a>(&'a self, _: &'a str) -> Viewed<'a> {
        let view = self.0.lock().unwrap().clone();
        Box::pin(async move { view })
    }
}

impl PrView for GhView {
    fn view<'a>(&'a self, url: &'a str) -> Viewed<'a> {
        Box::pin(crate::prs::Gh::view(&crate::prs::GhCli, url))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub(crate) struct Details {
    pub title: Option<String>,
    pub body: Option<String>,
    pub head_ref: Option<String>,
    pub head_sha: Option<String>,
    pub base_ref: Option<String>,
    pub checks: Vec<Check>,
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: u64,
    pub conflicts: bool,
    pub mergeability_unknown: bool,
    /// The images of `body` as the daemon brought them from GitHub.
    pub images: Vec<PrImage>,
    pub draft: bool,
    /// GitHub's `BLOCKED`: something other than conflicts stops the merge, such as a required review.
    pub blocked: bool,
    #[serde(skip)]
    observed_at: Option<i64>,
}

/// Whether a PR merges into its base, as far as GitHub has told us.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Merge {
    Clean,
    Conflicts,
    Unknown,
}

const CLEAN_CACHE_MILLIS: i64 = 60_000;

impl Details {
    fn fresh_at(&self, now: i64) -> bool {
        self.observed_at.and_then(|at| now.checked_sub(at))
            .is_some_and(|age| (0..CLEAN_CACHE_MILLIS).contains(&age))
    }

    fn matches_revision(&self, head: &str, base: &str) -> bool {
        self.head_sha.as_deref() == Some(head) && self.base_ref.as_deref() == Some(base)
    }

    fn merge(&self) -> Merge {
        if self.conflicts {
            Merge::Conflicts
        } else if self.mergeability_unknown {
            Merge::Unknown
        } else {
            Merge::Clean
        }
    }
}

#[derive(Default)]
pub(crate) struct PrDetails {
    /// A holder that panics leaves it whole, so it is recovered: a cache of GitHub reads the next
    /// refresh writes again.
    cache: Mutex<Observations>,
    refresh: tokio::sync::Mutex<()>,
    #[cfg(test)]
    clock: Mutex<Option<Arc<dyn Fn() -> i64 + Send + Sync>>>,
}

#[derive(Default)]
struct Observations {
    next: u64,
    changes: u64,
    entries: BTreeMap<String, Observation>,
}

#[derive(Default)]
struct Observation {
    accepted: u64,
    details: Option<Details>,
    images: Option<Images>,
}

/// The images brought for one description at one head, read by request `read`.
struct Images {
    body: String,
    head: String,
    brought: Vec<PrImage>,
    retry: bool,
    read: u64,
}

impl PrDetails {
    fn now(&self) -> i64 {
        #[cfg(test)]
        if let Some(clock) = self.clock.lock_or_recover("the pull request clock").clone() { return clock(); }
        alondra_core::now_millis()
    }

    /// Grows whenever the details any pull request shows change.
    pub(crate) fn version(&self) -> u64 {
        self.cache.lock_or_recover("the pull request details").changes
    }

    pub(crate) fn get(&self, pr_id: &str) -> Option<Details> {
        let cache = self.cache.lock_or_recover("the pull request details");
        let entry = cache.entries.get(pr_id)?;
        let mut details = entry.details.clone()?;
        match entry.images.as_ref() {
            Some(images) if images.read > entry.accepted && !images.brought.is_empty() => {
                details.body = Some(images.body.clone());
                details.images = images.brought.clone();
            }
            Some(images) if details.body.as_deref() == Some(images.body.as_str()) => details.images = images.brought.clone(),
            _ => {}
        }
        Some(details)
    }

    /// Brings from GitHub the images of each open pull request's description not brought yet.
    pub(crate) async fn bring_images(&self, state: &DomainState, github: &dyn GitHubImages, home: &std::path::Path) {
        for pr in state.pull_requests.values().filter(|pr| pr.state == PrState::Open) {
            self.bring(pr, github, home).await;
        }
    }

    /// The images of `pr`'s description as last read, brought now when they were not yet.
    async fn bring(&self, pr: &PullRequest, github: &dyn GitHubImages, home: &std::path::Path) -> Vec<PrImage> {
        let (body, head, read) = {
            let cache = self.cache.lock_or_recover("the pull request details");
            let Some(entry) = cache.entries.get(&pr.id.0) else { return vec![] };
            let Some(details) = entry.details.as_ref() else { return vec![] };
            let Some(body) = details.body.clone() else { return vec![] };
            (body, details.head_sha.clone().unwrap_or_else(|| pr.head.clone()), entry.accepted)
        };
        self.bring_for(pr, &body, &head, read, github, home).await
    }

    /// The images of `body`, a description of `pr` at `head` read by request `read`, kept for the
    /// clients: a description read after the details shows in their place until they read it too.
    async fn bring_for(&self, pr: &PullRequest, body: &str, head: &str, read: u64, github: &dyn GitHubImages, home: &std::path::Path) -> Vec<PrImage> {
        let target = crate::pr_images::Target { host: &pr.host, repository: &pr.repository, number: pr.number, head };
        let known: BTreeMap<String, Shown> = {
            let cache = self.cache.lock_or_recover("the pull request details");
            let images = cache.entries.get(&pr.id.0).and_then(|entry| entry.images.as_ref());
            if let Some(images) = images.filter(|images| images.body == body && images.head == head && !images.retry && images.read >= read) {
                return images.brought.clone();
            }
            images.map(|images| crate::pr_images::lasting(&target, &images.brought, images.head == head)).unwrap_or_default()
        };
        let brought = crate::pr_images::bring(github, home, &target, body, &known).await;
        let mut cache = self.cache.lock_or_recover("the pull request details");
        let Some(entry) = cache.entries.get_mut(&pr.id.0) else { return brought.images };
        if entry.images.as_ref().is_some_and(|images| images.read > read) {
            return brought.images;
        }
        let changed = entry.images.as_ref().is_none_or(|images| images.body != body || images.brought != brought.images);
        entry.images = Some(Images { body: body.to_owned(), head: head.to_owned(), brought: brought.images.clone(), retry: brought.retry, read });
        if changed { cache.changes += 1; }
        brought.images
    }

    fn begin(&self, pr_id: &str) -> u64 {
        let mut cache = self.cache.lock_or_recover("the pull request details");
        cache.next = cache.next.checked_add(1).expect("pull request observation counter exhausted");
        let request = cache.next;
        cache.entries.entry(pr_id.to_owned()).or_default();
        request
    }

    fn complete(&self, pr_id: &str, request: u64, mut details: Details) -> bool {
        let mut cache = self.cache.lock_or_recover("the pull request details");
        let Some(entry) = cache.entries.get_mut(pr_id).filter(|entry| entry.accepted <= request) else { return false };
        entry.accepted = request;
        details.observed_at = Some(self.now());
        entry.details = Some(details);
        cache.changes += 1;
        true
    }

    #[cfg(test)]
    pub(crate) fn record(&self, pr_id: &str, details: Details) {
        self.complete(pr_id, self.begin(pr_id), details);
    }

    fn keep_only(&self, state: &DomainState) {
        let mut cache = self.cache.lock_or_recover("the pull request details");
        let before = cache.entries.len();
        cache.entries.retain(|id, _| state.pull_requests.keys().any(|pr| &pr.0 == id));
        if cache.entries.len() != before { cache.changes += 1; }
    }

    pub(crate) async fn refresh(&self, state: &DomainState, source: &dyn PrView) {
        let _refresh = self.refresh.lock().await;
        self.keep_only(state);
        let pending = Mutex::new(state.pull_requests.values().filter(|pr| pr.state == PrState::Open));
        let read = || async {
            loop {
                let Some(pr) = pending.lock_or_recover("the pull request refresh").next() else { break };
                let request = self.begin(&pr.id.0);
                let url = crate::domain_github::pr_url(&pr.host, &pr.repository, pr.number);
                if let Some(view) = source.view(&url).await {
                    self.complete(&pr.id.0, request, parse(&pr.id.0, pr.number, &view));
                }
            }
        };
        tokio::join!(read(), read());
    }
}

impl super::Client {
    pub(crate) fn use_pr_view(&self, view: Arc<dyn PrView>) {
        *self.pr_view.lock_or_recover("the pull request view") = view;
    }

    #[cfg(test)]
    pub(crate) fn use_pr_images(&self, github: Arc<dyn GitHubImages>) {
        *self.pr_images.lock_or_recover("the pull request images") = github;
    }

    pub(crate) async fn bring_pr_images(&self, state: &DomainState) {
        let github = self.pr_images.lock_or_recover("the pull request images").clone();
        self.details.bring_images(state, &*github, &self.home).await;
    }

    /// The images `pr`'s description cites that do not exist where it says, read from GitHub now:
    /// the description may have changed a moment ago.
    async fn missing_images(&self, pr: &PullRequest) -> Vec<PrImage> {
        let view = self.pr_view.lock_or_recover("the pull request view").clone();
        let url = crate::domain_github::pr_url(&pr.host, &pr.repository, pr.number);
        let read = self.details.begin(&pr.id.0);
        let Some(answer) = view.view(&url).await else { return vec![] };
        let fresh = parse(&pr.id.0, pr.number, &answer);
        let Some(body) = fresh.body else { return vec![] };
        let head = fresh.head_sha.unwrap_or_else(|| pr.head.clone());
        let github = self.pr_images.lock_or_recover("the pull request images").clone();
        self.details.bring_for(pr, &body, &head, read, &*github, &self.home).await.into_iter()
            .filter(|image| matches!(&image.shown, Shown::Unavailable { problem, .. } if problem.missing()))
            .collect()
    }

    /// Whether `pr` merges at `head`, asked up to `attempts` times while GitHub is still computing.
    /// A recent clean answer for this head and base is trusted; a conflict or unknown is asked again, since a
    /// base that moved can clear either without a new head.
    pub(crate) async fn merge_state(&self, pr: &PullRequest, head: &str, attempts: usize, pause: Duration) -> Merge {
        if self.details.get(&pr.id.0)
            .is_some_and(|details| details.matches_revision(head, &pr.base) && details.merge() == Merge::Clean && details.fresh_at(self.details.now()))
        {
            return Merge::Clean;
        }
        let view = self.pr_view.lock_or_recover("the pull request view").clone();
        let url = crate::domain_github::pr_url(&pr.host, &pr.repository, pr.number);
        for attempt in 0..attempts {
            if attempt > 0 {
                tokio::time::sleep(pause).await;
            }
            let request = self.details.begin(&pr.id.0);
            let Some(answer) = view.view(&url).await else { return Merge::Unknown };
            let details = parse(&pr.id.0, pr.number, &answer);
            if !details.matches_revision(head, &pr.base) { continue; }
            let merge = details.merge();
            if !self.details.complete(&pr.id.0, request, details) {
                let Some(latest) = self.details.get(&pr.id.0).filter(|details| details.matches_revision(head, &pr.base)) else {
                    return Merge::Unknown;
                };
                let accepted = latest.merge();
                if latest.fresh_at(self.details.now()) && accepted != Merge::Unknown { return accepted; }
                return if merge == Merge::Conflicts { Merge::Conflicts } else { Merge::Unknown };
            }
            if merge != Merge::Unknown {
                return merge;
            }
        }
        Merge::Unknown
    }

    /// A node presenting a PR that GitHub says conflicts is told to resolve it first: the card and
    /// its push would only reach the user to be handed back a minute later.
    pub(crate) async fn verify_presentation(&self, command: &Command) -> Result<(), DomainError> {
        let Command::PresentResult { targets, interaction_id, .. } = command else { return Ok(()) };
        let Ok(state) = self.domain.state() else { return Ok(()) };
        if state.interactions.contains_key(interaction_id) {
            return Ok(());
        }
        for target in targets {
            let DeliveryTarget::PullRequest { pr_id, head } = target else { continue };
            let Some(pr) = state.pull_requests.get(pr_id).filter(|pr| pr.state == PrState::Open) else { continue };
            let head = if head.is_empty() { &pr.head } else { head };
            if *head != pr.head {
                continue;
            }
            if self.merge_state(pr, head, PRESENT_ATTEMPTS, PRESENT_PAUSE).await == Merge::Conflicts {
                return Err(DomainError::new("pr_conflicts", presentation_conflicts_text(pr)));
            }
            let missing = self.missing_images(pr).await;
            if !missing.is_empty() {
                return Err(DomainError::new("pr_images_missing", missing_images_text(pr, &missing)));
            }
        }
        Ok(())
    }

    /// A node presenting a head Alondra has not seen has usually just pushed it, before the
    /// watcher's next round: Alondra asks GitHub for the PR and records what it reports, so the
    /// domain judges the head against the PR's real commit. GitHub can take a moment to move the
    /// PR after a push, so it is asked again while it still reports another commit.
    pub(crate) async fn catch_up_heads(&self, command: &Command) {
        let Command::PresentResult { targets, interaction_id, .. } = command else { return };
        let Ok(state) = self.domain.state() else { return };
        if state.interactions.contains_key(interaction_id) {
            return;
        }
        let view = self.pr_view.lock_or_recover("the pull request view").clone();
        for target in targets {
            let DeliveryTarget::PullRequest { pr_id, head } = target else { continue };
            let Some(pr) = state.pull_requests.get(pr_id).filter(|pr| pr.state == PrState::Open) else { continue };
            if head.is_empty() || *head == pr.head {
                continue;
            }
            let url = crate::domain_github::pr_url(&pr.host, &pr.repository, pr.number);
            for attempt in 0..HEAD_ATTEMPTS {
                if attempt > 0 {
                    tokio::time::sleep(HEAD_PAUSE).await;
                }
                let Some(facts) = view.view(&url).await.and_then(|answer| crate::domain_github::parse(&answer).ok()) else { continue };
                if let Err(error) = crate::domain_github::observe(&self.domain, pr_id, &facts) {
                    log!("could not record what GitHub reports of {}: {error}", pr.id);
                    return;
                }
                if facts.head == *head || facts.state != PrState::Open {
                    break;
                }
            }
        }
    }
}

const PRESENT_ATTEMPTS: usize = 3;
const PRESENT_PAUSE: Duration = Duration::from_secs(2);
const HEAD_ATTEMPTS: usize = 3;
const HEAD_PAUSE: Duration = if cfg!(test) { Duration::ZERO } else { Duration::from_secs(2) };

fn presentation_conflicts_text(pr: &PullRequest) -> String {
    let url = crate::domain_github::pr_url(&pr.host, &pr.repository, pr.number);
    let base = &pr.base;
    format!("GitHub reports that {url} conflicts with {base}, so nothing was presented: bring {base} into the branch, \
        resolve the conflicts, run the tests and push, then present the PR again with present_result and its new head. \
        If you cannot resolve them, ask the user.")
}

fn missing_images_text(pr: &PullRequest, missing: &[PrImage]) -> String {
    let url = crate::domain_github::pr_url(&pr.host, &pr.repository, pr.number);
    let mut each: Vec<String> = missing.iter().map(|image| match &image.shown {
        Shown::Unavailable { problem, .. } => format!("{} ({})", image.url, problem.text(alondra_core::views::Lang::En)),
        Shown::Stored { .. } => image.url.clone(),
    }).collect();
    each.dedup();
    let host = alondra_core::domain::normalize_pr_host(&pr.host);
    format!("The description of {url} cites images that do not exist, so nothing was presented: {}. Commit each image to the \
        branch and cite it at the commit you present (https://{host}/{}/blob/<commit>/<path>?raw=true), or take it out of \
        the description with gh pr edit, then present the PR again.", each.join("; "), pr.repository)
}

/// A presented PR that GitHub says conflicts goes back to its Agent with the words of the user's
/// Resolve conflicts, once per head and without a card: the user only hears of it if the Agent
/// asks.
pub(crate) fn hand_back_conflicts(domain: &crate::domain_runtime::DomainRuntime, details: &PrDetails, name: &str) {
    let Ok(state) = domain.state() else { return };
    for pr_id in state.pull_requests.keys() {
        let Ok(state) = domain.state() else { return };
        let Some(pr) = state.pull_requests.get(pr_id) else { continue };
        if !details.get(&pr.id.0).is_some_and(|details| details.conflicts && details.matches_revision(&pr.head, &pr.base)) || conflicts_sent(&state, pr) {
            continue;
        }
        let Some(presentation) = crate::views::actions::pr_presentation(&state, pr) else { continue };
        let InteractionPayload::Delivery { targets, acceptance, .. } = &presentation.payload else { continue };
        if acceptance.as_ref().is_some_and(|acceptance| acceptance.valid) || state.handed_back(presentation).is_some() {
            continue;
        }
        let envelope = CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.data_epoch.clone(),
            command_id: crate::views::actions::conflicts_command_id(pr),
            expected_revision: state.revision,
            command: Command::SubmitFeedback {
                attachments: vec![],
                presentation_id: presentation.id.clone(),
                targets: targets.clone(),
                body: crate::domain_messages::conflicts_text(name, pr),
                instruction: Instruction::Conflicts,
            },
        };
        if let Err(error) = domain.execute(&Actor::System, &envelope, None, || Ok(())) {
            log!("could not hand the conflicts of {} back to its node: {error}", pr.id);
        }
    }
}

fn conflicts_sent(state: &DomainState, pr: &alondra_core::domain::PullRequest) -> bool {
    state.interactions.values().any(|interaction| matches!(&interaction.payload,
        InteractionPayload::Delivery { targets, feedback, .. }
            if feedback.iter().any(|f| f.instruction == Instruction::Conflicts && alondra_core::domain::merge_failure(&f.body).is_none())
                && targets.iter().any(|target| matches!(target, DeliveryTarget::PullRequest { pr_id, head }
                    if pr_id == &pr.id && head == &pr.head))))
}

fn parse(id: &str, number: u64, view: &Value) -> Details {
    let stub = alondra_core::pr::PullRequest::opened(0, &alondra_core::pr::canonical("o/r", number), id, None, 0)
        .expect("a canonical URL parses");
    let card = crate::prs::refreshed(&stub, view, None);
    Details {
        title: Some(card.title.clone()),
        body: card.body.clone(),
        head_ref: card.head.clone(),
        head_sha: card.head_sha.clone(),
        base_ref: card.base.clone(),
        conflicts: card.conflicts(),
        mergeability_unknown: card.mergeability_unknown(),
        draft: card.draft,
        blocked: card.merge_state.as_deref() == Some("BLOCKED"),
        additions: card.additions,
        deletions: card.deletions,
        changed_files: card.changed_files,
        checks: card.checks,
        images: vec![],
        observed_at: None,
    }
}

#[cfg(test)]
mod tests {
    use super::{Details, PrDetails, hand_back_conflicts, parse};
    use alondra_core::domain::{
        Actor, Command, CommandEnvelope, DeliveryTarget, Instruction, InteractionPayload, MergeMethod, PrState,
    };
    use serde_json::json;

    struct Home(std::path::PathBuf);
    impl Drop for Home {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
    }

    fn run(domain: &crate::domain_runtime::DomainRuntime, actor: Actor, id: &str, command: Command) {
        let state = domain.state().unwrap();
        domain.execute(&actor, &CommandEnvelope { protocol_version: 2, data_epoch: state.data_epoch.clone(),
            command_id: id.into(), expected_revision: state.revision, command }, None, || Ok(())).unwrap();
    }

    fn present(domain: &crate::domain_runtime::DomainRuntime, id: &str, head: &str) {
        run(domain, Actor::Agent { agent_id: "agent".into() }, id, Command::PresentResult { checklist: Vec::new(), attachments: vec![],
            task_id: "task".into(), interaction_id: id.into(), body: "Ready".into(), is_final: true, nothing_to_review: false,
            targets: vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: head.into() }], card: alondra_core::domain::CardSpec::delivery("Review the result", &[], true) });
    }

    fn conflicts_told(domain: &crate::domain_runtime::DomainRuntime) -> Vec<(String, String)> {
        domain.state().unwrap().interactions.values().flat_map(|interaction| match &interaction.payload {
            InteractionPayload::Delivery { feedback, .. } => feedback.iter()
                .filter(|f| f.instruction == Instruction::Conflicts)
                .map(|f| (interaction.id.0.clone(), f.body.clone())).collect(),
            _ => vec![],
        }).collect()
    }

    #[test]
    fn a_conflicting_presented_pr_goes_back_to_its_node_once_per_head() {
        let scratch_directory = crate::test_support::Directory::new();
        let home = Home(scratch_directory.parent().unwrap().join(format!("alondra-conflicts-{}", crate::auth::random_hex(8))));
        std::fs::create_dir(&home.0).unwrap();
        let domain = crate::domain_runtime::DomainRuntime::open(&home.0).unwrap();
        let user = Actor::User { user_id: "user".into() };
        run(&domain, Actor::System, "agent", Command::RegisterAgent { agent_id: "agent".into(), name: "Agent".into(), at: 0 });
        run(&domain, user.clone(), "request", Command::CreateRequest { request_id: "request".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
        run(&domain, Actor::Orchestrator { generation: 0 }, "task", Command::CreateTask { plan_items: None, task_id: "task".into(), request_id: "request".into(), title: "Task".into(),
            agent_id: Some("agent".into()), review_prs: false, dependencies: Default::default(), stack: None,
            launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")), });
        run(&domain, Actor::Agent { agent_id: "agent".into() }, "pr", Command::RegisterPr { task_id: "task".into(),
            pr_id: "pr".into(), host: "github.com".into(), repository: "me/app".into(), number: 7, head: "head1".into(),
            base: "main".into(), allowed_methods: vec![MergeMethod::Squash] });
        present(&domain, "first", "head1");
        let details = PrDetails::default();
        details.record("pr", Details { conflicts: false, ..Default::default() });
        hand_back_conflicts(&domain, &details, "Ana");
        assert!(conflicts_told(&domain).is_empty(), "a PR that merges cleanly stays with the user");

        details.record("pr", Details { conflicts: true, head_sha: Some("head1".into()), base_ref: Some("main".into()), ..Default::default() });
        hand_back_conflicts(&domain, &details, "Ana");
        hand_back_conflicts(&domain, &details, "Ana");
        let told = conflicts_told(&domain);
        assert_eq!(told.len(), 1, "once per head: {told:?}");
        assert!(told[0].1.starts_with("Ana asks you to resolve the conflicts of https://github.com/me/app/pull/7 with main"), "{}", told[0].1);
        assert!(domain.read().index().waiting().is_empty(), "no card for the user");

        present(&domain, "again", "head1");
        hand_back_conflicts(&domain, &details, "Ana");
        assert_eq!(conflicts_told(&domain).len(), 1, "presenting the same head again is not a new head");

        run(&domain, Actor::System, "observed", Command::ObservePrState { pr_id: "pr".into(), head: "head2".into(),
            base: "main".into(), state: PrState::Open, merged_head: None });
        present(&domain, "pushed", "head2");
        details.record("pr", Details { conflicts: true, head_sha: Some("head2".into()), base_ref: Some("main".into()), ..Default::default() });
        hand_back_conflicts(&domain, &details, "Ana");
        assert_eq!(conflicts_told(&domain).iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(), ["first", "pushed"]);
    }

    #[test]
    fn a_merge_failure_is_not_a_conflict_already_sent() {
        let scratch_directory = crate::test_support::Directory::new();
        let home = Home(scratch_directory.parent().unwrap().join(format!("alondra-conflicts-merge-{}", crate::auth::random_hex(8))));
        std::fs::create_dir(&home.0).unwrap();
        let domain = crate::domain_runtime::DomainRuntime::open(&home.0).unwrap();
        let user = Actor::User { user_id: "user".into() };
        run(&domain, Actor::System, "agent", Command::RegisterAgent { agent_id: "agent".into(), name: "Agent".into(), at: 0 });
        run(&domain, user.clone(), "request", Command::CreateRequest { request_id: "request".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
        run(&domain, Actor::Orchestrator { generation: 0 }, "task", Command::CreateTask { plan_items: None, task_id: "task".into(), request_id: "request".into(), title: "Task".into(),
            agent_id: Some("agent".into()), review_prs: false, dependencies: Default::default(), stack: None,
            launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")), });
        run(&domain, Actor::Agent { agent_id: "agent".into() }, "pr", Command::RegisterPr { task_id: "task".into(),
            pr_id: "pr".into(), host: "github.com".into(), repository: "me/app".into(), number: 7, head: "head1".into(),
            base: "main".into(), allowed_methods: vec![MergeMethod::Squash] });
        present(&domain, "first", "head1");
        run(&domain, Actor::System, "merge-failed", Command::SubmitFeedback { attachments: vec![], presentation_id: "first".into(),
            targets: vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: "head1".into() }],
            body: alondra_core::domain::merge_failed_text(7, "https://github.com/me/app/pull/7", "main", "review required"),
            instruction: Instruction::Conflicts });
        present(&domain, "again", "head1");
        let details = PrDetails::default();
        details.record("pr", Details { conflicts: true, head_sha: Some("head1".into()), base_ref: Some("main".into()), ..Default::default() });
        hand_back_conflicts(&domain, &details, "Ana");
        assert_eq!(conflicts_told(&domain).iter().filter(|(id, _)| id == "again").count(), 1,
            "a real conflict at the same head still goes back");
    }

    #[derive(Clone, Default)]
    struct ReadMetrics {
        started: Vec<u64>,
        active: usize,
        peak: usize,
        active_body_bytes: usize,
        peak_body_bytes: usize,
        completed: usize,
        errors: usize,
        returned_body_bytes: usize,
    }

    struct ControlledView {
        mergeable: &'static str,
        hold_all: bool,
        released: std::sync::atomic::AtomicBool,
        release: tokio::sync::Notify,
        metrics: std::sync::Mutex<ReadMetrics>,
    }

    impl ControlledView {
        fn new(hold_all: bool) -> Self {
            Self { mergeable: "MERGEABLE", hold_all, released: false.into(), release: Default::default(), metrics: Default::default() }
        }
        fn release(&self) {
            self.released.store(true, std::sync::atomic::Ordering::SeqCst);
            self.release.notify_waiters();
        }
        fn metrics(&self) -> ReadMetrics {
            self.metrics.lock().unwrap().clone()
        }
    }

    struct ActiveRead<'a> {
        source: &'a ControlledView,
        bytes: usize,
    }
    impl Drop for ActiveRead<'_> {
        fn drop(&mut self) {
            let mut metrics = self.source.metrics.lock().unwrap();
            metrics.active -= 1;
            metrics.active_body_bytes -= self.bytes;
        }
    }

    impl super::PrView for ControlledView {
        fn view<'a>(&'a self, url: &'a str) -> super::Viewed<'a> {
            Box::pin(async move {
                let number: u64 = url.rsplit('/').next().unwrap().parse().unwrap();
                let value = (number != 3).then(|| json!({
                    "title":format!("Provider PR {number}"),"body":"x".repeat(4096),
                    "headRefOid":"a".repeat(40),"headRefName":"branch","baseRefName":"main",
                    "state":"OPEN","mergeable":self.mergeable,"statusCheckRollup":[],
                }));
                let bytes = value.as_ref().map_or(0, |value| value["body"].as_str().unwrap().len());
                {
                    let mut metrics = self.metrics.lock().unwrap();
                    metrics.started.push(number);
                    metrics.active += 1;
                    metrics.peak = metrics.peak.max(metrics.active);
                    metrics.active_body_bytes += bytes;
                    metrics.peak_body_bytes = metrics.peak_body_bytes.max(metrics.active_body_bytes);
                }
                let _active = ActiveRead { source: self, bytes };
                if (self.hold_all || number == 1) && !self.released.load(std::sync::atomic::Ordering::SeqCst) {
                    self.release.notified().await;
                }
                {
                    let mut metrics = self.metrics.lock().unwrap();
                    metrics.completed += 1;
                    metrics.errors += usize::from(value.is_none());
                    metrics.returned_body_bytes += bytes;
                }
                value
            })
        }
    }

    fn watched_prs(count: u64) -> alondra_core::domain::DomainState {
        let mut state = alondra_core::domain::DomainState::new("provider-fixture");
        let mut apply = |actor, command| {
            state.execute(&actor, &CommandEnvelope {
                protocol_version: 2, data_epoch: state.data_epoch.clone(),
                command_id: format!("fixture-{}", state.revision).into(),
                expected_revision: state.revision, command,
            }).unwrap();
        };
        apply(Actor::User { user_id: "user".into() }, Command::CreateRequest {
            request_id: "request".into(), body: "Review the providers".into(),
            auto_merge: false, maintenance: false, message: false,
        });
        apply(Actor::User { user_id: "user".into() }, Command::TitleRequest {
            request_id: "request".into(), title: "Provider review".into(),
        });
        for number in 1..=count {
            let agent = format!("agent-{number}");
            let task = format!("task-{number}");
            apply(Actor::System, Command::RegisterAgent { agent_id: agent.clone().into(), name: "Fixture".into(), at: 0 });
            apply(Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None,
                task_id: task.clone().into(), request_id: "request".into(), title: "Review".into(),
                agent_id: Some(agent.clone().into()), review_prs: true, dependencies: Default::default(), stack: None,
                launch: Some(alondra_core::domain::LaunchPlan::new("node", "/synthetic")),
            });
            apply(Actor::Agent { agent_id: agent.into() }, Command::RegisterPr {
                task_id: task.into(), pr_id: format!("pr-{number}").into(), host: "github.com".into(),
                repository: format!("fixture/repo-{number}"), number, head: "a".repeat(40),
                base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
            });
        }
        state.validate().unwrap();
        state
    }

    fn poll_pending(future: std::pin::Pin<&mut impl std::future::Future>) {
        let waker = std::task::Waker::noop();
        assert!(future.poll(&mut std::task::Context::from_waker(waker)).is_pending());
    }

    fn foreground_fixture() -> (crate::test_support::Directory, std::sync::Arc<crate::client_routes::Client>, alondra_core::domain::PullRequest) {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = std::sync::Arc::new(crate::domain_runtime::DomainRuntime::open(&home).unwrap());
        run(&domain, Actor::System, "agent", Command::RegisterAgent { agent_id: "agent".into(), name: "Fixture".into(), at: 0 });
        run(&domain, Actor::User { user_id: "user".into() }, "request", Command::CreateRequest {
            request_id: "request".into(), body: "Review".into(), auto_merge: false, maintenance: false, message: false,
        });
        run(&domain, Actor::Orchestrator { generation: 0 }, "task", Command::CreateTask { plan_items: None,
            task_id: "task".into(), request_id: "request".into(), title: "Review".into(), agent_id: Some("agent".into()),
            review_prs: true, dependencies: Default::default(), stack: None,
            launch: Some(alondra_core::domain::LaunchPlan::new("node", "/synthetic")),
        });
        run(&domain, Actor::Agent { agent_id: "agent".into() }, "pr", Command::RegisterPr {
            task_id: "task".into(), pr_id: "pr-1".into(), host: "github.com".into(), repository: "fixture/repo".into(),
            number: 1, head: "a".repeat(40), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
        });
        let pr = domain.state().unwrap().pull_requests[&"pr-1".into()].clone();
        let client = crate::client_routes::Client::open(domain, &home, Default::default());
        client.details.record(&pr.id.0, Details {
            title: Some("Prior unknown observation".into()), head_sha: Some(pr.head.clone()),
            base_ref: Some(pr.base.clone()), mergeability_unknown: true, ..Default::default()
        });
        (home, client, pr)
    }

    fn foreground_presentation(pr: &alondra_core::domain::PullRequest) -> Command {
        let targets = vec![DeliveryTarget::PullRequest { pr_id: pr.id.clone(), head: pr.head.clone() }];
        Command::PresentResult {
            checklist: Vec::new(),
            attachments: vec![], task_id: pr.task_id.clone(), interaction_id: "candidate".into(), body: "Ready".into(),
            is_final: true, nothing_to_review: false,
            card: alondra_core::domain::CardSpec::delivery("Review", &targets, true), targets,
        }
    }

    #[tokio::test]
    async fn a_presented_pr_whose_description_cites_an_image_missing_from_the_branch_is_refused_naming_it() {
        use crate::pr_images::tests::{FakeGitHub, PNG};
        let (_home, client, pr) = foreground_fixture();
        let view = std::sync::Arc::new(super::FakePrView::default());
        let answer = |body: &str| json!({
            "title": "Duo", "body": body, "headRefOid": pr.head, "headRefName": "duo", "baseRefName": "main",
            "state": "OPEN", "mergeable": "MERGEABLE", "mergeStateStatus": "CLEAN", "statusCheckRollup": [],
        });
        view.answer(Some(answer("![areas](docs/areas.png)\n\n![gone](docs/gone.png)\n\n![](https://github.com/secret/repo/blob/main/a.png)")));
        client.use_pr_view(view.clone());
        client.use_pr_images(std::sync::Arc::new(FakeGitHub::with(&[("fixture/repo", &pr.head, "docs/areas.png", PNG)])));
        let error = client.verify_presentation(&foreground_presentation(&pr)).await.unwrap_err();
        assert_eq!(error.code, "pr_images_missing");
        assert!(error.message.contains("docs/gone.png (not in the branch (aaaaaaa))"), "{}", error.message);
        assert!(!error.message.contains("areas.png") && !error.message.contains("secret/repo"), "{}", error.message);
        let shown = client.details.get(&pr.id.0).unwrap().images;
        assert!(matches!(shown[0].shown, alondra_core::views::pr_images::Shown::Stored { .. }), "{shown:?}");

        view.answer(Some(answer("![areas](docs/areas.png)\n\n![](https://github.com/secret/repo/blob/main/a.png)")));
        client.verify_presentation(&foreground_presentation(&pr)).await.unwrap();
        let shown = client.details.get(&pr.id.0).unwrap();
        assert!(!shown.body.as_deref().unwrap().contains("gone"), "the description read to present it shows at once: {:?}", shown.body);
        assert!(matches!(shown.images[0].shown, alondra_core::views::pr_images::Shown::Stored { .. }), "{:?}", shown.images);
    }

    #[tokio::test]
    async fn provider_later_failed_background_read_preserves_a_valid_foreground_answer() {
        for (mergeable, expected) in [("MERGEABLE", super::Merge::Clean), ("CONFLICTING", super::Merge::Conflicts)] {
            let (_home, client, pr) = foreground_fixture();
            let state = client.domain.state().unwrap();
            let source = std::sync::Arc::new(ControlledView { mergeable, ..ControlledView::new(false) });
            client.use_pr_view(source.clone());
            let previous = client.details.get(&pr.id.0).unwrap();
            let mut foreground = Box::pin(client.merge_state(&pr, &pr.head, 1, std::time::Duration::ZERO));
            poll_pending(foreground.as_mut());
            client.details.refresh(&state, &super::FakePrView::default()).await;
            assert_eq!(client.details.get(&pr.id.0).unwrap(), previous, "a failed later read preserves the old data and timestamp");
            source.release();
            assert_eq!(foreground.await, expected, "a failed later read is not authority to hide a valid foreground answer");
            let accepted = client.details.get(&pr.id.0).unwrap();
            assert_eq!(accepted.merge(), expected);
            assert_eq!(accepted.title.as_deref(), Some("Provider PR 1"));
            assert!(accepted.observed_at.is_some() && accepted.matches_revision(&pr.head, &pr.base));
            assert_eq!(client.domain.state().unwrap(), state);
        }
    }

    #[tokio::test]
    async fn provider_later_failed_background_read_cannot_hide_foreground_presentation_conflicts() {
        let (_home, client, pr) = foreground_fixture();
        let state = client.domain.state().unwrap();
        let source = std::sync::Arc::new(ControlledView { mergeable: "CONFLICTING", ..ControlledView::new(false) });
        client.use_pr_view(source.clone());
        let command = foreground_presentation(&pr);
        let mut foreground = Box::pin(client.verify_presentation(&command));
        poll_pending(foreground.as_mut());
        client.details.refresh(&state, &super::FakePrView::default()).await;
        source.release();
        assert_eq!(foreground.await.unwrap_err().code, "pr_conflicts", "known conflicts must still refuse the presentation");
        let accepted = client.details.get(&pr.id.0).unwrap();
        assert!(accepted.conflicts && accepted.observed_at.is_some());
        assert_eq!(client.domain.state().unwrap(), state, "no presentation or effect is committed by readonly verification");
    }

    #[tokio::test]
    async fn provider_later_successful_background_answer_keeps_matching_authority() {
        for (foreground_merge, background_merge, head, base, expected) in [
            ("MERGEABLE", "CONFLICTING", "a".repeat(40), "main", super::Merge::Conflicts),
            ("CONFLICTING", "MERGEABLE", "a".repeat(40), "main", super::Merge::Clean),
            ("MERGEABLE", "CONFLICTING", "b".repeat(40), "main", super::Merge::Unknown),
            ("MERGEABLE", "CONFLICTING", "a".repeat(40), "release", super::Merge::Unknown),
        ] {
            let (_home, client, pr) = foreground_fixture();
            let state = client.domain.state().unwrap();
            let source = std::sync::Arc::new(ControlledView { mergeable: foreground_merge, ..ControlledView::new(false) });
            client.use_pr_view(source.clone());
            let mut foreground = Box::pin(client.merge_state(&pr, &pr.head, 1, std::time::Duration::ZERO));
            poll_pending(foreground.as_mut());
            let background = super::FakePrView::default();
            background.answer(Some(json!({"state":"OPEN", "title":"New background observation",
                "headRefOid":head, "baseRefName":base, "mergeable":background_merge})));
            client.details.refresh(&state, &background).await;
            let newest = client.details.get(&pr.id.0).unwrap();
            assert!(newest.observed_at.is_some());
            source.release();
            assert_eq!(foreground.await, expected, "a losing foreground read uses only matching accepted authority");
            assert_eq!(client.details.get(&pr.id.0).unwrap(), newest, "the newer successful answer and timestamp survive the older completion");
            assert_eq!(client.domain.state().unwrap(), state);
        }
    }

    #[tokio::test]
    async fn provider_later_successful_background_conflicts_still_refuse_a_presentation() {
        let (_home, client, pr) = foreground_fixture();
        let state = client.domain.state().unwrap();
        let source = std::sync::Arc::new(ControlledView::new(false));
        client.use_pr_view(source.clone());
        let command = foreground_presentation(&pr);
        let mut foreground = Box::pin(client.verify_presentation(&command));
        poll_pending(foreground.as_mut());
        let background = super::FakePrView::default();
        background.answer(Some(json!({"state":"OPEN", "headRefOid":pr.head, "baseRefName":pr.base, "mergeable":"CONFLICTING"})));
        client.details.refresh(&state, &background).await;
        let newest = client.details.get(&pr.id.0).unwrap();
        source.release();
        assert_eq!(foreground.await.unwrap_err().code, "pr_conflicts", "newer accepted conflicts cannot become Unknown");
        assert_eq!(client.details.get(&pr.id.0).unwrap(), newest);
        assert_eq!(client.domain.state().unwrap(), state);
    }

    struct CacheView {
        answer: Option<serde_json::Value>,
        reads: std::sync::atomic::AtomicUsize,
        errors: std::sync::atomic::AtomicUsize,
        bytes: std::sync::atomic::AtomicUsize,
    }
    impl CacheView {
        fn new(mergeable: Option<&str>) -> Self {
            Self {
                answer: mergeable.map(|mergeable| json!({"state":"OPEN", "headRefOid":"a".repeat(40),
                    "baseRefName":"main", "mergeable":mergeable, "title":"Current provider", "body":"x".repeat(4096)})),
                reads: 0.into(), errors: 0.into(), bytes: 0.into(),
            }
        }
        fn metrics(&self) -> (usize, usize, usize) {
            use std::sync::atomic::Ordering::SeqCst;
            (self.reads.load(SeqCst), self.errors.load(SeqCst), self.bytes.load(SeqCst))
        }
    }
    impl super::PrView for CacheView {
        fn view<'a>(&'a self, _: &'a str) -> super::Viewed<'a> {
            Box::pin(async move {
                use std::sync::atomic::Ordering::SeqCst;
                self.reads.fetch_add(1, SeqCst);
                self.errors.fetch_add(usize::from(self.answer.is_none()), SeqCst);
                self.bytes.fetch_add(self.answer.as_ref().and_then(|v| v["body"].as_str()).map_or(0, str::len), SeqCst);
                self.answer.clone()
            })
        }
    }

    fn cache_clock(details: &PrDetails) -> std::sync::Arc<std::sync::atomic::AtomicI64> {
        let clock = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(1000));
        let read = clock.clone();
        *details.clock.lock().unwrap() = Some(std::sync::Arc::new(move || read.load(std::sync::atomic::Ordering::SeqCst)));
        clock
    }
    fn cached_clean(client: &crate::client_routes::Client, pr: &alondra_core::domain::PullRequest) {
        client.details.record(&pr.id.0, Details { title: Some("Cached clean".into()), body: Some("Preserved body".into()),
            head_sha: Some(pr.head.clone()), base_ref: Some(pr.base.clone()), ..Default::default() });
    }

    #[tokio::test]
    async fn freshness_expired_clean_revalidates_and_failure_retains_data_without_authority() {
        let (_home, client, pr) = foreground_fixture();
        let state = client.domain.state().unwrap();
        let clock = cache_clock(&client.details);
        let source = std::sync::Arc::new(CacheView::new(None));
        client.use_pr_view(source.clone());
        cached_clean(&client, &pr);
        let previous = client.details.get(&pr.id.0).unwrap();
        clock.store(1000 + 59_999, std::sync::atomic::Ordering::SeqCst);
        let fresh = client.merge_state(&pr, &pr.head, 1, std::time::Duration::ZERO).await;
        let fresh_metrics = source.metrics();
        clock.store(1000 + 60_000, std::sync::atomic::Ordering::SeqCst);
        let stale = client.merge_state(&pr, &pr.head, 1, std::time::Duration::ZERO).await;
        println!("clean_cache queries=2 fresh={fresh:?} fresh_provider={fresh_metrics:?} stale={stale:?} total_provider={:?}", source.metrics());
        assert_eq!(fresh, super::Merge::Clean);
        assert_eq!(fresh_metrics, (0, 0, 0), "recent matching clean remains a useful fast path");
        assert_eq!(stale, super::Merge::Unknown, "expired clean cannot certify the current base after provider failure");
        assert_eq!(source.metrics(), (1, 1, 0));
        assert_eq!(client.details.get(&pr.id.0).unwrap(), previous, "failure preserves body and provenance without renewing it");
        assert_eq!(previous.observed_at, Some(1000));
        assert_eq!(client.domain.state().unwrap(), state);
    }

    #[tokio::test]
    async fn freshness_expired_clean_fetches_current_conflicts_before_presentation() {
        let (_home, client, pr) = foreground_fixture();
        let state = client.domain.state().unwrap();
        let clock = cache_clock(&client.details);
        let source = std::sync::Arc::new(CacheView::new(Some("CONFLICTING")));
        client.use_pr_view(source.clone());
        cached_clean(&client, &pr);
        clock.store(61_000, std::sync::atomic::Ordering::SeqCst);
        let result = client.verify_presentation(&foreground_presentation(&pr)).await;
        println!("clean_cache expired_conflicts provider={:?} rejected={}", source.metrics(), result.is_err());
        assert_eq!(result.unwrap_err().code, "pr_conflicts");
        assert_eq!(source.metrics(), (1, 0, 4096));
        let accepted = client.details.get(&pr.id.0).unwrap();
        assert!(accepted.conflicts && accepted.matches_revision(&pr.head, &pr.base));
        assert_eq!(accepted.observed_at, Some(61_000));
        assert_eq!(client.domain.state().unwrap(), state);
    }

    #[tokio::test]
    async fn freshness_successful_revalidation_renews_clean_and_next_read_is_cached() {
        let (_home, client, pr) = foreground_fixture();
        let clock = cache_clock(&client.details);
        let source = std::sync::Arc::new(CacheView::new(Some("MERGEABLE")));
        client.use_pr_view(source.clone());
        cached_clean(&client, &pr);
        clock.store(61_000, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(client.merge_state(&pr, &pr.head, 1, std::time::Duration::ZERO).await, super::Merge::Clean);
        println!("clean_cache successful_revalidation provider={:?}", source.metrics());
        assert_eq!(source.metrics(), (1, 0, 4096));
        assert_eq!(client.details.get(&pr.id.0).unwrap().observed_at, Some(61_000));
        clock.store(61_001, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(client.merge_state(&pr, &pr.head, 1, std::time::Duration::ZERO).await, super::Merge::Clean);
        assert_eq!(source.metrics(), (1, 0, 4096), "a fresh renewed answer avoids another provider read");
    }

    #[tokio::test]
    async fn freshness_missing_future_or_overflowing_provenance_requires_revalidation() {
        for observed_at in [None, Some(1001), Some(i64::MIN)] {
            let (_home, client, pr) = foreground_fixture();
            cache_clock(&client.details);
            let source = std::sync::Arc::new(CacheView::new(None));
            client.use_pr_view(source.clone());
            cached_clean(&client, &pr);
            client.details.cache.lock().unwrap().entries.get_mut(&pr.id.0).unwrap().details.as_mut().unwrap().observed_at = observed_at;
            let previous = client.details.get(&pr.id.0).unwrap();
            assert_eq!(client.merge_state(&pr, &pr.head, 1, std::time::Duration::ZERO).await, super::Merge::Unknown);
            assert_eq!(source.metrics(), (1, 1, 0));
            assert_eq!(client.details.get(&pr.id.0).unwrap(), previous);
        }
    }

    #[tokio::test]
    async fn freshness_losing_foreground_cannot_certify_an_expired_background_clean() {
        let (_home, client, pr) = foreground_fixture();
        let state = client.domain.state().unwrap();
        let clock = cache_clock(&client.details);
        let source = std::sync::Arc::new(ControlledView::new(false));
        client.use_pr_view(source.clone());
        let mut foreground = Box::pin(client.merge_state(&pr, &pr.head, 1, std::time::Duration::ZERO));
        poll_pending(foreground.as_mut());
        client.details.refresh(&state, &CacheView::new(Some("MERGEABLE"))).await;
        let previous = client.details.get(&pr.id.0).unwrap();
        assert_eq!(previous.observed_at, Some(1000));
        clock.store(61_000, std::sync::atomic::Ordering::SeqCst);
        source.release();
        assert_eq!(foreground.await, super::Merge::Unknown, "losing-completion fallback cannot bypass clean expiry");
        assert_eq!(client.details.get(&pr.id.0).unwrap(), previous);
        assert_eq!(client.domain.state().unwrap(), state);
    }

    #[tokio::test]
    async fn freshness_boundary_expired_clean_cannot_hide_a_returned_foreground_conflict() {
        let (_home, client, pr) = foreground_fixture();
        let state = client.domain.state().unwrap();
        let clock = cache_clock(&client.details);
        let source = std::sync::Arc::new(ControlledView { mergeable: "CONFLICTING", ..ControlledView::new(false) });
        client.use_pr_view(source.clone());
        let command = foreground_presentation(&pr);
        let mut foreground = Box::pin(client.verify_presentation(&command));
        poll_pending(foreground.as_mut());
        client.details.refresh(&state, &CacheView::new(Some("MERGEABLE"))).await;
        let newest = client.details.get(&pr.id.0).unwrap();
        clock.store(61_000, std::sync::atomic::Ordering::SeqCst);
        source.release();
        assert_eq!(foreground.await.unwrap_err().code, "pr_conflicts", "expired clean is not contradictory authority for matching live conflicts");
        assert_eq!(client.details.get(&pr.id.0).unwrap(), newest, "rejection need not replace or renew the newer cache row");
        assert_eq!(newest.observed_at, Some(1000));
        assert_eq!(client.domain.state().unwrap(), state);
    }

    #[tokio::test]
    async fn freshness_boundary_fresh_unknown_cannot_hide_a_returned_foreground_conflict() {
        let (_home, client, pr) = foreground_fixture();
        let state = client.domain.state().unwrap();
        let clock = cache_clock(&client.details);
        let source = std::sync::Arc::new(ControlledView { mergeable: "CONFLICTING", ..ControlledView::new(false) });
        client.use_pr_view(source.clone());
        let command = foreground_presentation(&pr);
        let mut foreground = Box::pin(client.verify_presentation(&command));
        poll_pending(foreground.as_mut());
        clock.store(61_000, std::sync::atomic::Ordering::SeqCst);
        client.details.refresh(&state, &CacheView::new(Some("UNKNOWN"))).await;
        let newest = client.details.get(&pr.id.0).unwrap();
        source.release();
        assert_eq!(foreground.await.unwrap_err().code, "pr_conflicts", "Unknown does not contradict matching live conflicts");
        assert_eq!(client.details.get(&pr.id.0).unwrap(), newest);
        assert_eq!(newest.observed_at, Some(61_000));
        assert_eq!(client.domain.state().unwrap(), state);
    }

    #[tokio::test]
    async fn freshness_boundary_newer_fresh_clean_still_supersedes_foreground_conflicts() {
        let (_home, client, pr) = foreground_fixture();
        let state = client.domain.state().unwrap();
        let clock = cache_clock(&client.details);
        let source = std::sync::Arc::new(ControlledView { mergeable: "CONFLICTING", ..ControlledView::new(false) });
        client.use_pr_view(source.clone());
        let command = foreground_presentation(&pr);
        let mut foreground = Box::pin(client.verify_presentation(&command));
        poll_pending(foreground.as_mut());
        clock.store(61_000, std::sync::atomic::Ordering::SeqCst);
        client.details.refresh(&state, &CacheView::new(Some("MERGEABLE"))).await;
        let newest = client.details.get(&pr.id.0).unwrap();
        source.release();
        assert!(foreground.await.is_ok(), "fresh matching later clean remains authoritative");
        assert_eq!(client.details.get(&pr.id.0).unwrap(), newest);
        assert_eq!(newest.observed_at, Some(61_000));
        assert_eq!(client.domain.state().unwrap(), state);
    }

    #[tokio::test]
    async fn freshness_boundary_wrong_head_or_base_still_returns_unknown() {
        for (field, value) in [("headRefOid", "b".repeat(40)), ("baseRefName", "release".into())] {
            let (_home, client, pr) = foreground_fixture();
            let state = client.domain.state().unwrap();
            cache_clock(&client.details);
            let source = std::sync::Arc::new(ControlledView { mergeable: "CONFLICTING", ..ControlledView::new(false) });
            client.use_pr_view(source.clone());
            let mut foreground = Box::pin(client.merge_state(&pr, &pr.head, 1, std::time::Duration::ZERO));
            poll_pending(foreground.as_mut());
            let mut background = CacheView::new(Some("MERGEABLE"));
            background.answer.as_mut().unwrap()[field] = value.into();
            client.details.refresh(&state, &background).await;
            let newest = client.details.get(&pr.id.0).unwrap();
            source.release();
            assert_eq!(foreground.await, super::Merge::Unknown);
            assert_eq!(client.details.get(&pr.id.0).unwrap(), newest);
            assert_eq!(client.domain.state().unwrap(), state);
        }
    }

    #[tokio::test]
    async fn provider_refresh_publishes_independent_prs_before_a_blocked_first_read() {
        let state = watched_prs(4);
        let unchanged = state.clone();
        let details = PrDetails::default();
        details.record("pr-3", Details { title: Some("Last successful observation".into()), ..Default::default() });
        let previous_failed = details.get("pr-3").unwrap();
        let source = ControlledView::new(false);
        let mut refresh = Box::pin(details.refresh(&state, &source));
        poll_pending(refresh.as_mut());
        let partial = details.get("pr-2").is_some() && details.get("pr-4").is_some();
        let pending = source.metrics();
        source.release();
        refresh.await;
        let completed = source.metrics();
        println!("provider_reads offered=4 completed={} errors={} peak_active={} peak_body_bytes={} returned_body_bytes={} partial_before_release={partial} pending_at_checkpoint={}",
            completed.completed, completed.errors, completed.peak, completed.peak_body_bytes,
            completed.returned_body_bytes, pending.active);
        assert_eq!(completed.started, [1, 2, 3, 4]);
        assert_eq!((completed.completed, completed.errors, completed.active), (4, 1, 0));
        assert_eq!(completed.returned_body_bytes, 3 * 4096);
        assert_eq!(details.get("pr-3").unwrap(), previous_failed, "a failed read never renews its last observation");
        assert!(details.get("pr-2").unwrap().observed_at.is_some());
        assert!(serde_json::to_value(details.get("pr-2").unwrap()).unwrap().get("observed_at").is_none(),
            "observation provenance remains private rather than an unused product field");
        assert_eq!(state, unchanged, "detail preparation never writes the domain");
        assert!(partial, "independent details must be published before the first provider read is released");
    }

    #[tokio::test]
    async fn provider_old_background_answer_cannot_replace_a_newer_foreground_observation() {
        let state = watched_prs(1);
        let details = PrDetails::default();
        let source = ControlledView::new(false);
        let mut refresh = Box::pin(details.refresh(&state, &source));
        poll_pending(refresh.as_mut());
        details.record("pr-1", Details {
            title: Some("New foreground observation".into()), head_sha: Some("b".repeat(40)),
            base_ref: Some("release".into()), ..Default::default()
        });
        let newest = details.get("pr-1").unwrap();
        source.release();
        refresh.await;
        assert_eq!(details.get("pr-1").unwrap(), newest, "an older provider answer cannot overwrite the newer head or base");
    }

    #[tokio::test]
    async fn provider_merge_state_requires_the_requested_head_and_base() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = std::sync::Arc::new(crate::domain_runtime::DomainRuntime::open(&home).unwrap());
        let before = domain.state().unwrap();
        let client = crate::client_routes::Client::open(domain.clone(), &home, Default::default());
        let source = std::sync::Arc::new(super::FakePrView::default());
        client.use_pr_view(source.clone());
        let state = watched_prs(1);
        let pr = state.pull_requests.values().next().unwrap();
        for mergeable in ["MERGEABLE", "CONFLICTING"] {
            for (head, base) in [("b".repeat(40), "main"), (pr.head.clone(), "release")] {
                client.details.record(&pr.id.0, Details::default());
                source.answer(Some(json!({"state":"OPEN","headRefOid":head,"baseRefName":base,"mergeable":mergeable})));
                assert_eq!(client.merge_state(pr, &pr.head, 1, std::time::Duration::ZERO).await, super::Merge::Unknown,
                    "a different head or base is not an authoritative answer about the requested PR revision");
            }
        }
        for (mergeable, expected) in [("MERGEABLE", super::Merge::Clean), ("CONFLICTING", super::Merge::Conflicts)] {
            client.details.record(&pr.id.0, Details::default());
            source.answer(Some(json!({"state":"OPEN","headRefOid":pr.head,"baseRefName":"main","mergeable":mergeable})));
            assert_eq!(client.merge_state(pr, &pr.head, 1, std::time::Duration::ZERO).await, expected);
        }
        assert_eq!(domain.state().unwrap(), before, "provider preparation never writes or authorizes effects");
    }

    #[tokio::test]
    async fn provider_cached_clean_answer_for_another_base_is_not_authority() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = std::sync::Arc::new(crate::domain_runtime::DomainRuntime::open(&home).unwrap());
        let client = crate::client_routes::Client::open(domain, &home, Default::default());
        let source = std::sync::Arc::new(super::FakePrView::default());
        client.use_pr_view(source);
        let state = watched_prs(1);
        let pr = state.pull_requests.values().next().unwrap();
        client.details.record(&pr.id.0, Details { head_sha: Some(pr.head.clone()), base_ref: Some("release".into()), ..Default::default() });
        assert_eq!(client.merge_state(pr, &pr.head, 1, std::time::Duration::ZERO).await, super::Merge::Unknown,
            "a cached clean answer about another base cannot bypass a failed current read");
    }

    #[test]
    fn provider_stale_conflicts_do_not_hand_back_a_new_head_or_base() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = crate::domain_runtime::DomainRuntime::open(&home).unwrap();
        run(&domain, Actor::System, "agent", Command::RegisterAgent { agent_id: "agent".into(), name: "Fixture".into(), at: 0 });
        run(&domain, Actor::User { user_id: "user".into() }, "request", Command::CreateRequest {
            request_id: "request".into(), body: "Review".into(), auto_merge: false, maintenance: false, message: false,
        });
        run(&domain, Actor::Orchestrator { generation: 0 }, "task", Command::CreateTask { plan_items: None,
            task_id: "task".into(), request_id: "request".into(), title: "Review".into(), agent_id: Some("agent".into()),
            review_prs: true, dependencies: Default::default(), stack: None,
            launch: Some(alondra_core::domain::LaunchPlan::new("node", "/synthetic")),
        });
        run(&domain, Actor::Agent { agent_id: "agent".into() }, "pr", Command::RegisterPr {
            task_id: "task".into(), pr_id: "pr".into(), host: "github.com".into(), repository: "fixture/repo".into(),
            number: 1, head: "new-head".into(), base: "release".into(), allowed_methods: vec![MergeMethod::Squash],
        });
        present(&domain, "presentation", "new-head");
        let details = PrDetails::default();
        for (head, base) in [("old-head", "release"), ("new-head", "main")] {
            details.record("pr", Details { conflicts: true, head_sha: Some(head.into()), base_ref: Some(base.into()), ..Default::default() });
            hand_back_conflicts(&domain, &details, "User");
            assert!(conflicts_told(&domain).is_empty(), "conflicts from a different head or base are not authority for feedback");
        }
        details.record("pr", Details { conflicts: true, head_sha: Some("new-head".into()), base_ref: Some("release".into()), ..Default::default() });
        hand_back_conflicts(&domain, &details, "User");
        assert_eq!(conflicts_told(&domain).len(), 1, "matching provider facts still hand conflicts back once");
    }

    #[tokio::test]
    async fn provider_partial_details_are_visible_in_a_real_view_before_first_release() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = std::sync::Arc::new(crate::domain_runtime::DomainRuntime::open(&home).unwrap());
        for number in 1..=2 {
            let agent = format!("agent-{number}");
            let request = format!("request-{number}");
            let task = format!("task-{number}");
            run(&domain, Actor::System, &agent, Command::RegisterAgent { agent_id: agent.clone().into(), name: "Fixture".into(), at: 0 });
            run(&domain, Actor::User { user_id: "user".into() }, &request, Command::CreateRequest {
                request_id: request.clone().into(), body: "Review".into(), auto_merge: false, maintenance: false, message: false,
            });
            run(&domain, Actor::Orchestrator { generation: 0 }, &task, Command::CreateTask { plan_items: None,
                task_id: task.clone().into(), request_id: request.into(), title: "Review".into(),
                agent_id: Some(agent.clone().into()), review_prs: true, dependencies: Default::default(), stack: None,
                launch: Some(alondra_core::domain::LaunchPlan::new("node", "/synthetic")),
            });
            run(&domain, Actor::Agent { agent_id: agent.into() }, &format!("pr-{number}"), Command::RegisterPr {
                task_id: task.into(), pr_id: format!("pr-{number}").into(), host: "github.com".into(),
                repository: format!("fixture/repo-{number}"), number, head: "a".repeat(40),
                base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
            });
        }
        let state = domain.state().unwrap();
        let client = crate::client_routes::Client::open(domain.clone(), &home, Default::default());
        async fn get(client: &crate::client_routes::Client, path: &str) -> serde_json::Value {
            let response = crate::views::routes::relay(client, "fixture-device", path).await;
            assert_eq!(response.status(), axum::http::StatusCode::OK);
            let bytes = axum::body::to_bytes(response.into_body(), crate::views::VIEW_BUDGET_BYTES).await.unwrap();
            serde_json::from_slice(&bytes).unwrap()
        }
        let path = "/views/pull_request:pr-2?protocol=1&client=mac&lang=en";
        let initial = get(&client, path).await;
        assert_eq!(initial["title"], "PR #2");
        let source = ControlledView::new(false);
        let mut refresh = Box::pin(client.details.refresh(&state, &source));
        poll_pending(refresh.as_mut());
        let partial = get(&client, &format!("{path}&known={}", initial["version"].as_str().unwrap())).await;
        assert_eq!(partial["title"], "Provider PR 2", "the real view GET does not wait for the other PR's provider");
        assert_ne!(partial["unchanged"], true, "the previously rendered fallback does not mask the new details");
        assert!(client.details.get("pr-1").is_none(), "first provider remains held while the second is visible");
        assert_eq!(source.metrics().active, 1);
        assert_eq!(domain.state().unwrap(), state, "publishing readonly details changes no domain state");
        source.release();
        refresh.await;
    }

    #[tokio::test]
    async fn provider_overlapping_refreshes_share_the_pending_read_bound() {
        let state = watched_prs(6);
        let details = PrDetails::default();
        let source = ControlledView::new(true);
        let mut first = Box::pin(details.refresh(&state, &source));
        let mut second = Box::pin(details.refresh(&state, &source));
        poll_pending(first.as_mut());
        poll_pending(second.as_mut());
        let pending = source.metrics();
        assert!(pending.active <= 2 && pending.started.len() <= 2,
            "overlapping refreshes of the same cache may not admit two sets of pending reads");
        assert!(pending.peak_body_bytes <= 2 * 4096);
        drop(first);
        drop(second);
        assert_eq!((source.metrics().active, source.metrics().active_body_bytes), (0, 0));
    }

    #[tokio::test]
    async fn provider_pending_read_admission_and_cancellation_are_bounded() {
        let state = watched_prs(6);
        let details = PrDetails::default();
        let source = ControlledView::new(true);
        let mut refresh = Box::pin(details.refresh(&state, &source));
        poll_pending(refresh.as_mut());
        let pending = source.metrics();
        assert!(!pending.started.is_empty());
        assert!(pending.started.len() <= 2, "only two pending provider reads may be admitted");
        assert!(pending.active <= 2 && pending.peak_body_bytes <= 2 * 4096);
        drop(refresh);
        assert_eq!((source.metrics().active, source.metrics().active_body_bytes), (0, 0),
            "dropping the refresh drops all its owned provider futures and bodies");
    }

    #[test]
    fn details_keep_the_change_size_and_each_check_by_name() {
        let details = parse("pr-1", 7, &json!({
            "title": "Editor", "additions": 120, "deletions": 30, "changedFiles": 4,
            "mergeable": "MERGEABLE", "state": "OPEN",
            "statusCheckRollup": [
                {"name": "build", "status": "COMPLETED", "conclusion": "SUCCESS"},
                {"name": "ui tests", "status": "IN_PROGRESS"}]
        }));
        assert_eq!((details.additions, details.deletions, details.changed_files), (120, 30, 4));
        assert_eq!(details.checks.iter().map(|check| check.name.as_str()).collect::<Vec<_>>(), ["build", "ui tests"]);
    }
}
