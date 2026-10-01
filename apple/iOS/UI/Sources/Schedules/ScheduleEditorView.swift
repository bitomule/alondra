import AlondraContract
import AlondraDesign
import AlondraFeatures
import ComposableArchitecture
import SwiftUI

/// The form for one scheduled task: what to do, when to repeat it, how to launch it.
public struct ScheduleEditorView: View {
    @Bindable var store: StoreOf<ScheduleEditorFeature>

    public init(store: StoreOf<ScheduleEditorFeature>) { self.store = store }

    @Environment(\.calendar) private var calendar

    private var state: ScheduleEditorFeature.State { store.state }

    public var body: some View {
        Group {
            if store.form != nil {
                form
            } else {
                ScrollView {
                    ScreenPhaseView(phase: store.screen.phase, missing: "This schedule is gone") { store.send(.task) }
                }
            }
        }
        .background(DawnBackground())
        .navigationTitle(store.isNew ? Text("New Schedule") : Text(verbatim: store.title ?? ""))
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .confirmationAction) {
                Button("Save", systemImage: "checkmark") { store.send(.save) }
                    .disabled(!store.canSave)
            }
        }
        .task { await store.send(.task).finish() }
        .actionDialogs(store.scope(state: \.actions, action: \.actions))
        .sensoryFeedback(.success, trigger: store.actions.stamp)
    }

    private var form: some View {
        Form {
            notices
            missionSection
            whenSection
            launchOptionsSection
            advancedSection
            if let error = store.formError {
                Section { Label(error, systemImage: "exclamationmark.triangle").foregroundStyle(Palette.rose) }
            }
        }
        .scrollContentBackground(.hidden)
    }

    @ViewBuilder private var notices: some View {
        if !store.notices.isEmpty {
            Section {
                ForEach(store.notices, id: \.text) { notice in
                    Label(notice.text, systemImage: notice.tone == .warning ? "exclamationmark.triangle" : "clock.arrow.circlepath")
                        .font(.subheadline)
                        .foregroundStyle(TaskStyle.tint(tone: notice.tone))
                }
            }
        }
    }

    private var missionSection: some View {
        Section("Mission") {
            VStack(alignment: .leading, spacing: 4) {
                TextField(label("name", "Name"), text: textBinding("name"))
                    .accessibilityLabel(label("name", "Name"))
                fieldError("name")
            }
            VStack(alignment: .leading, spacing: 4) {
                ZStack(alignment: .topLeading) {
                    if state.text("mission").isEmpty {
                        Text("What should the node do?")
                            .foregroundStyle(.tertiary)
                            .padding(.top, 8).padding(.leading, 5)
                            .allowsHitTesting(false)
                    }
                    TextEditor(text: textBinding("mission"))
                        .scrollContentBackground(.hidden)
                        .frame(height: 112)
                        .accessibilityLabel(label("mission", "Mission"))
                }
                fieldError("mission")
            }
            if let project = state.field("project"), case .choice(let items) = project.kind {
                Picker(project.label, selection: textBinding("project")) {
                    ForEach(items, id: \.id) { item in Text(verbatim: item.label).tag(item.id) }
                }
            }
        }
    }

    private var whenSection: some View {
        Section("When") {
            if let repeatField = state.field("repeat"), case .choice(let items) = repeatField.kind {
                Picker(repeatField.label, selection: textBinding("repeat")) {
                    ForEach(items, id: \.id) { item in Text(verbatim: item.label).tag(item.id) }
                }
            }
            if let weekday = visible("weekday"), case .choice(let items) = weekday.kind {
                VStack(alignment: .leading, spacing: 4) {
                    Picker(weekday.label, selection: integerBinding("weekday", fallback: 1)) {
                        ForEach(items, id: \.id) { item in
                            if let number = Int64(item.id) { Text(verbatim: item.label).tag(number) }
                        }
                    }
                    fieldError("weekday")
                }
            }
            if let interval = visible("interval"), case .stepper(let minimum, let maximum) = interval.kind {
                VStack(alignment: .leading, spacing: 4) {
                    Stepper(value: integerBinding("interval", fallback: minimum), in: minimum...maximum) {
                        LabeledContent(interval.label, value: "\(integerBinding("interval", fallback: minimum).wrappedValue)")
                    }
                    fieldError("interval")
                }
            }
            if let start = visible("start_at") {
                VStack(alignment: .leading, spacing: 4) {
                    DatePicker(start.label, selection: startBinding, displayedComponents: [.date, .hourAndMinute])
                    fieldError("start_at")
                }
            }
            if let time = visible("time") {
                VStack(alignment: .leading, spacing: 4) {
                    DatePicker(time.label, selection: timeBinding, displayedComponents: .hourAndMinute)
                    fieldError("time")
                }
            }
            Text("Uses this device’s local time zone. Missed runs are sent once when Alondra starts again.")
                .font(.caption).foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            if let active = state.field("active") {
                Toggle(active.label, isOn: boolBinding("active"))
            }
        }
    }

    @ViewBuilder private var launchOptionsSection: some View {
        let fields = store.form.map(ScheduleEditorRules.launchOptionFields) ?? []
        if !fields.isEmpty {
            Section("Launch options") {
                ForEach(fields, id: \.id) { option in
                    switch option.kind {
                    case .toggle:
                        Toggle(option.label, isOn: boolBinding(option.id))
                    case .choice(let items):
                        Picker(option.label, selection: textBinding(option.id)) {
                            ForEach(items, id: \.id) { item in
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(verbatim: item.label)
                                    if let description = item.description {
                                        Text(verbatim: description)
                                            .font(.caption)
                                            .foregroundStyle(.secondary)
                                    }
                                }
                                .tag(item.id)
                            }
                        }
                        .pickerStyle(.navigationLink)
                    default:
                        EmptyView()
                    }
                }
            }
        }
    }

    @ViewBuilder private var advancedSection: some View {
        if let tools = state.field("disallowed_tools") {
            Section("Advanced") {
                VStack(alignment: .leading, spacing: 4) {
                    TextField(tools.label, text: textBinding("disallowed_tools"), prompt: Text("One pattern per line"), axis: .vertical)
                        .lineLimit(2...6)
                        .accessibilityLabel(tools.label)
                    fieldError("disallowed_tools")
                }
                Text("These tool patterns are passed to Claude as disallowed tools for every run.")
                    .font(.caption).foregroundStyle(.secondary)
            }
        }
    }

    @ViewBuilder private func fieldError(_ id: String) -> some View {
        if let message = store.errors[id] {
            Label(message, systemImage: "exclamationmark.circle.fill")
                .font(.footnote)
                .foregroundStyle(Palette.rose)
                .accessibilityIdentifier("scheduleEditor.error.\(id)")
        }
    }

    private func label(_ id: String, _ fallback: String) -> String {
        state.field(id)?.label ?? fallback
    }

    private func visible(_ id: String) -> Field? {
        state.field(id).flatMap { state.isVisible($0) ? $0 : nil }
    }

    private func textBinding(_ id: String) -> Binding<String> {
        Binding(get: { state.text(id) }, set: { store.send(.set(id, .string($0))) })
    }

    private func boolBinding(_ id: String) -> Binding<Bool> {
        Binding(
            get: { if case .bool(let flag) = state.value(id) { flag } else { false } },
            set: { store.send(.set(id, .bool($0))) }
        )
    }

    private func integerBinding(_ id: String, fallback: Int64) -> Binding<Int64> {
        Binding(
            get: {
                switch state.value(id) {
                case .integer(let number): number
                case .string(let text): Int64(text) ?? fallback
                default: fallback
                }
            },
            set: { store.send(.set(id, .integer($0))) }
        )
    }

    private var startBinding: Binding<Date> {
        Binding(
            get: {
                if case .integer(let millis) = state.value("start_at") { Date(timeIntervalSince1970: Double(millis) / 1000) } else { .now }
            },
            set: { store.send(.set("start_at", .integer(Int64(($0.timeIntervalSince1970 * 1000).rounded())))) }
        )
    }

    private var timeBinding: Binding<Date> {
        Binding(
            get: {
                let parts = state.text("time").split(separator: ":").compactMap { Int($0) }
                return calendar.date(
                    bySettingHour: parts.first ?? 9, minute: parts.dropFirst().first ?? 0, second: 0, of: .now
                ) ?? .now
            },
            set: {
                let parts = calendar.dateComponents([.hour, .minute], from: $0)
                store.send(.set("time", .string(String(format: "%02d:%02d", parts.hour ?? 9, parts.minute ?? 0))))
            }
        )
    }
}
