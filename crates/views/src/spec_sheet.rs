//! Builds the sheet of a spec or of a decision from the files Alondra keeps: what it holds now,
//! what the project had before, and the diagrams and mockups of each.
use super::{
    text::{t, tf},
    DecisionSheet, Icon, Lang, Link, SheetAnchor, SheetBackground, SheetChange, SheetLane, SheetLaneKind, SheetMark, SheetPart, SheetPartKind, SheetPicture,
    SheetReference, SheetReferences, SheetScenario, SheetSection, SheetStep, SheetTable, SheetUnchanged, SpecSheet,
};
use alondra_domain_model::specs::{self, engine, SpecContent, StoredImage};

/// The most scenarios a sheet draws open; with more, each is drawn closed.
const MOST_OPEN: usize = 8;

/// One version of a spec: its file and what each part it cites holds.
#[derive(Clone, Copy)]
pub(super) struct Version<'a> {
    pub text: &'a str,
    pub parts: &'a dyn Fn(&str) -> Option<SpecContent>,
}

/// What a sheet is drawn from.
pub(super) struct Source<'a> {
    pub path: &'a str,
    /// `None` when the change removes the spec.
    pub after: Option<Version<'a>>,
    /// `None` for a spec the project did not have.
    pub before: Option<Version<'a>>,
    /// The title of each decision it may cite, by identifier.
    pub titles: &'a std::collections::BTreeMap<String, String>,
}

/// Where the screens that edit a sheet are; `None` where nothing is edited.
pub(super) struct Editing<'a> {
    pub editor: &'a str,
}

fn part_word(kind: SheetPartKind) -> &'static str {
    match kind {
        SheetPartKind::Diagram => "diagram",
        _ => "mockup",
    }
}

impl Editing<'_> {
    /// The key of the form of a part of `target` (`s<spec>` or `c<scenario>`); `new` adds one.
    pub(super) fn part_key(&self, kind: SheetPartKind, target: &str, part: &str) -> String {
        format!("{}:part:{}:{target}:{part}", self.editor, part_word(kind))
    }

    /// Links each part that is still cited to the form that edits it.
    fn parts(&self, parts: &mut [SheetPart], target: &str) {
        for part in parts.iter_mut().filter(|part| part.mark.as_ref().is_none_or(|mark| mark.change != SheetChange::Removed)) {
            part.edit = Some(Link::View { key: self.part_key(part.kind, target, &specs::handle(&part.name)) });
        }
    }

    fn add_parts(&self, target: &str, lang: Lang) -> Vec<SheetReference> {
        let reference = |kind, title: &str| SheetReference { title: title.into(), link: Some(Link::View { key: self.part_key(kind, target, "new") }) };
        vec![
            reference(SheetPartKind::Diagram, t!(lang, "Add a diagram", "Añadir un diagrama")),
            reference(SheetPartKind::Mockup, t!(lang, "Add a mockup", "Añadir un mockup")),
        ]
    }
}

pub(super) fn mark(change: SheetChange, lang: Lang, feminine: bool) -> SheetMark {
    let label = match (change, feminine) {
        (SheetChange::New, true) => t!(lang, "new", "nueva"),
        (SheetChange::New, false) => t!(lang, "new", "nuevo"),
        (SheetChange::Changed, true) => t!(lang, "changed", "cambiada"),
        (SheetChange::Changed, false) => t!(lang, "changed", "cambiado"),
        (SheetChange::Removed, true) => t!(lang, "removed", "eliminada"),
        (SheetChange::Removed, false) => t!(lang, "removed", "eliminado"),
        (SheetChange::Unchanged, _) => t!(lang, "unchanged", "sin cambios"),
    };
    SheetMark { change, label: label.into() }
}

fn lane_label(kind: SheetLaneKind, lang: Lang) -> &'static str {
    match kind {
        SheetLaneKind::Given => t!(lang, "Given", "Partiendo de"),
        SheetLaneKind::When => t!(lang, "When", "Cuando"),
        SheetLaneKind::Then | SheetLaneKind::Other => t!(lang, "Then", "Entonces"),
    }
}

