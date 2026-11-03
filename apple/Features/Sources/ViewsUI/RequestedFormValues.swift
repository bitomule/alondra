import AlondraContract
import Foundation

public extension RequestedForm {
    /// The card's option that sends the form's values: the others answer without them.
    static let acceptOption = "accept"

    func isFilled(by action: WireAction) -> Bool { action.optionId == Self.acceptOption }

    /// What the form shows before the person touches it: each field's default, and a switch off
    /// when the server gives none, since that is what the person sees.
    var initialValues: [String: JSONValue] {
        fields.reduce(into: [:]) { values, field in
            switch field.kind {
            case let .text(_, _, _, value?): values[field.name] = .string(value)
            case let .number(integer, _, _, value?): values[field.name] = integer ? .integer(Int64(value)) : .number(value)
            case let .boolean(value): values[field.name] = .bool(value ?? false)
            case let .choice(_, value?): values[field.name] = .string(value)
            case let .choices(_, _, _, picked) where !picked.isEmpty: values[field.name] = .array(picked.map(JSONValue.string))
            default: break
            }
        }
    }

    /// The first field the values do not fill as the server asks, and what it lacks; nil when the
    /// form can go. The daemon checks it again.
    func problem(_ values: [String: JSONValue]) -> String? {
        for field in fields {
            guard let value = values[field.name] else {
                if field.required { return String(localized: "Fill in \(field.shownTitle)") }
                continue
            }
            if !field.kind.accepts(value) { return String(localized: "Check \(field.shownTitle): it is outside what it allows") }
        }
        return nil
    }
}

public extension RequestedField {
    var shownTitle: String { title ?? name }
}

public extension RequestedChoice {
    var shownTitle: String { title ?? value }
}

extension RequestedFieldKind {
    func accepts(_ value: JSONValue) -> Bool {
        func within(_ n: Double, _ least: Double?, _ most: Double?) -> Bool { n >= (least ?? -.infinity) && n <= (most ?? .infinity) }
        switch (self, value) {
        case let (.text(_, min, max, _), .string(text)): return within(Double(text.count), min.map { Double($0) }, max.map { Double($0) })
        case let (.number(_, min, max, _), .integer(n)): return within(Double(n), min, max)
        case let (.number(integer, min, max, _), .number(n)): return !integer && within(n, min, max)
        case (.boolean, .bool): return true
        case let (.choice(options, _), .string(picked)): return options.contains { $0.value == picked }
        case let (.choices(options, min, max, _), .array(picked)):
            let names = picked.compactMap { if case let .string(name) = $0 { name } else { nil } }
            return names.count == picked.count && Set(names).count == names.count
                && names.allSatisfy { name in options.contains { $0.value == name } }
                && within(Double(names.count), min.map { Double($0) }, max.map { Double($0) })
        default: return false
        }
    }
}
