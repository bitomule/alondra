//! The notification of the engine's over-pace card (`daemon-engine`'s `usage_pause`), as `views`
//! words it for each device's language.
use crate::usage_pause::fixtures::*;
use crate::usage_pause::raise_over_pace_card;
use alondra_core::now_millis;

#[test]
fn the_over_pace_card_has_the_numbers_and_two_options_and_reads_in_each_devices_language() {
    super::wired();
    use alondra_views::{Lang, push_content};
    let (domain, home) = over_pace_domain();
    raise_over_pace_card(&domain, now_millis());
    let card = the_card(&domain, "over-pace-").expect("a card for the day");
    assert_eq!(card.id.0, format!("over-pace-{}", alondra_daemon_engine_model::local_day(now_millis())));
    assert_eq!(card.card.kind, alondra_domain_rules::CardKind::Decide);
    assert_eq!(card.author, alondra_domain_rules::Author::System);
    assert!(!card.blocking);
    assert_eq!(card.card.recommended.as_deref(), Some("keep_going"));
    let alondra_domain_rules::InteractionPayload::Question { options, .. } = &card.payload else { panic!("a question") };
    assert_eq!(options.iter().map(|option| option.id.as_str()).collect::<Vec<_>>(), ["pause_until_tomorrow", "keep_going"]);

    let read = domain.read();
    let english = push_content(&read, &card.id.0, Lang::En).unwrap();
    let spanish = push_content(&read, &card.id.0, Lang::Es).unwrap();
    assert_eq!(english.interruption_level, "passive");
    assert_eq!(english.alert.as_ref().unwrap().title, "Today is over its share of the weekly Claude usage");
    assert_eq!(english.alert.unwrap().body.as_deref(), Some("41% used today against a 18% share · 53% of the week used"));
    assert_eq!(spanish.alert.as_ref().unwrap().title, "Hoy vas por encima de tu parte del uso semanal");
    assert_eq!(spanish.alert.unwrap().body.as_deref(), Some("41% usado hoy frente a una parte del 18% · 53% de la semana usado"));
    let labels = |lang| push_content(&read, &card.id.0, lang).unwrap().actions.into_iter().map(|action| action.label).collect::<Vec<_>>();
    let english_labels = labels(Lang::En);
    let spanish_labels = labels(Lang::Es);
    assert!(english_labels.iter().any(|label| label == "Pause until tomorrow") && english_labels.iter().any(|label| label.ends_with("Keep going")), "{english_labels:?}");
    assert!(spanish_labels.iter().any(|label| label == "Pausar hasta mañana") && spanish_labels.iter().any(|label| label.ends_with("Seguir")), "{spanish_labels:?}");
    std::fs::remove_dir_all(home).unwrap();
}