/// The steps in their lanes: `And` and `But` join the lane before them.
pub(super) fn lanes(steps: &[engine::Step], lang: Lang) -> Vec<SheetLane> {
    let mut lanes: Vec<SheetLane> = vec![];
    for step in steps {
        let kind = match step.keyword {
            engine::Keyword::Given => Some(SheetLaneKind::Given),
            engine::Keyword::When => Some(SheetLaneKind::When),
            engine::Keyword::Then => Some(SheetLaneKind::Then),
            engine::Keyword::And | engine::Keyword::But => None,
        };
        let drawn = SheetStep { text: step.text.clone(), table: step.table.clone(), doc: step.doc.clone() };
        match lanes.last_mut() {
            Some(lane) if kind.is_none_or(|kind| kind == lane.kind) => lane.steps.push(drawn),
            _ => {
                let kind = kind.unwrap_or(SheetLaneKind::Given);
                lanes.push(SheetLane { kind, label: lane_label(kind, lang).into(), steps: vec![drawn] });
            }
        }
    }
    lanes
}

pub(super) fn kind_label(scenario: &engine::Scenario, lang: Lang) -> String {
    match &scenario.judgment {
        None => t!(lang, "test", "test").into(),
        Some(judgment) => match (&judgment.pattern, judgment.evidence) {
            (Some(pattern), _) => tf!(lang, "judgment on `{pattern}`", "juicio sobre `{pattern}`"),
            (None, engine::EvidenceKind::Capture) => t!(lang, "judgment on captures", "juicio sobre capturas").into(),
            (None, engine::EvidenceKind::Video) => t!(lang, "judgment on a video", "juicio sobre un vídeo").into(),
            (None, engine::EvidenceKind::None) => t!(lang, "judgment", "juicio").into(),
        },
    }
}

/// «Refund flow» for `diagrams/refund-flow.mmd`.
fn part_name(file: &str) -> String {
    let stem = file.rsplit('/').next().unwrap_or(file);
    let stem = stem.rsplit_once('.').map_or(stem, |(stem, _)| stem);
    let words = stem.replace(['-', '_'], " ");
    let mut chars = words.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => file.to_owned(),
    }
}

pub(super) fn picture(image: &StoredImage) -> SheetPicture {
    SheetPicture { url: image.route(), width: image.width, height: image.height, bytes: image.bytes }
}

fn print(content: Option<&SpecContent>) -> String {
    specs::fingerprint(content)
}

struct Cited<'a> {
    kind: SheetPartKind,
    file: &'a str,
}

fn cited<'a>(diagrams: &'a [String], mockups: &'a [String]) -> Vec<Cited<'a>> {
    diagrams.iter().map(|file| Cited { kind: SheetPartKind::Diagram, file })
        .chain(mockups.iter().map(|file| Cited { kind: SheetPartKind::Mockup, file }))
        .collect()
}

/// What an illustrated thing is called in a part's alternative text and comments.
struct Owner<'a> {
    /// `<spec>` or `<spec>/<scenario>`.
    id: String,
    title: &'a str,
    scenario: bool,
}

struct Drawing<'a, 'b> {
    source: &'b Source<'a>,
    lang: Lang,
}

impl Drawing<'_, '_> {
    fn part(&self, owner: &Owner<'_>, item: &Cited<'_>, version: &Version<'_>, change: Option<SheetChange>) -> SheetPart {
        let lang = self.lang;
        let path = engine::part_path(self.source.path, item.file);
        let content = (version.parts)(&path);
        let name = part_name(item.file);
        let title = match item.kind {
            SheetPartKind::Diagram => tf!(lang, "Diagram “{name}”", "Diagrama «{name}»"),
            _ => tf!(lang, "Mockup “{name}”", "Mockup «{name}»"),
        };
        let what = &owner.title;
        let alt = if owner.scenario {
            tf!(lang, "{title} of the scenario “{what}”", "{title} del escenario «{what}»")
        } else {
            tf!(lang, "{title} of the spec “{what}”", "{title} de la spec «{what}»")
        };
        let (code, image) = match &content {
            Some(SpecContent::Text(text)) if item.kind == SheetPartKind::Diagram => (Some(text.clone()), None),
            Some(SpecContent::Mockup(image)) => (None, Some(picture(image))),
            _ => (None, None),
        };
        let unavailable = (code.is_none() && image.is_none()).then(|| tf!(lang, "{file} is not available", "{file} no está disponible", file = item.file));
        SheetPart {
            id: format!("part:{}:{}", owner.id, item.file),
            kind: item.kind,
            name: item.file.to_owned(),
            anchor: Some(SheetAnchor { id: format!("part:{}:{}", owner.id, item.file), quote: format!("{} · {title}", owner.id) }),
            title,
            alt,
            mark: change.map(|change| mark(change, lang, false)),
            code,
            picture: image,
            unavailable,
            edit: None,
        }
    }

