import Foundation
import SwiftUI

public struct ScheduledTaskList: Decodable, Sendable {
    public let schedules: [ScheduledTask]
}

public struct ScheduledTask: Decodable, Identifiable, Equatable, Sendable {
    public let id: String
    public let title: String
    public let text: String
    public let project: String?
    public let options: [String: OptionChoice]
    public let disallowedTools: [String]
    public let frequency: String
    public let hour: Int
    public let minute: Int
    public let weekday: Int?
    public let interval: Int?
    public let startAt: Int64?
    public let enabled: Bool
    public let nextRun: Int64?
    public let lastRun: Int64?
    public let lastRequest: String?
    public let lastState: String?
    public let lastNodes: [String]?
    public let lastError: String?
    public let status: String
    public let revision: Int
}

public struct ScheduledTaskDraft: Encodable, Equatable, Sendable {
    public var title: String
    public var text: String
    public var project: String?
    public var options: [String: OptionChoice]
    public var disallowedTools: [String]
    public var frequency: String
    public var hour: Int
    public var minute: Int
    public var weekday: Int?
    public var interval: Int?
    public var startAt: Int64?
    public var enabled: Bool
    public var revision: Int

    private enum CodingKeys: String, CodingKey {
        case title, text, project, options, frequency, hour, minute, weekday, interval, enabled, revision
        case disallowedTools = "disallowed_tools"
        case startAt = "start_at"
    }

    public init(_ task: ScheduledTask? = nil) {
        title = task?.title ?? ""
        text = task?.text ?? ""
        project = task?.project
        options = task?.options ?? [:]
        disallowedTools = task?.disallowedTools ?? []
        frequency = task?.frequency ?? "daily"
        hour = task?.hour ?? 9
        minute = task?.minute ?? 0
        weekday = task?.weekday ?? 1
        interval = task?.interval ?? 1
        startAt = task?.startAt ?? Int64((Date.now.addingTimeInterval(3600).timeIntervalSince1970 * 1000).rounded())
        enabled = task?.enabled ?? true
        revision = task?.revision ?? 0
    }
}

/// Shared management surface; both Apple apps use the same wording and server semantics.
public struct ScheduledTasksView: View {
    public let client: AlondraClient
    @State private var tasks: [ScheduledTask] = []
    @State private var editing: ScheduledTask?
    @State private var showingCreate = false
    @State private var deleting: ScheduledTask?
    @State private var selectedID: String?
    #if os(macOS)
    @State private var inspectorVisible = false
    #endif
    @State private var loading = false
    @State private var error: String?

    public init(client: AlondraClient) { self.client = client }

