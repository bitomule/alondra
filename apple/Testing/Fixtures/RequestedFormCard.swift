import AlondraContract
import Foundation

/// An MCP server's request for input as the daemon sends it, with a form of every kind of field.
enum RequestedFormCard {
    static func document(_ base: ViewDocument, spanish: Bool = false) throws -> ViewDocument {
        var object = try JSONSerialization.jsonObject(with: ContractCodec.encode(base)) as! [String: Any]
        var sections = object["sections"] as! [[String: Any]]
        let index = sections.firstIndex { ($0["items"] as? [[String: Any]])?.first?["type"] as? String == "card" }!
        var items = sections[index]["items"] as! [[String: Any]]
        var card = items[0]
        let need = spanish ? "Configura la sincronización del calendario" : "Set up the calendar sync"
        card["need"] = need; card["title"] = need
        card["why"] = spanish ? "El servidor MCP **calendar** pide estos datos para seguir con la tarea."
            : "The **calendar** MCP server asks for these details to go on with the task."
        card["subtitle"] = card["why"]
        card["eyebrow"] = ["text": spanish ? "Aportar" : "Provide", "tone": "accent"]
        if var actions = card["actions"] as? [[String: Any]] {
            for i in actions.indices {
                let accept = actions[i]["option_id"] as? String == "accept"
                actions[i]["label"] = spanish ? (accept ? "Aceptar" : "Rechazar") : (accept ? "Accept" : "Decline")
            }
            card["actions"] = actions
        }
        card.removeValue(forKey: "reply")
        card["form"] = ["fields": [
            ["name": "email", "title": spanish ? "Correo de la cuenta" : "Account email", "required": true,
             "kind": ["kind": "text", "format": "email"]],
            ["name": "days", "title": spanish ? "Días hacia atrás" : "Days to look back",
             "description": spanish ? "Cuántos días de eventos copia la primera vez." : "How many days of events it copies the first time.",
             "required": true, "kind": ["kind": "number", "integer": true, "minimum": 1, "maximum": 90, "default": 30]],
            ["name": "calendar", "title": spanish ? "Calendario" : "Calendar", "required": true,
             "kind": ["kind": "choice", "options": [["value": "work", "title": spanish ? "Trabajo" : "Work"],
                                                    ["value": "home", "title": spanish ? "Casa" : "Home"]], "default": "work"]],
            ["name": "include", "title": spanish ? "Qué copiar" : "What to copy",
             "kind": ["kind": "choices", "options": [["value": "events", "title": spanish ? "Eventos" : "Events"],
                                                     ["value": "reminders", "title": spanish ? "Recordatorios" : "Reminders"]],
                      "default": ["events"]]],
            ["name": "remember", "title": spanish ? "Recordar esta cuenta" : "Remember this account", "kind": ["kind": "boolean", "default": true]],
        ]]
        items[0] = card
        sections[index]["items"] = items
        object["sections"] = sections
        return try ContractCodec.decode(ViewDocument.self, from: JSONSerialization.data(withJSONObject: object))
    }
}