    /// The parts of one owner, each marked against what it cited before, with those it no longer
    /// cites marked as removed.
    fn parts(&self, owner: &Owner<'_>, now: Option<(&[String], &[String])>, was: Option<(&[String], &[String])>, marked: bool) -> Vec<SheetPart> {
        let was_cited = was.map(|(diagrams, mockups)| cited(diagrams, mockups)).unwrap_or_default();
        let now_cited = now.map(|(diagrams, mockups)| cited(diagrams, mockups)).unwrap_or_default();
        let mut drawn = vec![];
        if let Some(after) = &self.source.after {
            for item in &now_cited {
                let path = engine::part_path(self.source.path, item.file);
                let change = self.source.before.as_ref().filter(|_| marked).map(|before| {
                    match was_cited.iter().any(|old| old.file == item.file && old.kind == item.kind) {
                        false => SheetChange::New,
                        true if print((before.parts)(&path).as_ref()) != print((after.parts)(&path).as_ref()) => SheetChange::Changed,
                        true => SheetChange::Unchanged,
                    }
                }).filter(|change| *change != SheetChange::Unchanged);
                drawn.push(self.part(owner, item, after, change));
            }
        }
        if let Some(before) = &self.source.before && marked {
            for item in was_cited.iter().filter(|old| !now_cited.iter().any(|item| item.file == old.file && item.kind == old.kind)) {
                drawn.push(self.part(owner, item, before, Some(SheetChange::Removed)));
            }
        }
        drawn
    }

    /// Whether a scenario reads the same, with the same parts, before and after.
    fn same(&self, now: &engine::Scenario, was: &engine::Scenario) -> bool {
        let words = |scenario: &engine::Scenario| (scenario.title.clone(), specs::scenario_body(scenario), scenario.judgment.clone(), scenario.promise, scenario.reserved);
        let parts = |scenario: &engine::Scenario, version: &Option<Version<'_>>| scenario.diagrams.iter().chain(&scenario.mockups)
            .map(|file| {
                let content = version.and_then(|version| (version.parts)(&engine::part_path(self.source.path, file)));
                (file.clone(), print(content.as_ref()))
            }).collect::<Vec<_>>();
        words(now) == words(was) && parts(now, &self.source.after) == parts(was, &self.source.before)
    }

    fn scenario(&self, spec: &str, scenario: &engine::Scenario, was: Option<&engine::Scenario>, change: Option<SheetChange>) -> SheetScenario {
        let id = format!("{spec}/{}", scenario.id);
        let owner = Owner { id: id.clone(), title: &scenario.title, scenario: true };
        let removed = change == Some(SheetChange::Removed);
        let now_parts = (!removed).then_some((&scenario.diagrams[..], &scenario.mockups[..]));
        let was_parts = was.map(|was| (&was.diagrams[..], &was.mockups[..]));
        SheetScenario {
            anchor: Some(SheetAnchor { id: format!("scenario:{id}"), quote: format!("{id} · {}", scenario.title) }),
            title: scenario.title.clone(),
            kind: kind_label(scenario, self.lang),
            mark: change.map(|change| mark(change, self.lang, false)),
            collapsed: false,
            lanes: lanes(&scenario.steps, self.lang),
            examples: (!scenario.examples.is_empty()).then(|| SheetTable { label: t!(self.lang, "Examples", "Ejemplos").into(), rows: scenario.examples.clone() }),
            parts: self.parts(&owner, now_parts, was_parts, change == Some(SheetChange::Changed) || removed),
            result: None,
            edit: None,
            add: vec![],
            id,
        }
    }
}

fn read(path: &str, version: Option<Version<'_>>) -> Option<Result<engine::Spec, engine::Problem>> {
    let version = version?;
    Some(engine::read_spec(path, version.text.as_bytes()).map_err(|problems| problems.into_iter().next().unwrap_or(engine::Problem {
        code: engine::Code::UnsupportedSyntax, path: path.to_owned(), line: 0, message: String::new(),
    })))
}

