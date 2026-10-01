import AlondraViews
import AlondraViewsUI
import AlondraViewsWorkspaceFeature
import ComposableArchitecture
import SwiftUI

struct ServiceScheduleEditor: View {
    @Bindable var store: StoreOf<ViewsScreenFeature>
    let form: WireForm
    @Environment(\.dismiss) private var dismiss

    private var editing: Bool { !store.key.rawValue.hasPrefix("schedule:new") }
    private var values: [String: JSONValue] { store.action.fields }
    private var canSave: Bool {
        !store.action.inFlight
            && !string("name").trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && !string("mission").trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    var body: some View {
        Form {
            missionSection
            whenSection
            launchOptionsSection
            advancedSection
            if let error = store.action.error { Text(error).foregroundStyle(.red) }
        }
        .navigationTitle(editing ? "Edit Schedule" : "New Schedule")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .confirmationAction) {
                Button("Save", systemImage: "checkmark") { store.send(.action(.submit)) }
                    .disabled(!canSave)
            }
        }
        .onChange(of: store.action.stamp) { old, new in
            if old == nil, new != nil { dismiss() }
        }
    }

    private var missionSection: some View {
        Section("Mission") {
            TextField("Name", text: stringBinding("name"))
            ZStack(alignment: .topLeading) {
                if string("mission").isEmpty {
                    Text("What should the node do?")
                        .foregroundStyle(.tertiary)
                        .padding(.top, 8).padding(.leading, 5)
                        .allowsHitTesting(false)
                }
                TextEditor(text: stringBinding("mission"))
                    .scrollContentBackground(.hidden)
                    .frame(height: 112)
                    .accessibilityLabel("Mission instructions")
            }
            if let project = field("project"), case let .choice(items) = project.kind {
                Picker("Project", selection: stringBinding("project")) {
                    ForEach(items, id: \.id) { item in Text(item.label).tag(item.id) }
                }
            }
        }
    }

    private var whenSection: some View {
        let visible = ScheduleEditorRules.visibleTimingFieldIDs(in: form, values: values)
        let mode = ScheduleEditorRules.repeatMode(string("repeat"))
        return Section("When") {
            Picker("Repeat", selection: repeatModeBinding) {
                Text("Every day").tag("daily")
                Text("Every week").tag("weekly")
                Text("Custom").tag("custom")
                Text("Once").tag("once")
            }
            if visible.contains("weekday"), let weekday = field("weekday"), case let .choice(items) = weekday.kind {
                Picker("Day", selection: integerBinding("weekday", fallback: 1)) {
                    ForEach(items, id: \.id) { item in
                        if let number = Int64(item.id) { Text(item.label).tag(number) }
                    }
                }
            }
            if mode == "custom" {
                Picker("Unit", selection: stringBinding("repeat")) {
                    Text("Hours").tag("every_hours")
                    Text("Days").tag("every_days")
                    Text("Months").tag("every_months")
                }
                if visible.contains("interval"), let interval = field("interval"), case let .stepper(minimum, maximum) = interval.kind {
                    Stepper(value: integerBinding("interval", fallback: minimum), in: minimum...maximum) {
                        LabeledContent("Every", value: "\(integerBinding("interval", fallback: minimum).wrappedValue)")
                    }
                }
            }
            if visible.contains("start_at") {
                DatePicker(mode == "once" ? "Date and time" : "Starts", selection: startDateBinding,
                           displayedComponents: [.date, .hourAndMinute])
            }
            if visible.contains("time") {
                DatePicker("Time", selection: timeBinding, displayedComponents: .hourAndMinute)
            }
            Text("Uses this device’s local time zone. Missed runs are sent once when Alondra starts again.")
                .font(.caption).foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            if field("active") != nil { Toggle("Active", isOn: boolBinding("active")) }
        }
    }

    @ViewBuilder private var launchOptionsSection: some View {
        let fields = ScheduleEditorRules.launchOptionFields(in: form)
        if !fields.isEmpty {
            Section("Launch options") {
                ForEach(fields, id: \.id) { option in
                    switch option.kind {
                    case .toggle: Toggle(option.label, isOn: boolBinding(option.id))
                    case let .choice(items):
                        Picker(option.label, selection: stringBinding(option.id)) {
                            ForEach(items, id: \.id) { item in Text(item.label).tag(item.id) }
                        }
                    default: EmptyView()
                    }
                }
            }
        }
    }

    @ViewBuilder private var advancedSection: some View {
        if field("disallowed_tools") != nil {
            Section("Advanced") {
                TextField("Disallowed tools (one pattern per line)", text: stringBinding("disallowed_tools"), axis: .vertical)
                    .lineLimit(2...6)
                Text("These tool patterns are passed to Claude as disallowed tools for every run.")
                    .font(.caption).foregroundStyle(.secondary)
            }
        }
    }

    private func field(_ id: String) -> Field? { form.fields.first { $0.id == id } }

    private func value(_ id: String) -> JSONValue { values[id] ?? field(id)?.value ?? .null }

    private func string(_ id: String) -> String {
        if case let .string(text) = value(id) { text } else { "" }
    }

    private func set(_ id: String, _ newValue: JSONValue) {
        var fields = values
        fields[id] = newValue
        store.send(.action(.fieldsChanged(fields)))
        if field(id)?.reloadOnChange == true { store.send(.reloadForm(id, newValue)) }
    }

    private func stringBinding(_ id: String) -> Binding<String> {
        Binding(get: { string(id) }, set: { set(id, .string($0)) })
    }

    private func boolBinding(_ id: String) -> Binding<Bool> {
        Binding(get: { if case let .bool(flag) = value(id) { flag } else { false } }, set: { set(id, .bool($0)) })
    }

    private func integerBinding(_ id: String, fallback: Int64) -> Binding<Int64> {
        Binding(get: { if case let .integer(number) = value(id) { number } else { fallback } },
                set: { set(id, .integer($0)) })
    }

    private var repeatModeBinding: Binding<String> {
        Binding(get: { ScheduleEditorRules.repeatMode(string("repeat")) }, set: { mode in
            set("repeat", .string(ScheduleEditorRules.repeatValue(forMode: mode)))
            if mode != "daily", mode != "weekly", case .null = value("start_at") {
                set("start_at", .integer(Int64((Date.now.timeIntervalSince1970 * 1000).rounded())))
            }
        })
    }

    private var startDateBinding: Binding<Date> {
        Binding(get: {
            if case let .integer(millis) = value("start_at") { Date(timeIntervalSince1970: Double(millis) / 1000) } else { .now }
        }, set: { set("start_at", .integer(Int64(($0.timeIntervalSince1970 * 1000).rounded()))) })
    }

    private var timeBinding: Binding<Date> {
        Binding(get: {
            let parts = string("time").split(separator: ":").compactMap { Int($0) }
            return Calendar.current.date(bySettingHour: parts.first ?? 9, minute: parts.dropFirst().first ?? 0, second: 0, of: .now) ?? .now
        }, set: {
            let parts = Calendar.current.dateComponents([.hour, .minute], from: $0)
            set("time", .string(String(format: "%02d:%02d", parts.hour ?? 9, parts.minute ?? 0)))
        })
    }
}
