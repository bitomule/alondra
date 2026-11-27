import AlondraContract
import AlondraDesign
import AlondraFeed
import Foundation
import SwiftUI

/// The edits of specs the person made that the Mac has not confirmed. A screen draws its sheets
/// through them, so an edit shows at once, marked as saving; one the Mac refuses goes back to what it
/// was, and its form opens again with the reason beside the field it names.
public struct SpecEdits: Equatable, Sendable {
    public enum Operation: Equatable, Sendable {
        case scenario(String), newScenario(String), removeScenario(String)
        case spec(String), removeSpec(String)
        case decision(String), removeDecision(String)
        case diagram(String), removePart(String)
        case other(String)

        var target: String {
            switch self {
            case let .scenario(key), let .newScenario(key), let .removeScenario(key), let .spec(key), let .removeSpec(key),
                 let .decision(key), let .removeDecision(key), let .diagram(key), let .removePart(key), let .other(key): key
            }
        }
    }

    public struct Edit: Equatable, Sendable, Identifiable {
        public let id: String
        public let operation: Operation
        public let fields: [String: JSONValue]
        public let form: WireForm?
        public fileprivate(set) var phase: Phase
    }

    public enum Phase: Equatable, Sendable {
        case sending, overdue
        case applied(shown: String?)
        case refused
    }

    public private(set) var edits: [Edit] = []

    public init() {}

    public var isEmpty: Bool { edits.isEmpty }

    /// Whether a link opens the form that edits a part of a plan's specs, which opens over the list of them.
    public static func isEditorForm(_ key: String) -> Bool {
        key.hasPrefix("task_specs:") && key.split(separator: ":").count > 2
    }

    public static func handles(_ request: ActionRequest) -> Bool { request.actionId.hasPrefix("spec_edit:") }

    public func isPending(_ request: ActionRequest) -> Bool { edits.contains { $0.id == request.deliveryCorrelationID } }

    /// Starts showing what `request` edits, from the form it was sent from.
    public mutating func begin(_ request: ActionRequest, form: WireForm?) {
        guard Self.handles(request), let operation = Self.operation(of: request.actionId) else { return }
        edits.removeAll { $0.id == request.deliveryCorrelationID }
        edits.append(Edit(id: request.deliveryCorrelationID, operation: operation, fields: request.input.fields ?? [:], form: form, phase: .sending))
    }

    /// The Mac answered `request`: an applied edit stays until a newer view arrives; a refused one
    /// stops showing and waits for its reason.
    public mutating func settle(_ request: ActionRequest, applied: Bool, shown: String?) {
        guard let index = edits.firstIndex(where: { $0.id == request.deliveryCorrelationID }) else { return }
        edits[index].phase = applied ? .applied(shown: shown) : .refused
    }

    /// The refused edit `request` made, taken out so its form can open again.
    public mutating func refused(_ request: ActionRequest) -> Edit? {
        guard let index = edits.firstIndex(where: { $0.id == request.deliveryCorrelationID && $0.phase == .refused }) else { return nil }
        return edits.remove(at: index)
    }

    public mutating func forget(_ request: ActionRequest) {
        edits.removeAll { $0.id == request.deliveryCorrelationID }
    }

    /// The Mac has not confirmed the edit in time: it keeps retrying and says so.
    @discardableResult
    public mutating func expire(_ id: String) -> Bool {
        guard let index = edits.firstIndex(where: { $0.id == id && $0.phase == .sending }) else { return false }
        edits[index].phase = .overdue
        return true
    }

    /// A view arrived: whatever it shows of an applied edit is the Mac's word from now on.
    public mutating func reconcile(_ version: String?) {
        edits.removeAll { if case let .applied(shown) = $0.phase { shown != version || version == nil } else { false } }
    }

    /// The parts being saved, by the link that edits each.
    public var saving: [String: SheetSaving] {
        var saving: [String: SheetSaving] = [:]
        for edit in edits {
            switch edit.phase {
            case .sending: saving[edit.operation.target] = .saving
            case .overdue: saving[edit.operation.target] = .overdue
            case .applied, .refused: break
            }
        }
        return saving
    }

    private var shown: [Edit] { edits.filter { $0.phase != .refused } }