/// The sheet of a spec, or why it cannot be read, with its file and line.
pub(super) fn spec_sheet(source: &Source<'_>, editing: Option<&Editing<'_>>, lang: Lang) -> Result<SpecSheet, String> {
    let unreadable = |problem: engine::Problem| match problem.line {
        0 => tf!(lang, "{path} cannot be read: {message}", "No se puede leer {path}: {message}", path = problem.path, message = problem.message),
        line => tf!(lang, "{path}, line {line}: {message}", "{path}, línea {line}: {message}", path = problem.path, message = problem.message),
    };
    let after = read(source.path, source.after).transpose().map_err(unreadable)?;
    let before = read(source.path, source.before).and_then(Result::ok);
    let Some(shown) = after.as_ref().or(before.as_ref()) else {
        return Err(tf!(lang, "{path} cannot be read", "No se puede leer {path}", path = source.path));
    };
    let drawing = Drawing { source, lang };
    let change = match (&after, &before, source.before) {
        (None, _, _) => SheetChange::Removed,
        (Some(_), _, None) => SheetChange::New,
        (Some(now), Some(was), _) => {
            let header = |spec: &engine::Spec| (spec.title.clone(), spec.description.clone(), spec.decisions.clone(), specs::background_body(spec));
            let parts_same = now.diagrams == was.diagrams && now.mockups == was.mockups && now.diagrams.iter().chain(&now.mockups).all(|file| {
                let path = engine::part_path(source.path, file);
                let of = |version: Option<Version<'_>>| print(version.and_then(|version| (version.parts)(&path)).as_ref());
                of(source.after) == of(source.before)
            });
            let scenarios_same = now.scenarios.len() == was.scenarios.len()
                && now.scenarios.iter().zip(&was.scenarios).all(|(now, was)| now.id == was.id && drawing.same(now, was));
            if header(now) == header(was) && parts_same && scenarios_same { SheetChange::Unchanged } else { SheetChange::Changed }
        }
        (Some(_), None, Some(_)) => SheetChange::Changed,
    };

    let mut visible = vec![];
    let mut unchanged = vec![];
    for scenario in after.iter().flat_map(|spec| &spec.scenarios) {
        let was = before.iter().flat_map(|spec| &spec.scenarios).find(|was| was.id == scenario.id);
        let of_scenario = match (change, was) {
            (SheetChange::New, _) | (_, None) => SheetChange::New,
            (_, Some(was)) if drawing.same(scenario, was) => SheetChange::Unchanged,
            _ => SheetChange::Changed,
        };
        let drawn = drawing.scenario(&shown.id, scenario, was, Some(of_scenario));
        if of_scenario == SheetChange::Unchanged { unchanged.push(drawn) } else { visible.push(drawn) }
    }
    for was in before.iter().flat_map(|spec| &spec.scenarios) {
        if !after.iter().flat_map(|spec| &spec.scenarios).any(|now| now.id == was.id) {
            visible.push(drawing.scenario(&shown.id, was, Some(was), Some(SheetChange::Removed)));
        }
    }
    let total = visible.len() + unchanged.len();
    let many = visible.len() > MOST_OPEN;
    for scenario in &mut visible {
        scenario.collapsed = many;
    }
    for scenario in &mut unchanged {
        scenario.collapsed = true;
        scenario.mark = None;
    }
    if let Some(editing) = editing && let Some(after) = &after {
        for scenario in visible.iter_mut().chain(&mut unchanged) {
            let raw = scenario.id.rsplit('/').next().unwrap_or_default().to_owned();
            if after.scenarios.iter().any(|known| known.id == raw) {
                let handle = specs::scenario_handle(source.path, &raw);
                let target = format!("c{handle}");
                scenario.edit = Some(Link::View { key: format!("{}:scenario:{handle}", editing.editor) });
                editing.parts(&mut scenario.parts, &target);
                scenario.add = editing.add_parts(&target, lang);
            }
        }
    }

    let now_parts = after.as_ref().map(|spec| (&spec.diagrams[..], &spec.mockups[..]));
    let was_parts = before.as_ref().map(|spec| (&spec.diagrams[..], &spec.mockups[..]));
    let owner = Owner { id: shown.id.clone(), title: &shown.title, scenario: false };
    let unchanged = (!unchanged.is_empty()).then(|| SheetUnchanged {
        label: match unchanged.len() {
            1 => t!(lang, "1 unchanged scenario", "1 escenario sin cambios").into(),
            n => tf!(lang, "{n} unchanged scenarios", "{n} escenarios sin cambios"),
        },
        scenarios: unchanged,
    });
    let mut parts = drawing.parts(&owner, now_parts, was_parts, change == SheetChange::Changed);
    let mut add = vec![];
    if let Some(editing) = editing.filter(|_| after.is_some()) {
        let handle = specs::handle(source.path);
        let target = format!("s{handle}");
        editing.parts(&mut parts, &target);
        add.push(SheetReference { title: t!(lang, "Add a scenario", "Añadir un escenario").into(), link: Some(Link::View { key: format!("{}:scenario:new-{handle}", editing.editor) }) });
        add.extend(editing.add_parts(&target, lang));
    }
    Ok(SpecSheet {
        id: format!("spec:{}", shown.id),
        spec: shown.id.clone(),
        title: shown.title.clone(),
        mark: Some(mark(change, lang, true)),
        explanation: shown.description.join(" "),
        decisions: (!shown.decisions.is_empty()).then(|| SheetReferences {
            label: t!(lang, "Rests on", "Se apoya en").into(),
            items: shown.decisions.iter().map(|id| SheetReference { title: source.titles.get(id).unwrap_or(id).clone(), link: None }).collect(),
        }),
        parts,
        add,
        background: (!shown.background.is_empty()).then(|| SheetBackground { label: t!(lang, "Always first", "Siempre antes").into(), lanes: lanes(&shown.background, lang) }),
        scenarios: visible,
        unchanged,
        empty: (total == 0).then(|| t!(lang, "No scenario yet.", "Aún sin escenarios.").into()),
        anchor: Some(SheetAnchor { id: format!("spec:{}", shown.id), quote: format!("{} · {}", shown.id, shown.title) }),
        edit: editing.filter(|_| after.is_some()).map(|editing| Link::View { key: format!("{}:spec:{}", editing.editor, specs::handle(source.path)) }),
        actions: vec![],
    })
}

