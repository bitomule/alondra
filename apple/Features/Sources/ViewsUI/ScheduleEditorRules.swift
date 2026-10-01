import AlondraContract

public enum ScheduleEditorRules {
    public static let missionFieldIDs: Set<String> = ["name", "mission", "project"]
    public static let timingFieldIDs: Set<String> = ["repeat", "weekday", "interval", "start_at", "time", "active"]
    public static let advancedFieldIDs: Set<String> = ["disallowed_tools"]

    public static func repeatMode(_ repeatValue: String) -> String {
        repeatValue.hasPrefix("every_") ? "custom" : repeatValue
    }

    public static func repeatValue(forMode mode: String) -> String {
        mode == "custom" ? "every_hours" : mode
    }

    public static func launchOptionFields(in form: WireForm) -> [Field] {
        let known = missionFieldIDs.union(timingFieldIDs).union(advancedFieldIDs)
        return form.fields.filter { !known.contains($0.id) }
    }

    public static func isVisible(_ field: Field, values: [String: JSONValue], in form: WireForm) -> Bool {
        guard let condition = field.visibleWhen else { return true }
        guard let value = values[condition.field] ?? form.fields.first(where: { $0.id == condition.field })?.value else {
            return false
        }
        return condition.matches(value)
    }

    public static func visibleTimingFieldIDs(in form: WireForm, values: [String: JSONValue]) -> [String] {
        form.fields.filter { timingFieldIDs.contains($0.id) && isVisible($0, values: values, in: form) }.map(\.id)
    }
}
