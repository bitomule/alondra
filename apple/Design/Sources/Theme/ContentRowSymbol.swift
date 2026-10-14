import AlondraContract

public extension ContentRow {
    /// The symbol of a card's line of content, the same on every Apple client.
    var symbol: String {
        switch kind {
        case "diff": "plusminus"
        case "command": "terminal"
        case "plan": "list.bullet.clipboard"
        case "document": "doc.richtext"
        case "report": "doc.text"
        case "steps": "checklist"
        case "options": "list.bullet"
        case "diagnosis": "stethoscope"
        case "attachments": "paperclip"
        default: "doc.text"
        }
    }
}