fn status_word(status: &str, lang: Lang) -> String {
    match status {
        "proposed" => t!(lang, "proposed", "propuesta").into(),
        "accepted" => t!(lang, "accepted", "aceptada").into(),
        "rejected" => t!(lang, "rejected", "rechazada").into(),
        "superseded" => t!(lang, "replaced", "sustituida").into(),
        "deprecated" => t!(lang, "deprecated", "en desuso").into(),
        other => other.to_owned(),
    }
}

/// The sheet of a decision from its file: `after` is `None` when the change removes it.
pub(super) fn decision_sheet(path: &str, after: Option<&str>, before: Option<&str>, editing: Option<&Editing<'_>>, lang: Lang) -> Option<DecisionSheet> {
    let text = after.or(before)?;
    let fields = specs::decision_fields(text);
    let change = match (after, before) {
        (None, _) => SheetChange::Removed,
        (_, None) => SheetChange::New,
        (Some(now), Some(was)) if now == was => SheetChange::Unchanged,
        _ => SheetChange::Changed,
    };
    let references = |label: &str, ids: Vec<String>| (!ids.is_empty()).then(|| SheetReferences {
        label: label.into(),
        items: ids.into_iter().map(|id| SheetReference { title: id, link: None }).collect(),
    });
    let section = |label: &str, text: &str| (!text.is_empty()).then(|| SheetSection { label: label.into(), text: text.to_owned() });
    Some(DecisionSheet {
        id: format!("decision:{}", fields.id),
        status: status_word(&specs::frontmatter(text, "status").unwrap_or_default(), lang),
        mark: Some(mark(change, lang, true)),
        sections: [
            section(t!(lang, "Context", "Contexto"), &fields.context),
            section(t!(lang, "Decision", "Decisión"), &fields.decision),
            section(t!(lang, "Consequences", "Consecuencias"), &fields.consequences),
            section(t!(lang, "How it is checked", "Cómo se comprueba"), &fields.confirmation),
        ].into_iter().flatten().collect(),
        supersedes: references(t!(lang, "Replaces", "Sustituye a"), fields.supersedes.clone()),
        superseded_by: references(t!(lang, "Replaced by", "Sustituida por"), specs::frontmatter(text, "superseded_by").filter(|by| !by.is_empty()).into_iter().collect()),
        anchor: Some(SheetAnchor { id: format!("decision:{}", fields.id), quote: format!("{} · {}", fields.id, fields.title) }),
        edit: editing.filter(|_| after.is_some()).map(|editing| Link::View { key: format!("{}:decision:{}", editing.editor, specs::handle(path)) }),
        decision: fields.id,
        title: fields.title,
    })
}

/// The icon and words of a state, so none is told by color alone.
pub(super) fn line(icon: Icon, tone: super::Tone, text: impl Into<String>, detail: Option<String>) -> super::SheetLine {
    super::SheetLine { icon, tone, text: text.into(), detail }
}
