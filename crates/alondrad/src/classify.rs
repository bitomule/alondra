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

fn asks_for_action(line: &str) -> bool {
    let lower = line.to_lowercase();
    // Read each sentence or clause, so a request after a status update is still found.
    lower.split(['.', ';', ':']).any(|part| {
        let part = part.trim().trim_start_matches(['-', '*', ' ']);
        if (part.starts_with("please note") || part.starts_with("ten en cuenta"))
            && !["approval is required", "approve", "need you to", "need your", "aprobación es necesaria", "necesito tu"]
                .iter().any(|marker| part.contains(marker)) {
            return false;
        }
        [
            "please ", "could you ", "can you ", "you need to ", "i need you to ",
            "approve ", "review ", "test ", "confirm ", "send me ", "provide ",
            "sign off ", "choose ", "decide ",
            "por favor ", "puedes ", "podrías ", "necesito que ", "necesita que ",
            "necesito tu ", "necesito tus ", "necesitamos tu ", "necesitamos tus ",
            "revisa ", "confirma ", "aprueba ", "prueba ", "decide ", "elige ",
            "envíame ", "enviame ", "mándame ", "mandame ", "pásame ", "pasame ", "dame ",
        ].iter().any(|marker| part.starts_with(marker))
            || ["need your approval", "need you to ", "require your approval", "your approval is required",
                "waiting for your ", "waits for your ", "awaits your ",
                "esperando tu ", "a la espera de tu ",
                "necesito tu aprobación",
                "tu aprobación es necesaria"].iter().any(|marker| part.contains(marker))
    })
}

/// Flag an explicit question or request. An ambiguous report remains visible in the inbox.
fn asks_question(line: &str) -> bool {
    let lower = line.trim().trim_start_matches(['#', '*', '-', ' ']).to_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") { return false; }
    let (before, after) = lower.split_once('?').unwrap_or((&lower, ""));
    let heading = ["what changed", "what was tested", "how was it tested", "qué cambió", "que cambió"];
    let heading_only = heading.contains(&before.trim().trim_start_matches('¿').trim());
    if heading_only { after.contains('?') || after.contains('¿') }
    else { line.contains('?') || line.contains('¿') }
}

pub fn judge_report(text: &str) -> ReportVerdict {
    let asks = markdown::prose_lines(text).into_iter().any(|line| {
        asks_question(line) || asks_for_action(line)
    });
    if asks { ReportVerdict::Asks } else { ReportVerdict::Fine }
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
    if lines.iter().any(|line| asks_for_action(line)) {
        return Expects::Act;
    }
    if lines.iter().any(|line| asks_question(line)) {
        return Expects::Decide;
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

/// Local word matching across node details. Results retain fleet order.
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
                .all(|term| hay.iter().any(|word| word.contains(term)))
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
    fn imperative_requests_are_not_final_reports() {
        for text in ["Test it on your iPhone before I publish.", "Approve it and I start.",
            "Necesito tu token para continuar.", "Envíame la clave.", "Sign off the rollout."] {
            assert_eq!(judge_report(text), ReportVerdict::Asks, "{text}");
        }
        assert_eq!(judge_report("Please note that the migration is complete."), ReportVerdict::Fine);
        assert_eq!(judge_report("What changed? Fixed camera permissions. Tests pass."), ReportVerdict::Fine);
        assert_eq!(judge_report("¿Qué cambió? Corregí el permiso. Pasan las pruebas."), ReportVerdict::Fine);
        assert_eq!(judge_report("What changed? Fixed the build. Should I merge?"), ReportVerdict::Asks);
        assert_eq!(judge_report("https://example.com/result?v=2"), ReportVerdict::Fine);
        assert_eq!(judge_report("Thanks for your review; merged and shipped."), ReportVerdict::Fine);
        assert_eq!(judge_report("Thanks for your approval; the release is live."), ReportVerdict::Fine);
        for text in ["Waiting for your approval before shipping.", "We need you to approve the release.",
            "Please note that your approval is required before release.",
            "The build awaits your TestFlight review before release."] {
            assert_eq!(judge_report(text), ReportVerdict::Asks, "{text}");
        }
        assert_eq!(what_node_expects("Can you test on your iPhone?"), Expects::Act);
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
