//! The words of every refusal, in one catalog whose `match` has no fallback: a variant without
//! text in both languages does not compile.
use crate::text::{t, t as tr, tf, Lang};
use alondra_domain_rules::{MergeMethod, Refusal};

fn methods(methods: &[MergeMethod]) -> String {
    methods
        .iter()
        .map(|method| match method {
            MergeMethod::Merge => "merge",
            MergeMethod::Squash => "squash",
            MergeMethod::Rebase => "rebase",
        })
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn refusal_text(refusal: &Refusal, lang: Lang) -> String {
    match refusal {
        Refusal::BlockedByQuestion { need } => tf!(lang, "Answer “{need}” first; this result waits for it.",
            "Responde primero a «{need}»; este resultado lo espera.", need = need),
        Refusal::BlockedByTask { title } => tf!(lang, "“{title}” has to finish reviewing this first.",
            "«{title}» tiene que terminar de revisarlo primero.", title = title),
        Refusal::WaitsForTask { title } => tf!(lang, "This task waits for “{title}”: its plan can be approved once that wait ends.",
            "Esta tarea espera a «{title}»: su plan se podrá aprobar cuando termine esa espera.", title = title),
        Refusal::UpstreamPrOpen { number } => tf!(lang, "PR #{number} must merge first: this one builds on it.",
            "Antes hay que hacer merge de la PR #{number}: esta se apoya en ella.", number = number),
        Refusal::MethodNotAllowed { allowed, .. } if allowed.is_empty() => t!(lang,
            "Alondra could not read how this repository allows merging. Try again in a moment.",
            "Alondra no pudo leer qué tipos de merge permite este repositorio. Inténtalo de nuevo en un momento.").into(),
        Refusal::MethodNotAllowed { allowed, .. } => tf!(lang, "This PR only allows: {allowed}.",
            "Esta PR solo admite: {allowed}.", allowed = methods(allowed)),
        Refusal::NoMethodChosen { number } => tf!(lang, "Choose how to merge PR #{number}.",
            "Elige cómo hacer merge de la PR #{number}.", number = number),
        Refusal::StaleHead { number } => tf!(lang, "PR #{number} has new commits since you looked. Review them and try again.",
            "La PR #{number} tiene commits nuevos desde que la miraste. Revísalos y vuelve a intentarlo.", number = number),
        Refusal::ResultBehindPr { number } => tf!(lang, "PR #{number} has commits this result doesn't include, so it can't be merged from here until the node presents it again.",
            "La PR #{number} tiene commits que este resultado no incluye: no se puede hacer merge desde aquí hasta que el nodo la vuelva a presentar.", number = number),
        Refusal::MergeInProgress { number } => tf!(lang, "Alondra is already merging PR #{number}; wait for GitHub to confirm.",
            "Alondra ya está haciendo merge de la PR #{number}; espera a que GitHub lo confirme.", number = number),
        Refusal::PrClosed { number } => tf!(lang, "PR #{number} is closed on GitHub, so it can't be merged.",
            "La PR #{number} está cerrada en GitHub y no se puede hacer merge.", number = number),
        Refusal::PrAlreadyMerged { number } => tf!(lang, "PR #{number} was already merged, so it can't be closed without merging.",
            "La PR #{number} ya estaba mergeada, así que no se puede cerrar sin hacer merge.", number = number),
        Refusal::PrSuperseded => t!(lang, "This card was another copy of the same pull request; use the one that remains",
            "Esta tarjeta era otra copia del mismo PR; usa la que queda").into(),
        Refusal::PrDetached => t!(lang, "This pull request is no longer in Alondra.", "Este PR ya no está en Alondra.").into(),
        Refusal::PrUncheckable => t!(lang, "Alondra can't check which pull request this is, so it does nothing on GitHub. Remove it from Alondra instead.",
            "Alondra no puede comprobar qué PR es, así que no hace nada en GitHub. Quítalo de Alondra.").into(),
        Refusal::UserMessageUnavailable => tr!(lang, "This node cannot confirm reading messages. Nothing was sent; send the work as a new request.",
            "Este nodo no puede confirmar la lectura de mensajes. No se envió nada; envía el encargo como una solicitud nueva.").into(),
        Refusal::PendingUserMessage => tr!(lang, "A message still needs a reply: wait for a new result.",
            "Queda un mensaje por responder: espera un nuevo resultado.").into(),
        Refusal::UnansweredMessage => tr!(lang, "This task got a message from you after opening the PR and is still working on it. Press merge again to confirm, or wait for its answer.",
            "Esta tarea recibió un mensaje tuyo después de abrir el PR y aún lo está atendiendo. Pulsa merge otra vez para confirmarlo, o espera su respuesta.").into(),
        Refusal::NewerResult => t!(lang, "There is a newer result: review it.",
            "Hay un resultado más nuevo: revísalo.").into(),
        Refusal::AlreadyAnswered => t!(lang, "You already answered this.", "Ya respondiste a esto.").into(),
        Refusal::Withdrawn => t!(lang, "Its author withdrew this card.", "Su autor retiró esta tarjeta.").into(),
        Refusal::ClarificationPending => t!(lang, "Waiting for the clarification you asked for.",
            "Esperando la aclaración que pediste.").into(),
        Refusal::TaskNotActive => t!(lang, "This task has finished or was cancelled.", "Esta tarea ya terminó o se canceló.").into(),
        Refusal::TaskClosing => t!(lang, "This task's PR is already merged and the task is closing, so it takes no more changes. Ask for more work as a new request.",
            "La PR de esta tarea ya está mergeada y la tarea se está cerrando, así que no admite más cambios. Pide más trabajo como una solicitud nueva.").into(),
        Refusal::Changed => t!(lang, "This changed while you were looking. It's updated now; try again.",
            "Esto cambió mientras lo mirabas. Ya está actualizado; vuelve a intentarlo.").into(),
        Refusal::NotForYou => t!(lang, "This is meant for the orchestrator or a node, not for you.",
            "Esto es para el orquestador o un nodo, no para ti.").into(),
        Refusal::NodeUnreachable { node } => tf!(lang, "“{node}” isn't connected: its session stopped. Nothing was sent.",
            "«{node}» no está conectado: su sesión se detuvo. No se envió nada.", node = node),
        Refusal::NothingToInterrupt => t!(lang, "This node isn't running anything to interrupt.",
            "Este nodo no está haciendo nada que interrumpir.").into(),
        Refusal::InterruptNotAccepted => t!(lang, "Interrupt it first: you can force it to stop only if it keeps going after Claude accepts the interrupt.",
            "Interrúmpelo primero: solo puedes forzar la parada si sigue después de que Claude acepte la interrupción.").into(),
        Refusal::DefaultAccountStays { name } => tf!(lang, "{name} is Claude's own sign-in on this Mac: it can't be removed.",
            "{name} es la sesión propia de Claude en este Mac: no se puede quitar.", name = name),
        Refusal::LastAccountOn { name } => tf!(lang, "{name} is the only account on: turn on another one first.",
            "{name} es la única cuenta activada: activa otra antes.", name = name),
        Refusal::AccountInUse { name } => tf!(lang, "Open tasks still run on {name}: remove it once they are done.",
            "Aún hay tareas abiertas que trabajan con {name}: quítala cuando terminen.", name = name),
        Refusal::AlreadyStopping => t!(lang, "Alondra is already stopping this node.", "Alondra ya está parando este nodo.").into(),
        Refusal::DeviceNotApproved => t!(lang, "This device is no longer approved. Approve it again from the Mac.",
            "Este dispositivo ya no está aprobado. Vuelve a aprobarlo desde el Mac.").into(),
        Refusal::StorageUnavailable => t!(lang, "Alondra couldn't save this. Nothing changed; try again.",
            "Alondra no pudo guardarlo. No cambió nada; inténtalo de nuevo.").into(),
        Refusal::GithubUnreachable => t!(lang, "Alondra couldn't reach GitHub. Nothing was changed; try again.",
            "Alondra no pudo contactar con GitHub. No cambió nada; inténtalo de nuevo.").into(),
        Refusal::NoLongerAvailable => t!(lang, "This is no longer available.", "Esto ya no está disponible.").into(),
        Refusal::Unchanged => t!(lang, "It was already like that: nothing changed.", "Ya estaba así: no cambió nada.").into(),
    }
}