    public var body: some View {
        content
            .navigationTitle("Scheduled Tasks")
            .toolbar {
                Button("Add Schedule", systemImage: "plus") { showingCreate = true }
                Button("Refresh", systemImage: "arrow.clockwise") { Task { await refresh() } }
                #if os(macOS)
                Button(String(localized: "Inspector", table: "AlondraShared", bundle: #bundle), systemImage: "sidebar.right") { inspectorVisible.toggle() }
                    .disabled(selectedID == nil)
                #endif
            }
            .task(id: client.identity) { await refresh() }
            .sheet(isPresented: $showingCreate) {
                ScheduleEditor(client: client, task: nil) { await refresh() }
            }
            .sheet(item: $editing) { task in
                ScheduleEditor(client: client, task: task) { await refresh() }
            }
            .confirmationDialog("Delete scheduled task?", isPresented: Binding(
                get: { deleting != nil }, set: { if !$0 { deleting = nil } }
            )) {
                Button("Delete", role: .destructive) {
                    if let deleting { Task { await delete(deleting) } }
                    deleting = nil
                }
            } message: {
                Text("Future runs will stop. Existing requests stay in Tasks.")
            }
    }

    @ViewBuilder private var content: some View {
        #if os(iOS)
        if tasks.isEmpty && !loading {
            emptyState
        } else {
            List {
                errorBanner
                ForEach(tasks, id: \.id) { task in
                    Button { editing = task } label: { summaryRow(task) }
                        .buttonStyle(.plain)
                        .swipeActions(edge: .leading, allowsFullSwipe: false) {
                            Button("Run", systemImage: "play.fill") { Task { await run(task) } }
                                .tint(.green)
                            if task.frequency != "once" || task.nextRun != nil {
                                Button(String(localized: task.enabled ? "Pause" : "Resume", table: "AlondraShared", bundle: #bundle), systemImage: task.enabled ? "pause.fill" : "play.circle") {
                                    Task { await setEnabled(task, !task.enabled) }
                                }
                                .tint(.orange)
                            }
                        }
                        .swipeActions(edge: .trailing, allowsFullSwipe: false) {
                            Button("Delete", systemImage: "trash", role: .destructive) { deleting = task }
                            Button("Edit", systemImage: "pencil") { editing = task }
                                .tint(.blue)
                        }
                        .contextMenu { taskActions(task) }
                }
            }
            .refreshable { await refresh() }
        }
        #else
        Group {
            if tasks.isEmpty && !loading {
                emptyState
            } else {
                List(selection: $selectedID) {
                    errorBanner
                    ForEach(tasks, id: \.id) { task in
                        summaryRow(task)
                            .tag(task.id)
                            .contextMenu { taskActions(task) }
                    }
                }
                .listStyle(.inset)
            }
        }
        .onChange(of: selectedID) { _, newValue in inspectorVisible = newValue != nil }
        .inspector(isPresented: $inspectorVisible) {
            if let selected = tasks.first(where: { $0.id == selectedID }) {
                detail(selected)
                    .inspectorColumnWidth(min: 270, ideal: 320, max: 380)
            }
        }
        #endif
    }

    @ViewBuilder private var errorBanner: some View {
        if let error {
            Label(displayError(error), systemImage: "exclamationmark.triangle")
                .foregroundStyle(.red)
        }
    }

    @ViewBuilder private var emptyState: some View {
        if let error {
            ContentUnavailableView {
                Label("Could not load scheduled tasks", systemImage: "wifi.slash")
            } description: {
                Text(displayError(error))
            } actions: {
                Button("Retry") { Task { await refresh() } }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            ContentUnavailableView("No scheduled tasks", systemImage: "calendar.badge.clock", description: Text(String(localized: "Run a task automatically on a schedule.", table: "AlondraShared", bundle: #bundle)))
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
                    if !task.enabled { Text(String(localized: "Paused", table: "AlondraShared", bundle: #bundle)).font(.caption).foregroundStyle(.secondary) }
                    else if task.frequency == "once" && task.nextRun == nil { Text(String(localized: "Completed", table: "AlondraShared", bundle: #bundle)).font(.caption).foregroundStyle(.secondary) }
                }
                Text(scheduleDescription(task))
                    .font(.subheadline).foregroundStyle(.secondary)
                if let next = task.nextRun, task.status != "waiting", task.frequency != "once" {
                    Text("\(String(localized: "Next", table: "AlondraShared", bundle: #bundle)): \(Date(timeIntervalSince1970: Double(next) / 1000).formatted(date: .abbreviated, time: .shortened))")
                        .font(.caption).foregroundStyle(.secondary)
                } else if task.status == "waiting" {
                    Text(String(localized: "Waiting for orchestrator", table: "AlondraShared", bundle: #bundle))
                        .font(.caption).foregroundStyle(.orange)
                }
            }
        }
        .padding(.vertical, 5)
        .contentShape(.rect)
    }

    @ViewBuilder private func taskActions(_ task: ScheduledTask) -> some View {
        Button("Edit", systemImage: "pencil") { editing = task }
        Button("Run", systemImage: "play.fill") { Task { await run(task) } }
        if task.frequency != "once" || task.nextRun != nil {
            Button(String(localized: task.enabled ? "Pause" : "Resume", table: "AlondraShared", bundle: #bundle), systemImage: task.enabled ? "pause" : "play") {
                Task { await setEnabled(task, !task.enabled) }
            }
        }
        Divider()
        Button("Delete", systemImage: "trash", role: .destructive) { deleting = task }
    }

    #if os(macOS)
    private func detail(_ task: ScheduledTask) -> some View {
        VStack(spacing: 0) {
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    Text(task.title).font(.title3.weight(.semibold))
                    Text(task.enabled ?
                        (task.frequency == "once" && task.nextRun == nil ?
                            String(localized: "Completed", table: "AlondraShared", bundle: #bundle) :
                            String(localized: "Active", table: "AlondraShared", bundle: #bundle)) :
                        String(localized: "Paused", table: "AlondraShared", bundle: #bundle))
                        .font(.subheadline).foregroundStyle(.secondary)
                    Divider()
                    VStack(alignment: .leading, spacing: 4) {
                        Text(String(localized: "Schedule", table: "AlondraShared", bundle: #bundle))
                            .font(.caption).foregroundStyle(.secondary)
                        Text(scheduleDescription(task))
                    }
                    if let next = task.nextRun {
                        VStack(alignment: .leading, spacing: 4) {
                            Text(String(localized: "Next", table: "AlondraShared", bundle: #bundle))
                                .font(.caption).foregroundStyle(.secondary)
                            Text(Date(timeIntervalSince1970: Double(next) / 1000).formatted(date: .abbreviated, time: .shortened))
                        }
                    }
                    Divider()
                    Text(task.text).textSelection(.enabled)
                    if let last = task.lastRun {
                        Divider()
                        VStack(alignment: .leading, spacing: 4) {
                            Text(String(localized: "Last", table: "AlondraShared", bundle: #bundle))
                                .font(.caption).foregroundStyle(.secondary)
                            Text("\(Date(timeIntervalSince1970: Double(last) / 1000).formatted(date: .abbreviated, time: .shortened)) · \(stateLabel(task.lastError == nil ? (task.lastState ?? task.status) : task.status))")
                        }
                    }
                    if let lastError = task.lastError {
                        Label(displayError(lastError), systemImage: "exclamationmark.triangle")
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
                Button("Edit", systemImage: "pencil") { editing = task }
                Button("Run", systemImage: "play.fill") { Task { await run(task) } }
                Menu { taskActions(task) } label: { Label("More", systemImage: "ellipsis") }
                    .labelStyle(.iconOnly)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(12)
        }
    }
    #endif

    private func scheduleDescription(_ task: ScheduledTask) -> String {
        let time = String(format: "%02d:%02d", task.hour, task.minute)
        let at = String(localized: "at", table: "AlondraShared", bundle: #bundle)
        let project = task.project ?? String(localized: "Projects folder", table: "AlondraShared", bundle: #bundle)
        let description: String
        switch task.frequency {
        case "weekly" where task.weekday != nil:
            let weekday = task.weekday!
            let names = Calendar.current.weekdaySymbols
            description = "\(String(localized: "Every", table: "AlondraShared", bundle: #bundle)) \(names[weekday]) \(at) \(time)"
        case "every_hours":
            description = task.interval == 1 ? String(localized: "Every hour", table: "AlondraShared", bundle: #bundle) :
                String.localizedStringWithFormat(String(localized: "Every %lld hours", table: "AlondraShared", bundle: #bundle), Int64(task.interval ?? 1))
        case "every_days":
            description = task.interval == 1 ? String(localized: "Every day", table: "AlondraShared", bundle: #bundle) :
                String.localizedStringWithFormat(String(localized: "Every %lld days", table: "AlondraShared", bundle: #bundle), Int64(task.interval ?? 1))
        case "every_months":
            description = task.interval == 1 ? String(localized: "Every month", table: "AlondraShared", bundle: #bundle) :
                String.localizedStringWithFormat(String(localized: "Every %lld months", table: "AlondraShared", bundle: #bundle), Int64(task.interval ?? 1))
        case "once":
            description = task.startAt.map { Date(timeIntervalSince1970: Double($0) / 1000).formatted(date: .abbreviated, time: .shortened) } ?? ""
        default:
            description = "\(String(localized: "Every day", table: "AlondraShared", bundle: #bundle)) \(at) \(time)"
        }
        return "\(description) · \(project)"
    }

    private func stateLabel(_ state: String) -> String {
        switch state {
        case "never_run": String(localized: "Never run", table: "AlondraShared", bundle: #bundle)
        case "launching": String(localized: "Sending", table: "AlondraShared", bundle: #bundle)
        case "sent": String(localized: "Sent", table: "AlondraShared", bundle: #bundle)
        case "failed": String(localized: "Failed", table: "AlondraShared", bundle: #bundle)
        case "interrupted": String(localized: "Interrupted", table: "AlondraShared", bundle: #bundle)
        case "waiting": String(localized: "Waiting for orchestrator", table: "AlondraShared", bundle: #bundle)
        case "defining": String(localized: "Preparing", table: "AlondraShared", bundle: #bundle)
        case "in_progress": String(localized: "In progress", table: "AlondraShared", bundle: #bundle)
        case "done": String(localized: "Done", table: "AlondraShared", bundle: #bundle)
        default: state
        }
    }

    private func displayError(_ message: String) -> String {
        scheduleError(message)
    }

    private func refresh() async {
        loading = true
        defer { loading = false }
        do {
            tasks = try await client.schedules()
            if !tasks.contains(where: { $0.id == selectedID }) { selectedID = nil }
            error = nil
        }
        catch { self.error = error.localizedDescription }
    }

    private func setEnabled(_ task: ScheduledTask, _ enabled: Bool) async {
        var draft = ScheduledTaskDraft(task)
        draft.enabled = enabled
        do { _ = try await client.saveSchedule(draft, id: task.id); await refresh() }
        catch { let message = error.localizedDescription; await refresh(); self.error = message }
    }

    private func run(_ task: ScheduledTask) async {
        do { try await client.runSchedule(task.id); await refresh() }
        catch { let message = error.localizedDescription; await refresh(); self.error = message }
    }

    private func delete(_ task: ScheduledTask) async {
        do { try await client.deleteSchedule(task.id); await refresh() }
        catch { self.error = error.localizedDescription }
    }
}

private func scheduleError(_ message: String) -> String {
    let key: String
    switch message {
    case "title must be 1–120 characters": key = "Enter a name of up to 120 characters."
    case "mission must be 1–20,000 characters": key = "Enter instructions of up to 20,000 characters."
    case "disallowed tools must be at most 100 nonempty tool patterns without spaces or flags": key = "Enter up to 100 tool patterns without spaces."
    case "time must be a valid local hour and minute": key = "Choose a valid time."
    case "weekly schedule needs weekday 0–6": key = "Choose a day of the week."
    case "custom interval must be 1–365": key = "Choose an interval from 1 to 365."
    case "start date must be before 2100": key = "Choose a date before 2100."
    case "one-time date must be in the future": key = "Choose a future date and time."
    case "project cannot be empty", "project must be one of the available project folders": key = "Choose an available project."
    case "projects directory does not exist": key = "The Projects folder is unavailable."
    case "schedule changed elsewhere; refresh before editing": key = "This schedule changed. Refresh and try again."
    case "schedule not found or a run is being sent", "already_running": key = "This schedule is running. Try again shortly."
    case "not_due": key = "This schedule is not due yet."
    case "the orchestrator is not connected to Alondra": key = "The orchestrator is offline"
    case "The scheduled task is waiting for the orchestrator": key = "Waiting for orchestrator"
    default: return message
    }
    return String(localized: String.LocalizationValue(key), table: "AlondraShared", bundle: #bundle)
}

private struct ScheduleEditor: View {
    let client: AlondraClient
    let task: ScheduledTask?
    let saved: () async -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var draft: ScheduledTaskDraft
    @State private var specs: [OptionSpec] = []
    @State private var projects: [String] = []
    @State private var error: String?
    @State private var saving = false
    @State private var disallowedToolsText: String

    init(client: AlondraClient, task: ScheduledTask?, saved: @escaping () async -> Void) {
        self.client = client
        self.task = task
        self.saved = saved
        _draft = State(initialValue: ScheduledTaskDraft(task))
        _disallowedToolsText = State(initialValue: task?.disallowedTools.joined(separator: "\n") ?? "")
    }

    var body: some View {
        #if os(macOS)
        macEditor
            .frame(width: 580, height: 700)
            .task(id: draft.project) { await loadOptions() }
        #else
        NavigationStack {
            Form {
                missionSection
                whenSection
                optionsSection
                advancedSection
                if let error { Text(scheduleError(error)).foregroundStyle(.red) }
            }
            .navigationTitle(task == nil ? "New Schedule" : "Edit Schedule")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel", systemImage: "xmark") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save", systemImage: "checkmark") { Task { await save() } }
                        .disabled(saving || draft.title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || draft.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
            }
            .task(id: draft.project) { await loadOptions() }
        }
        #endif
    }

    #if os(macOS)
    private var macEditor: some View {
        VStack(spacing: 0) {
            HStack {
                Text(task == nil ? "New Schedule" : "Edit Schedule")
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
                if let error { Text(scheduleError(error)).foregroundStyle(.red) }
            }
            .formStyle(.grouped)
            Divider()
            HStack(spacing: 10) {
                Spacer()
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Button("Save") { Task { await save() } }
                    .keyboardShortcut(.defaultAction)
                    .disabled(saving || draft.title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || draft.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
            .padding(.horizontal, 24)
            .padding(.vertical, 16)
        }
    }
    #endif

    private func loadOptions() async {
        do {
            if projects.isEmpty { projects = try await client.projects().projects.map(\.name) }
            specs = try await client.options(project: draft.project).options
            draft.options = draft.options.filter { id, choice in
                specs.first(where: { $0.id == id })?.accepts(choice) == true
            }
        }
        catch { self.error = error.localizedDescription }
    }

    private var editorTextHeight: CGFloat {
        #if os(macOS)
        90
        #else
        112
        #endif
    }

    private var missionSection: some View {
        Section("Mission") {
            TextField("Name", text: $draft.title)
            ZStack(alignment: .topLeading) {
                if draft.text.isEmpty {
                    Text(String(localized: "What should the node do?", table: "AlondraShared", bundle: #bundle))
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
                Text(String(localized: "Projects folder", table: "AlondraShared", bundle: #bundle)).tag("")
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
                Text(String(localized: "Custom", table: "AlondraShared", bundle: #bundle)).tag("custom")
                Text(String(localized: "Once", table: "AlondraShared", bundle: #bundle)).tag("once")
            }
            if draft.frequency == "weekly" {
                Picker("Day", selection: Binding(get: { draft.weekday ?? 1 }, set: { draft.weekday = $0 })) {
                    ForEach(0..<7, id: \.self) { day in
                        Text(Calendar.current.weekdaySymbols[day]).tag(day)
                    }
                }
            }
            if repeatMode.wrappedValue == "custom" {
                Picker(String(localized: "Unit", table: "AlondraShared", bundle: #bundle), selection: $draft.frequency) {
                    Text(String(localized: "Hours", table: "AlondraShared", bundle: #bundle)).tag("every_hours")
                    Text(String(localized: "Days", table: "AlondraShared", bundle: #bundle)).tag("every_days")
                    Text(String(localized: "Months", table: "AlondraShared", bundle: #bundle)).tag("every_months")
                }
                Stepper(value: Binding(
                    get: { draft.interval ?? 1 },
                    set: { draft.interval = $0 }
                ), in: 1...365) {
                    LabeledContent(String(localized: "Every", table: "AlondraShared", bundle: #bundle), value: "\(draft.interval ?? 1)")
                }
                DatePicker(String(localized: "Starts", table: "AlondraShared", bundle: #bundle), selection: startDate, displayedComponents: [.date, .hourAndMinute])
            } else if draft.frequency == "once" {
                DatePicker(String(localized: "Date and time", table: "AlondraShared", bundle: #bundle), selection: startDate, displayedComponents: [.date, .hourAndMinute])
            } else {
                DatePicker("Time", selection: timeOfDay, displayedComponents: .hourAndMinute)
            }
            Text(String(localized: "Uses this device’s local time zone. Missed runs are sent once when Alondra starts again.", table: "AlondraShared", bundle: #bundle))
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
        Section(String(localized: "Advanced", table: "AlondraShared", bundle: #bundle)) {
            TextField(String(localized: "Disallowed tools (one pattern per line)", table: "AlondraShared", bundle: #bundle), text: $disallowedToolsText, axis: .vertical)
                .lineLimit(2...6)
            Text(String(localized: "These tool patterns are passed to Claude as disallowed tools for every run.", table: "AlondraShared", bundle: #bundle))
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

    private func save() async {
        draft.disallowedTools = disallowedToolsText.split(whereSeparator: \.isNewline).map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }.filter { !$0.isEmpty }
        saving = true
        defer { saving = false }
        do {
            _ = try await client.saveSchedule(draft, id: task?.id)
            await saved()
            dismiss()
        } catch { self.error = error.localizedDescription }
    }
}
