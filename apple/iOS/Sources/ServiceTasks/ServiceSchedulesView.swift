import AlondraWorkspaceFeature
import AlondraShared
import AlondraTasksFeature
import ComposableArchitecture
import SwiftUI

struct ServiceSchedulesView: View {
    @Bindable var store: StoreOf<ServiceTasksFeature>
    private var tasks: [ScheduledTask] { store.schedules.tasks }
    private var loading: Bool { store.schedules.isLoading }
    private var error: String? { store.schedules.error }
    var body: some View {
        content
            .navigationTitle("Scheduled Tasks")
            .toolbar {
                Button("Add Schedule", systemImage: "plus") { store.send(.schedules(.createTapped)) }
                Button("Refresh", systemImage: "arrow.clockwise") { store.send(.schedules(.refresh)) }
            }
            .task { await store.send(.schedules(.appeared)).finish() }
            .sheet(isPresented: Binding(get: { store.schedules.draft != nil }, set: { if !$0 { store.send(.schedules(.editorDismissed)) } })) {
                ServiceScheduleEditor(store: store.scope(state: \.schedules, action: \.schedules))
            }
            .confirmationDialog("Delete scheduled task?", isPresented: Binding(
                get: { store.deletingScheduleID != nil }, set: { if !$0 { store.send(.deleteSchedule(nil)) } }
            )) {
                Button("Delete", role: .destructive) {
                    store.send(.confirmDeleteSchedule)
                    store.send(.deleteSchedule(nil))
                }
            } message: {
                Text("Future runs will stop. Existing requests stay in Tasks.")
            }
    }

    @ViewBuilder private var content: some View {
        if tasks.isEmpty && !loading {
            emptyState
        } else {
            List {
                errorBanner
                ForEach(tasks, id: \.id) { task in
                    Button { store.send(.schedules(.editTapped(task.id))) } label: { summaryRow(task) }
                        .buttonStyle(.plain)
                        .swipeActions(edge: .leading, allowsFullSwipe: false) {
                            Button("Run", systemImage: "play.fill") { store.send(.schedules(.runTapped(task.id))) }
                                .tint(.green)
                            if task.frequency != "once" || task.nextRun != nil {
                                Button(String(localized: task.enabled ? "Pause" : "Resume", table: "AlondraShared", bundle: SchedulePresentation.bundle), systemImage: task.enabled ? "pause.fill" : "play.circle") {
                                    store.send(.schedules(.enabledChanged(task.id, !task.enabled)))
                                }
                                .tint(.orange)
                            }
                        }
                        .swipeActions(edge: .trailing, allowsFullSwipe: false) {
                            Button("Delete", systemImage: "trash", role: .destructive) { store.send(.deleteSchedule(task.id)) }
                            Button("Edit", systemImage: "pencil") { store.send(.schedules(.editTapped(task.id))) }
                                .tint(.blue)
                        }
                        .contextMenu { taskActions(task) }
                        .disabled(store.schedules.inFlightTargets.contains(.schedule(task.id)))
                }
            }
            .refreshable { await store.send(.schedules(.refresh)).finish() }
        }
    }
    @ViewBuilder private var errorBanner: some View {
        if let error {
            Label(SchedulePresentation.error(error), systemImage: "exclamationmark.triangle")
                .foregroundStyle(.red)
        }
    }

