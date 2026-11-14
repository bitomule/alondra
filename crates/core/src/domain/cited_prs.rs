//! PR references in a delivery are checked against its typed targets, by address rather than
//! local ids: numbers repeat across repositories and a task can deliver work in several of them.
use super::*;

#[derive(Debug, PartialEq, Eq)]
struct Citation {
    host: Option<String>,
    repository: String,
    number: u64,
}

fn citations(text: &str) -> impl Iterator<Item = Citation> + '_ {
    text.split(|c: char| c.is_whitespace() || "()<>[]\"'`,;*~".contains(c)).filter_map(|word| {
        let word = word.trim_end_matches(['.', ':', '!', '?']);
        let word = if word.starts_with('_') && word.ends_with('_') { word.trim_matches('_') } else { word };
        let (host, repository, number) = if let Some(url) = word.strip_prefix("https://").or_else(|| word.strip_prefix("http://")) {
            let (host, path) = url.split_once('/')?;
            let (repository, tail) = path.split_once("/pull/")?;
            let number = tail.split(['/', '#', '?']).next()?;
            (Some(normalize_pr_host(host)), repository, number)
        } else {
            let (repository, number) = word.split_once('#')?;
            (None, repository, number)
        };
        let (owner, name) = repository.split_once('/')?;
        let plain = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
        if !plain(owner) || !plain(name) || host.as_ref().is_some_and(|host| !host.contains('.') || !plain(host)) { return None; }
        Some(Citation { host, repository: normalize_pr_repository(repository), number: number.parse::<u64>().ok().filter(|n| *n > 0)? })
    })
}

impl DomainState {
    pub(super) fn cited_pr_targets(&self, body: &str, targets: &[DeliveryTarget], card: &CardSpec, is_final: bool) -> Result<(), DomainError> {
        let card_type = if !is_final { CardType::Progress } else if card.plan.is_some() { CardType::ProposedPlan } else { CardType::Result };
        let fields = [("body", Some(body)), ("card.title", card.title.as_deref()), ("card.need", Some(card.need.as_str())),
            ("card.why", Some(card.why.as_str())), ("card.content.body", card.content.as_ref().map(|c| c.body.as_str())),
            ("card.content.summary", card.content.as_ref().and_then(|c| c.summary.as_deref())), ("card.plan", card.plan.as_deref())];
        let texts = targets.iter().filter_map(|t| match t { DeliveryTarget::Text { text } => Some(("targets.text", Some(text.as_str()))), _ => None });
        for (field, text) in fields.into_iter().chain(texts) {
            for citation in citations(text.unwrap_or_default()) {
                let carried = targets.iter().any(|t| match t {
                    DeliveryTarget::PullRequest { pr_id, .. } => self.pull_requests.get(pr_id).is_some_and(|pr|
                        normalize_pr_repository(&pr.repository) == citation.repository && pr.number == citation.number
                            && citation.host.as_ref().is_none_or(|host| normalize_pr_host(&pr.host) == *host)),
                    _ => false,
                });
                if !carried {
                    let mut error = DomainError::new("cited_pr_not_target", format!("{field}: cites {}#{}, which is not a delivery target: include the registered PR in targets or remove the citation", citation.repository, citation.number));
                    error.subject = Some(field.into());
                    return Err(error.breaking(Some(card_type), ContractRule::CitedPrNotTarget));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn addresses_keep_host_repository_and_number_and_ignore_issue_links() {
        let found: Vec<_> = citations("[diff](https://GitHub.com/Acme/App/pull/65/files#diff-x), acme/site#14. <https://git.example.com/acme/app/pull/65?tab=files> https://github.com/acme/app/issues/65 #65 acme/app#0").collect();
        assert_eq!(found, vec![
            Citation { host: Some("github.com".into()), repository: "acme/app".into(), number: 65 },
            Citation { host: None, repository: "acme/site".into(), number: 14 },
            Citation { host: Some("git.example.com".into()), repository: "acme/app".into(), number: 65 },
        ]);
    }
}
