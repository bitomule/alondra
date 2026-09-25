//! Local, conservative interpretation of reports and quiet nodes.
//! Unclear terminal messages never become completion evidence.

use crate::markdown;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReportVerdict {
    Asks,
    Fine,
}

pub trait JudgesReports: Send + Sync {
    fn judge<'a>(
        &'a self,
        text: &'a str,
        user: &'a str,
    ) -> std::pin::Pin<Box<dyn Future<Output = ReportVerdict> + Send + 'a>>;
}

pub struct LocalJudge;

impl JudgesReports for LocalJudge {
    fn judge<'a>(
        &'a self,
        text: &'a str,
        _user: &'a str,
    ) -> std::pin::Pin<Box<dyn Future<Output = ReportVerdict> + Send + 'a>> {
        Box::pin(async move { judge_report(text) })
    }
}

/// Flag an explicit question or request. An ambiguous report remains visible in the inbox.
pub fn judge_report(text: &str) -> ReportVerdict {
    let asks = markdown::prose_lines(text).into_iter().any(|line| {
        let lower = line.to_lowercase();
        line.contains('?')
            || line.contains('¿')
            || [
                "please ",
                "could you ",
                "can you ",
                "need you to ",
                "your approval",
                "you need to ",
                "por favor ",
                "puedes ",
                "podrías ",
                "necesito que ",
                "necesita que ",
                "tu aprobación",
                "revisa ",
                "confirma ",
                "aprueba ",
            ]
            .iter()
            .any(|marker| lower.contains(marker))
    });
    if asks {
        ReportVerdict::Asks
    } else {
        ReportVerdict::Fine
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expects {
    Decide,
    Act,
    Unsure,
}

/// The sentinel may add a question or action, never infer that an unclear task is done.
pub fn what_node_expects(text: &str) -> Expects {
    let lines = markdown::prose_lines(text);
    if lines
        .iter()
        .any(|line| line.contains('?') || line.contains('¿'))
    {
        return Expects::Decide;
    }
    let lower = lines.join(" ").to_lowercase();
    if [
        "please ",
        "could you ",
        "can you ",
        "need you to ",
        "you need to ",
        "por favor ",
        "necesito que ",
        "necesita que ",
        "revisa ",
        "confirma ",
        "aprueba ",
        "test on ",
        "prueba en ",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        return Expects::Act;
    }
    Expects::Unsure
}

fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Word and prefix matching across locally held node details. Results retain fleet order.
pub fn search(query: &str, candidates: Vec<(String, String)>) -> Vec<String> {
    let terms = words(query);
    if terms.is_empty() {
        return vec![];
    }
    candidates
        .into_iter()
        .filter_map(|(session, text)| {
            let hay = words(&text);
            terms
                .iter()
                .all(|term| hay.iter().any(|word| word.starts_with(term)))
                .then_some(session)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn questions_and_actions_remain_visible() {
        assert_eq!(
            what_node_expects("Done. Should I merge the PR?"),
            Expects::Decide
        );
        assert_eq!(what_node_expects("Please test on a device."), Expects::Act);
        assert_eq!(what_node_expects("Everything committed."), Expects::Unsure);
        assert_eq!(
            judge_report("Summary\n\n¿Lo apruebas?"),
            ReportVerdict::Asks
        );
        assert_eq!(judge_report("Everything committed."), ReportVerdict::Fine);
        assert_eq!(
            judge_report("```\nShould I merge?\n```\nTests pass."),
            ReportVerdict::Fine
        );
    }

    #[test]
    fn local_search_matches_words_without_a_service() {
        let candidates = vec![
            ("a".into(), "Lumo camera permission".into()),
            ("b".into(), "Quill release".into()),
        ];
        assert_eq!(search("cam perm", candidates), vec!["a"]);
    }
}