    @ViewBuilder private var emptyState: some View {
        if let error {
            ContentUnavailableView {
                Label("Could not load scheduled tasks", systemImage: "wifi.slash")
            } description: {
                Text(SchedulePresentation.error(error))
            } actions: {
                Button("Retry") { store.send(.schedules(.refresh)) }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            ContentUnavailableView("No scheduled tasks", systemImage: "calendar.badge.clock", description: Text(String(localized: "Run a task automatically on a schedule.", table: "AlondraShared", bundle: SchedulePresentation.bundle)))
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    private func summaryRow(_ task: ScheduledTask) -> some View {
        HStack(alignment: .top, spacing: 12) {
            Image(systemName: task.frequency == "once" ? "calendar" : "calendar.badge.clock")
                .foregroundStyle(task.enabled && task.nextRun != nil ? Color.accentColor : Color.secondary)
                .frame(width: 20)
            VStack(alignment: .leading, spacing: 4) {
                HStack {
                    Text(task.title).font(.headline)
                    Spacer(minLength: 8)
                    if !task.enabled { Text(String(localized: "Paused", table: "AlondraShared", bundle: SchedulePresentation.bundle)).font(.caption).foregroundStyle(.secondary) }
                    else if task.frequency == "once" && task.nextRun == nil { Text(String(localized: "Completed", table: "AlondraShared", bundle: SchedulePresentation.bundle)).font(.caption).foregroundStyle(.secondary) }
                }
                Text(SchedulePresentation.description(task))
                    .font(.subheadline).foregroundStyle(.secondary)
                if let next = task.nextRun, task.status != "waiting", task.frequency != "once" {
                    Text("\(String(localized: "Next", table: "AlondraShared", bundle: SchedulePresentation.bundle)): \(Date(timeIntervalSince1970: Double(next) / 1000).formatted(date: .abbreviated, time: .shortened))")
                        .font(.caption).foregroundStyle(.secondary)
                } else if task.status == "waiting" {
                    Text(String(localized: "Waiting for orchestrator", table: "AlondraShared", bundle: SchedulePresentation.bundle))
                        .font(.caption).foregroundStyle(.orange)
                }
            }
        }
        .padding(.vertical, 5)
        .contentShape(.rect)
    }

    @ViewBuilder private func taskActions(_ task: ScheduledTask) -> some View {
        Button("Edit", systemImage: "pencil") { store.send(.schedules(.editTapped(task.id))) }
        Button("Run", systemImage: "play.fill") { store.send(.schedules(.runTapped(task.id))) }
        if task.frequency != "once" || task.nextRun != nil {
            Button(String(localized: task.enabled ? "Pause" : "Resume", table: "AlondraShared", bundle: SchedulePresentation.bundle), systemImage: task.enabled ? "pause" : "play") {
                store.send(.schedules(.enabledChanged(task.id, !task.enabled)))
            }
        }
        Divider()
        Button("Delete", systemImage: "trash", role: .destructive) { store.send(.deleteSchedule(task.id)) }
    }

}
private struct ServiceScheduleEditor: View {
    let store: StoreOf<ScheduleFeature>
    var body: some View {
        ServiceScheduleForm(draft: Binding(get: { store.draft ?? ScheduledTaskDraft() }, set: { store.send(.draftChanged($0)) }),
                            specs: store.optionSpecs, projects: store.projects, error: store.error,
                            saving: store.isSaving || !store.optionsReady,
                            editing: store.editingID != nil,
                            cancel: { store.send(.editorDismissed) }, save: { store.send(.saveTapped) })
    }
}
private struct ServiceScheduleForm: View {
    @Binding var draft: ScheduledTaskDraft
    let specs: [OptionSpec]
    let projects: [String]
    let error: String?
    let saving: Bool
    let editing: Bool
    let cancel: () -> Void
    let save: () -> Void
    private var editorTextHeight: CGFloat { 112 }
    private var disallowedToolsText: Binding<String> {
        Binding(get: { draft.disallowedTools.joined(separator: "\n") }, set: { draft.disallowedTools = $0.components(separatedBy: .newlines) })
    }
    var body: some View {
        NavigationStack {
            Form {
                missionSection
                whenSection
                optionsSection
                advancedSection
                if let error { Text(SchedulePresentation.error(error)).foregroundStyle(.red) }
            }
            .navigationTitle(editing ? "Edit Schedule" : "New Schedule")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel", systemImage: "xmark", action: cancel)
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save", systemImage: "checkmark", action: save)
                        .disabled(saving || draft.title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || draft.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
            }
        }
    }
    private var missionSection: some View {
        Section("Mission") {
            TextField("Name", text: $draft.title)
            ZStack(alignment: .topLeading) {
                if draft.text.isEmpty {
                    Text(String(localized: "What should the node do?", table: "AlondraShared", bundle: SchedulePresentation.bundle))
                        .foregroundStyle(.tertiary)
                        .padding(.top, 8)
                        .padding(.leading, 5)
                        .allowsHitTesting(false)
                }
                TextEditor(text: $draft.text)
                    .scrollContentBackground(.hidden)
                    .frame(height: editorTextHeight)
                    .accessibilityLabel("Mission instructions")
            }
            Picker("Project", selection: Binding(
                get: { draft.project ?? "" },
                set: { draft.project = $0.isEmpty ? nil : $0 }
            )) {
                Text(String(localized: "Projects folder", table: "AlondraShared", bundle: SchedulePresentation.bundle)).tag("")
                ForEach(projects, id: \.self) { name in Text(name).tag(name) }
                if let current = draft.project, !projects.contains(current) { Text(current).tag(current) }
            }
        }
    }

    private var whenSection: some View {
        Section("When") {
            Picker("Repeat", selection: repeatMode) {
                Text("Every day").tag("daily")
                Text("Every week").tag("weekly")
                Text(String(localized: "Custom", table: "AlondraShared", bundle: SchedulePresentation.bundle)).tag("custom")
                Text(String(localized: "Once", table: "AlondraShared", bundle: SchedulePresentation.bundle)).tag("once")
            }
            if draft.frequency == "weekly" {
                Picker("Day", selection: Binding(get: { draft.weekday ?? 1 }, set: { draft.weekday = $0 })) {
                    ForEach(0..<7, id: \.self) { day in
                        Text(Calendar.current.weekdaySymbols[day]).tag(day)
                    }
                }
            }
            if repeatMode.wrappedValue == "custom" {
                Picker(String(localized: "Unit", table: "AlondraShared", bundle: SchedulePresentation.bundle), selection: $draft.frequency) {
                    Text(String(localized: "Hours", table: "AlondraShared", bundle: SchedulePresentation.bundle)).tag("every_hours")
                    Text(String(localized: "Days", table: "AlondraShared", bundle: SchedulePresentation.bundle)).tag("every_days")
                    Text(String(localized: "Months", table: "AlondraShared", bundle: SchedulePresentation.bundle)).tag("every_months")
                }
                Stepper(value: Binding(
                    get: { draft.interval ?? 1 },
                    set: { draft.interval = $0 }
                ), in: 1...365) {
                    LabeledContent(String(localized: "Every", table: "AlondraShared", bundle: SchedulePresentation.bundle), value: "\(draft.interval ?? 1)")
                }
                DatePicker(String(localized: "Starts", table: "AlondraShared", bundle: SchedulePresentation.bundle), selection: startDate, displayedComponents: [.date, .hourAndMinute])
            } else if draft.frequency == "once" {
                DatePicker(String(localized: "Date and time", table: "AlondraShared", bundle: SchedulePresentation.bundle), selection: startDate, displayedComponents: [.date, .hourAndMinute])
            } else {
                DatePicker("Time", selection: timeOfDay, displayedComponents: .hourAndMinute)
            }
            Text(String(localized: "Uses this device’s local time zone. Missed runs are sent once when Alondra starts again.", table: "AlondraShared", bundle: SchedulePresentation.bundle))
                .font(.caption).foregroundStyle(.secondary)
                .frame(maxWidth: 380, alignment: .leading)
                .fixedSize(horizontal: false, vertical: true)
            Toggle("Active", isOn: $draft.enabled)
        }
    }

    private var repeatMode: Binding<String> {
        Binding(
            get: { draft.frequency.hasPrefix("every_") ? "custom" : draft.frequency },
            set: { draft.frequency = $0 == "custom" ? "every_hours" : $0 }
        )
    }

    private var startDate: Binding<Date> {
        Binding(
            get: { Date(timeIntervalSince1970: Double(draft.startAt ?? 0) / 1000) },
            set: { draft.startAt = Int64(($0.timeIntervalSince1970 * 1000).rounded()) }
        )
    }

    private var timeOfDay: Binding<Date> {
        Binding(
            get: { Calendar.current.date(bySettingHour: draft.hour, minute: draft.minute, second: 0, of: .now) ?? .now },
            set: {
                let components = Calendar.current.dateComponents([.hour, .minute], from: $0)
                draft.hour = components.hour ?? 9
                draft.minute = components.minute ?? 0
            }
        )
    }

    @ViewBuilder private var optionsSection: some View {
        if !specs.isEmpty {
            Section("Launch options") {
                ForEach(specs, id: \.id) { spec in
                    optionControl(spec)
                }
            }
        }
    }

    private var advancedSection: some View {
        Section(String(localized: "Advanced", table: "AlondraShared", bundle: SchedulePresentation.bundle)) {
            TextField(String(localized: "Disallowed tools (one pattern per line)", table: "AlondraShared", bundle: SchedulePresentation.bundle), text: disallowedToolsText, axis: .vertical)
                .lineLimit(2...6)
            Text(String(localized: "These tool patterns are passed to Claude as disallowed tools for every run.", table: "AlondraShared", bundle: SchedulePresentation.bundle))
                .font(.caption).foregroundStyle(.secondary)
        }
    }

    @ViewBuilder private func optionControl(_ spec: OptionSpec) -> some View {
        if spec.kind == .toggle {
            Toggle(spec.title, isOn: Binding(
                get: { (draft.options[spec.id] ?? spec.default) == .toggle(true) },
                set: { draft.options[spec.id] = .toggle($0) == spec.default ? nil : .toggle($0) }
            ))
        } else {
            Picker(spec.title, selection: Binding(
                get: { draft.options[spec.id] ?? spec.default },
                set: { draft.options[spec.id] = $0 == spec.default ? nil : $0 }
            )) {
                ForEach(spec.values, id: \.id) { value in
                    Text(spec.title(of: .value(value.id))).tag(OptionChoice.value(value.id))
                }
            }
        }
    }

}