    /// `<verb>:<task>@<revision>~<extra>` as the daemon names an edit of a plan's specs.
    static func operation(of actionId: String) -> Operation? {
        guard let head = actionId.firstIndex(of: ":"), let at = actionId.firstIndex(of: "@"), let tilde = actionId.firstIndex(of: "~"),
              head < at, at < tilde else { return nil }
        let task = String(actionId[actionId.index(after: head)..<at])
        let extra = String(actionId[actionId.index(after: tilde)...])
        let base = "task_specs:\(task)"
        let parts = extra.split(separator: "-", maxSplits: 1).map(String.init)
        guard let verb = parts.first else { return nil }
        let rest = parts.count > 1 ? parts[1] : ""
        switch verb {
        case "sc" where rest.hasPrefix("new-"): return .newScenario("\(base):spec:\(rest.dropFirst(4))")
        case "sc": return .scenario("\(base):scenario:\(rest)")
        case "rmsc": return .removeScenario("\(base):scenario:\(rest)")
        case "spec" where rest != "new": return .spec("\(base):spec:\(rest)")
        case "rmspec": return .removeSpec("\(base):spec:\(rest)")
        case "dec" where rest != "new": return .decision("\(base):decision:\(rest)")
        case "rmdec": return .removeDecision("\(base):decision:\(rest)")
        case "part", "rmpart":
            let pieces = rest.split(separator: "-").map(String.init)
            guard pieces.count == 3 else { return .other(base) }
            let key = "\(base):part:\(pieces[0]):\(pieces[1]):\(pieces[2])"
            if verb == "rmpart" { return .removePart(key) }
            return pieces[0] == "diagram" ? .diagram(key) : .other(key)
        default: return .other(base)
        }
    }
}

public extension SpecEdits {
    /// The sheet as it will be once the Mac confirms what was sent; nil when an edit removes it.
    func patched(_ sheet: SpecSheet) -> SpecSheet? {
        var sheet = sheet
        for edit in shown {
            switch edit.operation {
            case let .removeSpec(key) where sheet.edit == .view(key: key): return nil
            case let .spec(key) where sheet.edit == .view(key: key):
                if let title = edit.fields["title"]?.sentText, !title.isEmpty { sheet.title = title }
                if let explanation = edit.fields["explanation"]?.sentText { sheet.explanation = explanation.split(separator: "\n").joined(separator: " ") }
                if let spec = edit.fields["id"]?.sentText, !spec.isEmpty { sheet.spec = spec }
            case let .newScenario(key) where sheet.edit == .view(key: key):
                sheet.scenarios.append(Self.scenario(from: edit, existing: nil, in: sheet))
            default: break
            }
        }
        sheet.scenarios = sheet.scenarios.compactMap { patched($0, in: sheet) }
        sheet.parts = sheet.parts.compactMap { patched($0) }
        if var unchanged = sheet.unchanged {
            unchanged.scenarios = unchanged.scenarios.compactMap { patched($0, in: sheet) }
            sheet.unchanged = unchanged
        }
        return sheet
    }

    func patched(_ sheet: DecisionSheet) -> DecisionSheet? {
        var sheet = sheet
        for edit in shown {
            switch edit.operation {
            case let .removeDecision(key) where sheet.edit == .view(key: key): return nil
            case let .decision(key) where sheet.edit == .view(key: key):
                if let title = edit.fields["title"]?.sentText, !title.isEmpty { sheet.title = title }
                if let id = edit.fields["id"]?.sentText, !id.isEmpty { sheet.decision = id }
                let order = ["context", "decision", "consequences", "confirmation"]
                if sheet.sections.count == order.count {
                    for (index, field) in order.enumerated() {
                        if let text = edit.fields[field]?.sentText { sheet.sections[index].text = text }
                    }
                }
            default: break
            }
        }
        return sheet
    }

    private func patched(_ scenario: SheetScenario, in sheet: SpecSheet) -> SheetScenario? {
        var scenario = scenario
        scenario.parts = scenario.parts.compactMap { patched($0) }
        for edit in shown {
            switch edit.operation {
            case let .removeScenario(key) where scenario.edit == .view(key: key): return nil
            case let .scenario(key) where scenario.edit == .view(key: key):
                scenario = Self.scenario(from: edit, existing: scenario, in: sheet)
            default: break
            }
        }
        return scenario
    }

    private func patched(_ part: SheetPart) -> SheetPart? {
        var part = part
        for edit in shown {
            switch edit.operation {
            case let .removePart(key) where part.edit == .view(key: key): return nil
            case let .diagram(key) where part.edit == .view(key: key):
                if let code = edit.fields["code"]?.sentText { part.code = code }
            default: break
            }
        }
        return part
    }

    private static func scenario(from edit: Edit, existing: SheetScenario?, in sheet: SpecSheet) -> SheetScenario {
        let known = (existing.map { [$0] } ?? sheet.scenarios).flatMap(\.lanes)
        let draft = edit.fields["steps"]?.sentText.flatMap { StepsDraft(parsing: $0) }
        let lanes = draft?.lanes(labelled: known) ?? existing?.lanes ?? []
        let examples = draft.flatMap { $0.examples.isEmpty ? nil : SheetTable(label: existing?.examples?.label ?? StepsDraft.examplesLabel, rows: $0.examples) }
        let title = edit.fields["title"]?.sentText ?? existing?.title ?? ""
        guard var scenario = existing else {
            return SheetScenario(id: edit.id, title: title, kind: sheet.scenarios.first?.kind ?? "", lanes: lanes, examples: examples)
        }
        scenario.title = title
        scenario.lanes = lanes
        scenario.examples = draft == nil ? scenario.examples : examples
        return scenario
    }
}

public extension EnvironmentValues {
    @Entry var specEdits = SpecEdits()
}

private extension JSONValue {
    var sentText: String? { if case let .string(text) = self { text } else { nil } }
}
