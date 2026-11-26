//! A pull request is its provider key, not its address. A read the adapter confirmed moves the
//! address to where the pull request lives now and keeps the old one as an alias; two records
//! of one key join into one, with every reference and decision of both and no remote effect.
use super::*;

pub(crate) fn observe_pr_address(domain: &mut DomainState, pr_id: &PrId, read: &PrRead, read_started_at: Millis, verdict: &PrAddressVerdict,
    effects: &mut Vec<DomainEffect>) -> Result<()> {
    if read_started_at <= 0 {
        return fail("invalid_input", "read_started_at must be a time");
    }
    let pr_id = &domain.surviving_pr(pr_id).clone();
    let pr = domain.pull_requests.get(pr_id).ok_or_else(|| DomainError::new("not_found", "PR missing"))?;
    if let PrRead::Address { repository } = read {
        canonical_pr(&pr.host, repository, pr.number)?;
    }
    if !pr.is_tracked() {
        return Ok(());
    }
    let stale = read_started_at <= pr.address_read_at;
    match verdict {
        PrAddressVerdict::Unreachable => {}
        PrAddressVerdict::Confirmed { key, locator, current_repository } => {
            nonempty(&key.0)?;
            nonempty(&locator.0)?;
            canonical_pr(&pr.host, current_repository, pr.number)?;
            if pr.identity.key().is_some_and(|known| known != key) {
                return address_lost(domain, pr_id, read, read_started_at, stale, effects);
            }
            let newer_miss = match &pr.identity {
                PrIdentity::Unresolvable { key: None, reason: UnresolvableReason::AddressReused, since, .. } if *since > read_started_at => Some(*since),
                _ => None,
            };
            confirm(domain, pr_id, read, read_started_at, key, locator, current_repository);
            if let Some(since) = newer_miss {
                effects.push(DomainEffect::Relocate { pr_id: pr_id.clone(), locator: domain.pull_requests[pr_id].identity.locator().cloned().unwrap() });
                domain.pull_requests.get_mut(pr_id).unwrap().relocation_asked_at = Some(since);
            }
            confirm_unverified_at_the_same_address(domain, pr_id, read, current_repository);
            join_same_pr(domain, pr_id, effects)?;
            one_record_per_address(domain, pr_id, read_started_at, effects);
        }
        PrAddressVerdict::NamesAnotherPr if !stale && pr.identity.is_unverified() && matches!(read, PrRead::Address { .. }) => {
            let pr = domain.pull_requests.get_mut(pr_id).unwrap();
            pr.identity = PrIdentity::Unresolvable { key: None, locator: None, reason: UnresolvableReason::AddressReused, since: read_started_at };
        }
        PrAddressVerdict::NamesAnotherPr | PrAddressVerdict::NotFound => {
            address_lost(domain, pr_id, read, read_started_at, stale, effects)?;
        }
    }
    Ok(())
}
/// The read did not reach this pull request: a verified one is looked for by its locator,
/// anything else counts one more definitive answer that it is gone.
pub(crate) fn address_lost(domain: &mut DomainState, pr_id: &PrId, read: &PrRead, at: Millis, stale: bool, effects: &mut Vec<DomainEffect>) -> Result<()> {
    let pr = &domain.pull_requests[pr_id];
    if stale || pr.identity.is_unresolvable() {
        return Ok(());
    }
    if let (PrRead::Address { .. }, Some(locator)) = (read, pr.identity.locator()) {
        if pr.relocation_asked_at.is_none_or(|asked| asked < at) {
            effects.push(DomainEffect::Relocate { pr_id: pr_id.clone(), locator: locator.clone() });
            domain.pull_requests.get_mut(pr_id).unwrap().relocation_asked_at = Some(at);
        }
        return Ok(());
    }
    let pr = domain.pull_requests.get_mut(pr_id).unwrap();
    pr.missing = match pr.missing {
        None => Some(PrMissing { strikes: 1, last: at }),
        Some(m) if at >= m.last.saturating_add(STRIKE_GAP) => Some(PrMissing { strikes: m.strikes.saturating_add(1), last: at }),
        kept => kept,
    };
    if pr.missing.is_some_and(|m| m.strikes >= GONE_STRIKES) {
        pr.identity = PrIdentity::Unresolvable { key: pr.identity.key().cloned(), locator: pr.identity.locator().cloned(), reason: UnresolvableReason::Gone, since: at };
    }
    Ok(())
}
pub(crate) fn confirm(domain: &mut DomainState, pr_id: &PrId, read: &PrRead, at: Millis, key: &ProviderPrKey, locator: &ProviderLocator, current: &str) {
    let pr = domain.pull_requests.get_mut(pr_id).unwrap();
    let newer = at > pr.address_read_at;
    let mut known = std::mem::take(&mut pr.aliases);
    known.insert(pr.repository.clone());
    known.insert(current.to_owned());
    if let PrRead::Address { repository } = read {
        known.insert(repository.clone());
    }
    let locator = match (&pr.identity, newer) {
        (PrIdentity::Verified { locator: kept, .. }, false) => kept.clone(),
        _ => locator.clone(),
    };
    if newer {
        pr.repository = current.to_owned();
        pr.address_read_at = at;
    }
    known.remove(&pr.repository);
    pr.aliases = known;
    pr.identity = PrIdentity::Verified { key: key.clone(), locator };
    if pr.missing.is_some_and(|m| m.last < at) {
        pr.missing = None;
    }
    if pr.relocation_asked_at.is_some_and(|asked| asked < at) {
        pr.relocation_asked_at = None;
    }
}
/// The read says which pull request its address and the current one are now, so a record
/// nobody checked yet that lives at either of them and stands at the same commit is that pull
/// request too. Another commit is evidence of another pull request, so it stays unchecked.
pub(crate) fn confirm_unverified_at_the_same_address(domain: &mut DomainState, pr_id: &PrId, read: &PrRead, current: &str) {
    let pr = &domain.pull_requests[pr_id];
    let (host, number, head, identity) = (pr.host.clone(), pr.number, pr.head.clone(), pr.identity.clone());
    let mut addresses = vec![current];
    if let PrRead::Address { repository } = read {
        addresses.push(repository);
    }
    for other in domain.pull_requests.values_mut() {
        if other.id != *pr_id && other.is_tracked() && other.identity.is_unverified() && other.head == head
            && addresses.iter().any(|address| other.lives_at(&host, address, number))
        {
            other.identity = identity.clone();
        }
    }
}
/// Two tracked records of different pull requests never stand at one address: the one whose
/// address was read last keeps it, and the other no longer knows where it lives, so it acts
/// nowhere and is looked for by its locator.
pub(crate) fn one_record_per_address(domain: &mut DomainState, pr_id: &PrId, at: Millis, effects: &mut Vec<DomainEffect>) {
    let pr = &domain.pull_requests[domain.surviving_pr(pr_id)];
    let (host, repository, number) = (pr.host.clone(), pr.repository.clone(), pr.number);
    let mut here: Vec<&PullRequest> = domain.pull_requests.values()
        .filter(|p| p.is_tracked() && !p.identity.is_unresolvable() && p.lives_at(&host, &repository, number))
        .collect();
    here.sort_by_key(|p| (std::cmp::Reverse(p.address_read_at), p.id.clone()));
    let displaced: Vec<PrId> = here.iter().skip(1).map(|p| p.id.clone()).collect();
    for id in displaced {
        let pr = domain.pull_requests.get_mut(&id).unwrap();
        if let Some(locator) = pr.identity.locator().cloned() {
            effects.push(DomainEffect::Relocate { pr_id: id.clone(), locator });
            pr.relocation_asked_at = Some(at);
        }
        pr.identity = PrIdentity::Unresolvable { key: pr.identity.key().cloned(), locator: pr.identity.locator().cloned(), reason: UnresolvableReason::AddressReused, since: at };
    }
}
/// Every other tracked record of the same key joins this one or takes it in.
pub(crate) fn join_same_pr(domain: &mut DomainState, pr_id: &PrId, effects: &mut Vec<DomainEffect>) -> Result<()> {
    let pr = &domain.pull_requests[pr_id];
    let Some(key) = pr.identity.key().cloned() else { return Ok(()) };
    let host = pr.host.clone();
    let mut same: Vec<PrId> = domain.pull_requests.values()
        .filter(|other| other.is_tracked() && other.host == host && other.identity.key() == Some(&key))
        .map(|other| other.id.clone())
        .collect();
    if same.len() < 2 {
        return Ok(());
    }
    same.sort_by_key(|id| survivor_rank(domain, id));
    let survivor = same.remove(0);
    for loser in same {
        join(domain, &survivor, &loser, effects)?;
    }
    Ok(())
}
/// The record that stays when several stand for one pull request, by a fixed order and never
/// by which was read first: a live task's, the earlier task's, the earlier in its task, the
/// lower id.
pub(crate) fn survivor_rank(domain: &DomainState, pr_id: &PrId) -> (bool, Revision, TaskId, usize, PrId) {
    let pr = &domain.pull_requests[pr_id];
    let task = domain.tasks.get(&pr.task_id);
    (
        task.is_none_or(|t| t.state.terminal()),
        task.map_or(Revision::MAX, |t| t.created_revision),
        pr.task_id.clone(),
        task.and_then(|t| t.pr_ids().position(|id| id == pr_id)).unwrap_or(usize::MAX),
        pr_id.clone(),
    )
}
pub(crate) fn join(domain: &mut DomainState, survivor: &PrId, loser: &PrId, effects: &mut Vec<DomainEffect>) -> Result<()> {
    let kept = domain.pull_requests[survivor].clone();
    let gone = domain.pull_requests[loser].clone();
    let newest = if gone.address_read_at > kept.address_read_at { &gone } else { &kept };
    let mut known: BTreeSet<String> = kept.aliases.union(&gone.aliases).cloned().collect();
    known.insert(kept.repository.clone());
    known.insert(gone.repository.clone());
    known.remove(&newest.repository);
    let missing = [kept.missing, gone.missing].into_iter().flatten().max_by_key(|m| m.last)
        .filter(|m| m.last > newest.address_read_at);
    let review = match (&kept.review, &gone.review) {
        (k, _) if kept.task_id != gone.task_id => k.clone(),
        (Some(k), Some(g)) => {
            let mut decisions = k.decisions.clone();
            for d in &g.decisions {
                if !decisions.contains(d) {
                    decisions.push(d.clone());
                }
            }
            decisions.sort_by_key(|d| d.at_revision);
            Some(PullRequestReview { decisions })
        }
        (k, g) => k.clone().or_else(|| g.clone()),
    };
    let pr = domain.pull_requests.get_mut(survivor).unwrap();
    pr.repository = newest.repository.clone();
    pr.address_read_at = newest.address_read_at;
    pr.identity = newest.identity.clone();
    pr.aliases = known;
    pr.missing = missing;
    pr.review = review;
    let identity = pr.identity.clone();
    for copy in domain.pull_requests.values_mut() {
        if matches!(&copy.tracking, PrTracking::Superseded { by } if by == loser) {
            copy.tracking = PrTracking::Superseded { by: survivor.clone() };
        }
    }
    let lost = domain.pull_requests.get_mut(loser).unwrap();
    lost.tracking = PrTracking::Superseded { by: survivor.clone() };
    lost.identity = identity;
    if kept.task_id == gone.task_id && let Some(review) = &mut lost.review {
        review.decisions.clear();
    }
    if let Some(owners) = domain.pr_task_history.get(loser).cloned() {
        let history = domain.pr_task_history.entry(survivor.clone()).or_default();
        history.extend(owners);
        history.insert(kept.task_id.clone());
    }
    if kept.task_id == gone.task_id {
        move_references(domain, &kept, &gone);
    } else if let Some(t) = domain.tasks.get(&gone.task_id).filter(|t| !t.state.terminal()) {
        let (task, title) = (t.id.clone(), domain.tasks[&kept.task_id].title.clone());
        let t = domain.tasks.get_mut(&task).unwrap();
        let ids: Vec<PrId> = t.pr_ids().filter(|id| *id != loser).cloned().collect();
        t.pr_id = ids.first().cloned();
        t.later_prs = ids.into_iter().skip(1).collect();
        notify(domain, &task, None, &joined_elsewhere_notice(gone.number, &title), effects)?;
    }
    Ok(())
}
/// Everything of the task that named the copy names the record that stays.
pub(crate) fn move_references(domain: &mut DomainState, kept: &PullRequest, gone: &PullRequest) {
    let (survivor, loser, task) = (&kept.id, &gone.id, &kept.task_id);
    let next = domain.revision + 1;
    let t = domain.tasks.get_mut(task).unwrap();
    let mut ids: Vec<PrId> = Vec::new();
    for id in t.pr_ids() {
        let id = if id == loser { survivor } else { id };
        if !ids.contains(id) {
            ids.push(id.clone());
        }
    }
    t.pr_id = ids.first().cloned();
    t.later_prs = ids.into_iter().skip(1).collect();
    let addresses: BTreeSet<&String> = gone.aliases.iter().chain(&kept.aliases).chain([&gone.repository, &kept.repository]).collect();
    t.opened_prs.retain(|o| !(o.host == gone.host && o.number == gone.number && addresses.contains(&o.repository)));
    t.revision = next;
    for i in domain.interactions.values_mut().filter(|i| i.scope == Scope::Task { task_id: task.clone() }) {
        let mut changed = false;
        if let InteractionPayload::Delivery { targets, feedback, .. } = &mut i.payload {
            let mut moved: Vec<DeliveryTarget> = Vec::with_capacity(targets.len());
            for target in targets.drain(..) {
                let target = match target {
                    DeliveryTarget::PullRequest { pr_id, head } if pr_id == *loser => {
                        changed = true;
                        DeliveryTarget::PullRequest { pr_id: survivor.clone(), head }
                    }
                    other => other,
                };
                let duplicate = moved.iter().any(|m| matches!((m, &target),
                    (DeliveryTarget::PullRequest { pr_id: a, .. }, DeliveryTarget::PullRequest { pr_id: b, .. }) if a == b));
                if !duplicate {
                    moved.push(target);
                }
            }
            *targets = moved;
            for f in feedback.iter_mut() {
                if let FeedbackTarget::PullRequest { pr_id, .. } = &mut f.target && pr_id == loser {
                    *pr_id = survivor.clone();
                    changed = true;
                }
            }
        }
        if let Some(InteractionSubject::PullRequest { pr_id }) = &mut i.subject && pr_id == loser {
            *pr_id = survivor.clone();
            changed = true;
        }
        for block in i.card.blocks.iter_mut() {
            if let CardTarget::PullRequest { pr_id } = block && pr_id == loser {
                *pr_id = survivor.clone();
                changed = true;
            }
        }
        if changed {
            i.revision = next;
        }
    }
    for t in domain.tasks.values_mut() {
        if let Some(stack) = &mut t.stack && stack.upstream_task == *task && stack.pr_id == *loser {
            stack.pr_id = survivor.clone();
        }
    }
    if let Some(readiness) = domain.stack_readiness.get_mut(task) && readiness.pr_id == *loser {
        readiness.pr_id = survivor.clone();
    }
    let presentations: BTreeSet<InteractionId> = domain.interactions.values()
        .filter(|i| i.scope == Scope::Task { task_id: task.clone() }).map(|i| i.id.clone()).collect();
    for (presentation, marks) in domain.checklist_marks.iter_mut() {
        if !presentations.contains(presentation) {
            continue;
        }
        for evidence in marks.iter_mut().flat_map(|m| m.evidence.iter_mut()) {
            if evidence.kind == EvidenceKind::Pr && evidence.value == loser.0 {
                evidence.value = survivor.0.clone();
            }
        }
    }
    for a in domain.merge_authorizations.values_mut().filter(|a| a.pr_id == *loser) {
        a.pr_id = survivor.clone();
    }
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut order: Vec<&mut MergeAuthorization> = domain.merge_authorizations.values_mut()
        .filter(|a| a.pr_id == *survivor && a.valid)
        .collect();
    order.sort_by_key(|a| (a.at_revision, a.id.clone()));
    for a in order {
        if !seen.insert(a.head.clone()) && a.attempt == MergeAttemptState::Unclaimed {
            a.valid = false;
        }
    }
}
/// The user takes a pull request Alondra cannot check out of it: nothing reaches the provider,
/// and the task goes on as after a pull request closed without merging.
pub(crate) fn detach_pr(domain: &mut DomainState, actor: &Actor, pr_id: &PrId, at: Millis, effects: &mut Vec<DomainEffect>, lifecycles: &mut Lifecycles) -> Result<()> {
    user(actor)?;
    let pr = domain.tracked_pr(pr_id)?;
    if !pr.identity.is_unresolvable() {
        return fail("pr_not_unresolvable", "Only a pull request Alondra cannot check can be removed from it");
    }
    let (task, number, was_open) = (pr.task_id.clone(), pr.number, pr.state == PrState::Open);
    domain.pull_requests.get_mut(pr_id).unwrap().tracking = PrTracking::Detached { at };
    domain.invalidate(&task);
    if !was_open {
        return Ok(());
    }
    let discarded = domain.active(&task).is_ok() && domain.task(&task).is_ok_and(|t| discarded(domain, t));
    if discarded {
        return domain.cancel(&task, effects, lifecycles);
    }
    if domain.active(&task).is_ok() {
        notify(domain, &task, None, &detached_notice(number), effects)?;
    }
    Ok(())
}
/// The id a new record takes: its address, or that with `~2`, `~3`… when an earlier record of
/// that address, joined, removed or unresolvable, already holds it.
pub(crate) fn fresh_pr_id(domain: &DomainState, wanted: &PrId) -> PrId {
    if !domain.pull_requests.contains_key(wanted) {
        return wanted.clone();
    }
    (2..).map(|n| PrId(format!("{}~{n}", wanted.0))).find(|id| !domain.pull_requests.contains_key(id)).unwrap()
}
/// The tracked record a new address names, if the provider has not said it is another one.
pub(crate) fn known_pr<'a>(domain: &'a DomainState, host: &str, repository: &str, number: u64, by_alias: bool) -> Option<&'a PullRequest> {
    domain.pull_requests.values().find(|p| p.is_tracked() && !p.identity.is_unresolvable()
        && if by_alias { p.answers_to(host, repository, number) } else { p.lives_at(host, repository, number) })
}
