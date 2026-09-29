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
                Button(String(localized: "Inspector", table: "AlondraShared", bundle: SchedulePresentation.bundle), systemImage: "sidebar.right") {
                    store.send(.schedules(.inspectorPresented(!store.schedules.inspectorVisible)))
                }
                .disabled(store.schedules.selectedID == nil)
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
                }
            } message: {
                Text("Future runs will stop. Existing requests stay in Tasks.")
            }
    }

    private var content: some View {
        Group {
            if tasks.isEmpty && !loading {
                VStack(spacing: 0) {
                    recoveryNotice
                    emptyState
                }
            } else {
                List(selection: Binding(get: { store.schedules.selectedID }, set: { store.send(.schedules(.selected($0))) })) {
                    errorBanner
                    recoveryNotice
                    ForEach(tasks, id: \.id) { task in
                        summaryRow(task)
                            .tag(task.id)
                            .contextMenu { taskActions(task) }
                    }
                }
                .listStyle(.inset)
            }
        }
        .inspector(isPresented: Binding(get: { store.schedules.inspectorVisible }, set: { store.send(.schedules(.inspectorPresented($0))) })) {
            if let selected = tasks.first(where: { $0.id == store.schedules.selectedID }) {
                detail(selected)
                    .inspectorColumnWidth(min: 270, ideal: 320, max: 380)
            }
        }
    }

    @ViewBuilder private var recoveryNotice: some View {
        if case .unknown = store.schedules.lastMutationOutcome {
            ServiceTaskOutcome(outcome: store.schedules.lastMutationOutcome)
        } else if case .obsolete = store.schedules.lastMutationOutcome {
            ServiceTaskOutcome(outcome: store.schedules.lastMutationOutcome)
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
        Group {
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
        .disabled(store.schedules.inFlightTargets.contains(.schedule(task.id)))
    }

    private func detail(_ task: ScheduledTask) -> some View {
        VStack(spacing: 0) {
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    Text(task.title).font(.title3.weight(.semibold))
                    Text(task.enabled ?
                        (task.frequency == "once" && task.nextRun == nil ?
                            String(localized: "Completed", table: "AlondraShared", bundle: SchedulePresentation.bundle) :
                            String(localized: "Active", table: "AlondraShared", bundle: SchedulePresentation.bundle)) :
                        String(localized: "Paused", table: "AlondraShared", bundle: SchedulePresentation.bundle))
                        .font(.subheadline).foregroundStyle(.secondary)
                    Divider()
                    VStack(alignment: .leading, spacing: 4) {
                        Text(String(localized: "Schedule", table: "AlondraShared", bundle: SchedulePresentation.bundle))
                            .font(.caption).foregroundStyle(.secondary)
                        Text(SchedulePresentation.description(task))
                    }
                    if let next = task.nextRun {
                        VStack(alignment: .leading, spacing: 4) {
                            Text(String(localized: "Next", table: "AlondraShared", bundle: SchedulePresentation.bundle))
                                .font(.caption).foregroundStyle(.secondary)
                            Text(Date(timeIntervalSince1970: Double(next) / 1000).formatted(date: .abbreviated, time: .shortened))
                        }
                    }
                    Divider()
                    Text(task.text).textSelection(.enabled)
                    if let last = task.lastRun {
                        Divider()
                        VStack(alignment: .leading, spacing: 4) {
                            Text(String(localized: "Last", table: "AlondraShared", bundle: SchedulePresentation.bundle))
                                .font(.caption).foregroundStyle(.secondary)
                            Text("\(Date(timeIntervalSince1970: Double(last) / 1000).formatted(date: .abbreviated, time: .shortened)) · \(SchedulePresentation.stateLabel(task.lastError == nil ? (task.lastState ?? task.status) : task.status))")
                        }
                    }
                    if let lastError = task.lastError {
                        Label(SchedulePresentation.error(lastError), systemImage: "exclamationmark.triangle")
                            .foregroundStyle(.red)
                    }
                    if let request = task.lastRequest { Text(request).font(.caption.monospaced()).foregroundStyle(.secondary) }
                    if let nodes = task.lastNodes, !nodes.isEmpty { Text(nodes.joined(separator: ", ")).font(.caption.monospaced()).foregroundStyle(.secondary) }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(16)
            }
            Divider()
            HStack {
                Button("Edit", systemImage: "pencil") { store.send(.schedules(.editTapped(task.id))) }
                Button("Run", systemImage: "play.fill") { store.send(.schedules(.runTapped(task.id))) }
                Menu { taskActions(task) } label: { Label("More", systemImage: "ellipsis") }
                    .labelStyle(.iconOnly)
            }
            .disabled(store.schedules.inFlightTargets.contains(.schedule(task.id)))
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(12)
        }
    }

}
private struct ServiceScheduleEditor: View {
    let store: StoreOf<ScheduleFeature>
    var body: some View {
        ServiceScheduleForm(draft: Binding(get: { store.draft ?? ScheduledTaskDraft() }, set: { store.send(.draftChanged($0)) }),
                            specs: store.optionSpecs, projects: store.projects, error: store.error,
                            saving: !store.canSaveDraft,
                            outcome: store.lastMutationOutcome,
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
    let outcome: TaskMutationOutcome?
    let editing: Bool
    let cancel: () -> Void
    let save: () -> Void
    private var editorTextHeight: CGFloat { 90 }
    private var disallowedToolsText: Binding<String> {
        Binding(get: { draft.disallowedTools.joined(separator: "\n") }, set: { draft.disallowedTools = $0.components(separatedBy: .newlines) })
    }
    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Text(editing ? "Edit Schedule" : "New Schedule")
                    .font(.title3.weight(.semibold))
                Spacer()
            }
            .padding(.horizontal, 24)
            .padding(.vertical, 18)
            Divider()
            Form {
                missionSection
                whenSection
                optionsSection
                advancedSection
                if let error { Text(SchedulePresentation.error(error)).foregroundStyle(.red) }
                if case .unknown = outcome { ServiceTaskOutcome(outcome: outcome) }
                else if case .obsolete = outcome { ServiceTaskOutcome(outcome: outcome) }
            }
            .formStyle(.grouped)
            Divider()
            HStack(spacing: 10) {
                Spacer()
                Button("Cancel", action: cancel)
                    .keyboardShortcut(.cancelAction)
                Button("Save", action: save)
                    .keyboardShortcut(.defaultAction)
                    .disabled(saving)
            }
            .padding(.horizontal, 24)
            .padding(.vertical, 16)
        }
        .frame(width: 580, height: 700)
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
