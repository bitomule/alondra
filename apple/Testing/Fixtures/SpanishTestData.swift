import AlondraContract
import Foundation
import XCTest

/// What a Spanish viewer reads on a card: the daemon words its own labels in Spanish (the `-es`
/// goldens), and the nodes write in the person's language. The fixtures hold English, so a Spanish
/// capture swaps both in, and never shows an English kind, label or sentence. Shared by the iPhone
/// and Mac captures.
enum SpanishTestData {
    static func document(_ document: ViewDocument) throws -> ViewDocument {
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: JSONEncoder().encode(document)))
        return try ContractCodec.decode(ViewDocument.self, from: JSONSerialization.data(withJSONObject: translated(object)))
    }

    private static func translated(_ value: Any, key: String? = nil) -> Any {
        switch value {
        case let object as [String: Any]:
            return Dictionary(uniqueKeysWithValues: object.map { ($0.key, translated($0.value, key: $0.key)) })
        case let array as [Any]:
            return array.map { translated($0, key: key) }
        case let text as String where !Self.untranslated.contains(key ?? ""):
            if let word = Self.spanishWords[text] { return word }
            let head = String(text.prefix { $0 != " " })
            if key == "text", let kind = Self.spanishKinds[head] { return kind + text.dropFirst(head.count) }
            return Self.spanishSentences.reduce(text) { $0.replacingOccurrences(of: $1.0, with: $1.1) }
        default:
            return value
        }
    }

    private static let untranslated: Set<String> = [
        "id", "key", "kind", "icon", "type", "tone", "style", "role", "presentation", "fetch", "digest", "url", "view",
        "client", "version", "cursor", "data_epoch", "option_id", "action_id", "name", "anchor",
    ]

    private static let spanishKinds = [
        "Decide": "Decidir", "Review": "Revisar", "Authorize": "Autorizar", "FYI": "Info", "Provide": "Aportar",
        "Test": "Probar", "Do": "Hacer", "Problem": "Problema", "Proposal": "Propuesta",
    ]

    /// Whole labels the daemon words in Spanish, for cards built from the English goldens.
    private static let spanishWords = [
        "Reply": "Responder", "Sent": "Enviado", "Answered": "Respondida", "Content": "Contenido", "Context": "Contexto",
        "Task": "Tarea", "Seen": "Visto", "Read": "Leída", "Report": "Informe", "3 points": "3 puntos",
        "Changes": "Cambios", "Diff": "Diferencias", "Description": "Descripción", "Request changes": "Pedir cambios",
        "Describe the changes": "Describe los cambios", "Changes requested": "Cambios solicitados",
        "Approve": "Aprobar", "Approved": "Aprobado", "6 steps": "6 pasos", "Command": "Comando", "Yes": "Sí",
        "Allow always": "Permitir siempre", "Choose an option": "Elige una opción",
        "Allows this now and stops asking in `alondra` for:\n· `make build` with any arguments: runs what the repo's `Makefile` says, even if the node changes it\n`tail` already didn't ask. You can remove it in Settings › Permissions.":
            "Permite esto ahora y en `alondra` deja de preguntar por:\n· `make build` con cualquier argumento: ejecuta lo que diga el `Makefile` del repo, también si el nodo lo cambia\n`tail` ya no preguntaba. Puedes quitarlo en Ajustes › Permisos.", "Blocks release": "Bloquea la publicación", "Retry now": "Reintentar ahora",
        "Skip this run": "Saltar esta ejecución", "Diagnosis": "Diagnóstico", "folder not found": "carpeta no encontrada",
        "Blocks the schedule": "Bloquea la programación", "Why each option": "Por qué cada opción",
        "2 options": "2 opciones", "5 options": "5 opciones", "Write my own answer": "Escribir mi respuesta",
        "Answer in your own words…": "Responde con tus palabras…", "Request clarification": "Pedir aclaración",
        "It is unclear": "No se entiende", "It contradicts something": "Se contradice", "It lacks context": "Le falta contexto",
        "What should it clarify?": "¿Qué debe aclarar?", "Clarification requested": "Aclaración solicitada",
        "Waiting for an answer": "Esperando respuesta", "Doesn't block": "No bloquea",
        "Reply to this node…": "Responder a este nodo…", "Reply to Alondra · iOS…": "Responder a Alondra · iOS…",
        "Blocks Alondra · iOS": "Bloquea a Alondra · iOS", "Pick the release channel": "Elige el canal de publicación",
        "Five channels are open and one ships first.": "Hay cinco canales abiertos y uno sale primero.",
        "Repository": "Repositorio", "Head": "Rama", "4 files · +120 −30": "4 archivos · +120 −30",
        "Merge": "Hacer merge", "Merge commit": "Merge commit",
        "Merging…": "Haciendo merge…", "Open on GitHub": "Abrir en GitHub", "Accept without merging": "Aceptar sin hacer merge",
        "Accepted": "Aceptado",
        "Answer “Ship the compact schedule editor with this beta?” first":
            "Responde primero a «¿Publicar el editor compacto con esta beta?»",
    ]

    /// What the nodes wrote, as they would write it to a Spanish speaker.
    private static let spanishSentences: [(String, String)] = [
        ("Concise: a short title and one explanatory sentence.\nWarm: the more conversational explanation.",
         "Conciso: un título corto y una frase que lo explique.\nCercano: la explicación más conversacional."),
        ("Replaces the long schedule form with a compact editor.\n\n- Repeat and time on one row\n- Launch options in a sheet",
         "Sustituye el formulario largo de programaciones por un editor compacto.\n\n- Repetición y hora en una fila\n- Opciones de lanzamiento en una hoja"),
        ("- Swipe navigation\n- Inline replies\n- Attachment previews",
         "- Navegación con gestos\n- Respuestas en línea\n- Vista previa de adjuntos"),
        ("The review of the pull request waits for your answer.", "La revisión del pull request espera tu respuesta."),
        ("Polish the Alondra beta", "Pulir la beta de Alondra"),
        ("The new empty state is ready; the beta ships with the copy you pick.",
         "El nuevo estado vacío está listo; la beta sale con el texto que elijas."),
        ("Swipe navigation, inline replies and attachment previews are ready for review.",
         "La navegación con gestos, las respuestas en línea y la vista previa de adjuntos están listas para revisar."),
        ("All required checks passed and the branch is up to date with main.",
         "Pasaron todas las comprobaciones obligatorias y la rama está al día con main."),
        ("It restarts the daemon for a few seconds. You can go back to 0.2.35.",
         "Reinicia el daemon unos segundos. Puedes volver a la 0.2.35."),
        ("Two files change and nothing starts until you approve.", "Cambian dos archivos y nada empieza hasta que lo apruebes."),
        ("Release at 09:00 could not start: the project folder is missing.",
         "La publicación de las 09:00 no pudo empezar: falta la carpeta del proyecto."),
        ("The project folder `~/Projects/release` does not exist on this Mac.",
         "La carpeta `~/Projects/release` no existe en este Mac."),
        ("Use a short title and one explanatory sentence.", "Un título corto y una frase que lo explique."),
        ("Preserve the more conversational explanation.", "Conservar la explicación más conversacional."),
        ("Restore the native review experience", "Recuperar la revisión nativa"),
        ("Approve the compact schedule plan", "Aprueba el plan del editor compacto"),
        ("Retry the failed scheduled launch", "Reintenta el lanzamiento programado que falló"),
        ("Add the compact schedule editor", "Añadir el editor compacto de programaciones"),
        ("The simulator pass is clean", "La pasada en el simulador está limpia"),
        ("Allow publishing Brew 0.2.36", "Permitir publicar Brew 0.2.36"),
        ("Review the beta candidate", "Revisa la candidata de la beta"),
        ("Choose the welcome copy", "Elige el texto de bienvenida"),
        ("Ship the concise copy", "Publicar el texto conciso"),
        ("Keep the warmer copy", "Mantener el texto más cercano"),
    ]
}
