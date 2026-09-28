import AlondraContracts
import Foundation

/// Client-side checks protect a useful editor; the server still decides whether a draft is valid.
public enum ScheduleDraftRules {
    public static func canSave(_ draft: ScheduledTaskDraft) -> Bool {
        !draft.title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty &&
        !draft.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty &&
        (0...23).contains(draft.hour) && (0...59).contains(draft.minute) &&
        (draft.frequency != "weekly" || draft.weekday.map { (0...6).contains($0) } == true) &&
        (!draft.frequency.hasPrefix("every_") || draft.interval.map { (1...365).contains($0) } == true)
    }

    /// Without an options catalog nothing can be checked here, so every stored choice is kept
    /// for the server to validate rather than silently dropped.
    public static func validOverrides(_ choices: [String: OptionChoice], in specs: [OptionSpec]) -> [String: OptionChoice] {
        guard !specs.isEmpty else { return choices }
        return choices.filter { id, choice in
            guard let spec = specs.first(where: { $0.id == id }), choice != spec.default else { return false }
            switch (spec.kind, choice) {
            case (.toggle, .toggle): return true
            case (.choice, .value(let value)): return spec.values.contains { $0.id == value }
            default: return false
            }
        }
    }
}
