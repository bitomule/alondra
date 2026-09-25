import Foundation
import SwiftUI

public struct ScheduledTaskList: Decodable, Sendable {
    public let schedules: [ScheduledTask]
}

public struct ScheduledTask: Decodable, Identifiable, Sendable {
    public let id: String
    public let title: String
    public let text: String
    public let project: String?
    public let options: [String: OptionChoice]
    public let frequency: String
    public let hour: Int
    public let minute: Int
    public let weekday: Int?
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

public struct ScheduledTaskDraft: Encodable, Sendable {
    public var title: String
    public var text: String
    public var project: String?
    public var options: [String: OptionChoice]
    public var frequency: String
    public var hour: Int
    public var minute: Int
    public var weekday: Int?
    public var enabled: Bool
    public var revision: Int

    public init(_ task: ScheduledTask? = nil) {
        title = task?.title ?? ""
        text = task?.text ?? ""
        project = task?.project
        options = task?.options ?? [:]
        frequency = task?.frequency ?? "daily"
        hour = task?.hour ?? 9
        minute = task?.minute ?? 0
        weekday = task?.weekday ?? 1
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
    @State private var loading = false
    @State private var error: String?

    public init(client: AlondraClient) { self.client = client }

    public var body: some View {
        List {
            if let error, !tasks.isEmpty {
                Label(displayError(error), systemImage: "exclamationmark.triangle")
                    .foregroundStyle(.red)
            }
            if tasks.isEmpty && !loading {
                if let error {
                    ContentUnavailableView("Could not load scheduled tasks", systemImage: "wifi.slash", description: Text(displayError(error)))
                    Button("Retry") { Task { await refresh() } }
                } else {
                    ContentUnavailableView("No scheduled tasks", systemImage: "calendar.badge.clock", description: Text("Create a recurring mission for the orchestrator."))
                }
            }
            ForEach(tasks, id: \.id) { task in
                VStack(alignment: .leading, spacing: 8) {
                    HStack {
                        Text(task.title).font(.headline)
                        Spacer()
                        Text(task.enabled ? String(localized: "Active", table: "AlondraShared", bundle: #bundle) : String(localized: "Paused", table: "AlondraShared", bundle: #bundle))
                            .font(.caption.weight(.semibold))
                            .foregroundStyle(task.enabled ? Color.green : Color.secondary)
                    }
                    Text(scheduleDescription(task))
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                    Text(task.text)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                    if task.status != "waiting", let next = task.nextRun {
                        Label("\(String(localized: "Next", table: "AlondraShared", bundle: #bundle)): \(Date(timeIntervalSince1970: Double(next) / 1000).formatted(date: .abbreviated, time: .shortened))", systemImage: "calendar")
                            .font(.caption)
                    }
                    if task.status == "waiting" {
                        Label("Waiting for orchestrator", systemImage: "clock.arrow.circlepath")
                            .font(.caption)
                            .foregroundStyle(.orange)
                    }
                    if let last = task.lastRun {
                        Label("\(String(localized: "Last", table: "AlondraShared", bundle: #bundle)): \(Date(timeIntervalSince1970: Double(last) / 1000).formatted(date: .abbreviated, time: .shortened)) · \(stateLabel(task.lastState ?? task.status))", systemImage: task.lastError == nil ? "checkmark.circle" : "exclamationmark.circle")
                            .font(.caption)
                            .foregroundStyle(task.lastError == nil ? Color.secondary : Color.red)
                    }
                    if let lastError = task.lastError {
                        Text(displayError(lastError)).font(.caption).foregroundStyle(.red)
                    }
                    if let request = task.lastRequest {
                        Text("\(String(localized: "Request", table: "AlondraShared", bundle: #bundle)) \(request)").font(.caption.monospaced()).foregroundStyle(.secondary)
                    }
                    if let nodes = task.lastNodes, !nodes.isEmpty {
                        Text("\(String(localized: "Nodes", table: "AlondraShared", bundle: #bundle)): \(nodes.joined(separator: ", "))").font(.caption.monospaced()).foregroundStyle(.secondary)
                    }
                    HStack(spacing: 8) {
                        Button("Edit", systemImage: "pencil") { editing = task }
                            .frame(minHeight: 44)
                            .contentShape(.rect)
                        Button("Run", systemImage: "play.fill") { Task { await run(task) } }
                            .frame(minHeight: 44)
                            .contentShape(.rect)
                        Menu {
                            Button(task.enabled ? String(localized: "Pause", table: "AlondraShared", bundle: #bundle) : String(localized: "Resume", table: "AlondraShared", bundle: #bundle), systemImage: task.enabled ? "pause" : "play") {
                                Task { await setEnabled(task, !task.enabled) }
                            }
                            Button("Delete", systemImage: "trash", role: .destructive) { deleting = task }
                        } label: {
                            Label("More", systemImage: "ellipsis.circle")
                                .labelStyle(.iconOnly)
                                .frame(width: 44, height: 44)
                                .background(Color.accentColor.opacity(0.12), in: .circle)
                        }
                        .buttonStyle(.plain)
                    }
                    .buttonStyle(.bordered)
                    .font(.caption.weight(.medium))
                }
                .padding(.vertical, 7)
            }
        }
        .scrollContentBackground(.hidden)
        .navigationTitle("Scheduled Tasks")
        .toolbar {
            Button("Add Schedule", systemImage: "plus") { showingCreate = true }
            Button("Refresh", systemImage: "arrow.clockwise") { Task { await refresh() } }
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

    private func scheduleDescription(_ task: ScheduledTask) -> String {
        let time = String(format: "%02d:%02d", task.hour, task.minute)
        let at = String(localized: "at", table: "AlondraShared", bundle: #bundle)
        let project = task.project ?? String(localized: "Any project", table: "AlondraShared", bundle: #bundle)
        if task.frequency == "weekly", let weekday = task.weekday {
            let names = Calendar.current.weekdaySymbols
            return "\(String(localized: "Every", table: "AlondraShared", bundle: #bundle)) \(names[weekday]) \(at) \(time) · \(project)"
        }
        return "\(String(localized: "Every day", table: "AlondraShared", bundle: #bundle)) \(at) \(time) · \(project)"
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
        if message == "the orchestrator is not connected to Alondra" {
            return String(localized: "The orchestrator is offline", table: "AlondraShared", bundle: #bundle)
        }
        if message == "The scheduled task is waiting for the orchestrator" {
            return String(localized: "Waiting for orchestrator", table: "AlondraShared", bundle: #bundle)
        }
        return message
    }

    private func refresh() async {
        loading = true
        defer { loading = false }
        do { tasks = try await client.schedules(); error = nil }
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

    init(client: AlondraClient, task: ScheduledTask?, saved: @escaping () async -> Void) {
        self.client = client
        self.task = task
        self.saved = saved
        _draft = State(initialValue: ScheduledTaskDraft(task))
    }

    var body: some View {
        NavigationStack {
            Form {
                missionSection
                whenSection
                optionsSection
                if let error { Text(error).foregroundStyle(.red) }
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
            .task(id: draft.project) {
                do {
                    if projects.isEmpty { projects = try await client.projects().projects.map(\.name) }
                    specs = try await client.options(project: draft.project).options
                    draft.options = draft.options.filter { id, choice in
                        specs.first(where: { $0.id == id })?.accepts(choice) == true
                    }
                }
                catch { self.error = error.localizedDescription }
            }
        }
        .frame(minWidth: editorMinimumWidth, minHeight: 480)
    }

    private var editorMinimumWidth: CGFloat? {
        #if os(macOS)
        400
        #else
        nil
        #endif
    }

    private var missionSection: some View {
        Section("Mission") {
            TextField("Name", text: $draft.title)
            ZStack(alignment: .topLeading) {
                if draft.text.isEmpty {
                    Text("What should the orchestrator do?")
                        .foregroundStyle(.tertiary)
                        .padding(.top, 8)
                        .padding(.leading, 5)
                        .allowsHitTesting(false)
                }
                TextEditor(text: $draft.text)
                    .scrollContentBackground(.hidden)
                    .frame(minHeight: 110)
                    .accessibilityLabel("Mission instructions")
            }
            Picker("Project", selection: Binding(
                get: { draft.project ?? "" },
                set: { draft.project = $0.isEmpty ? nil : $0 }
            )) {
                Text("The orchestrator decides").tag("")
                ForEach(projects, id: \.self) { name in Text(name).tag(name) }
                if let current = draft.project, !projects.contains(current) { Text(current).tag(current) }
            }
        }
    }

    private var whenSection: some View {
        Section("When") {
            Picker("Repeat", selection: $draft.frequency) {
                Text("Every day").tag("daily")
                Text("Every week").tag("weekly")
            }
            if draft.frequency == "weekly" {
                Picker("Day", selection: Binding(get: { draft.weekday ?? 1 }, set: { draft.weekday = $0 })) {
                    ForEach(0..<7, id: \.self) { day in
                        Text(Calendar.current.weekdaySymbols[day]).tag(day)
                    }
                }
            }
            HStack(spacing: 4) {
                Text("Time")
                Spacer()
                Picker("Hour", selection: $draft.hour) {
                    ForEach(0..<24, id: \.self) { Text(String(format: "%02d", $0)).tag($0) }
                }
                .labelsHidden()
                .pickerStyle(.menu)
                .frame(width: 65)
                Text(":")
                Picker("Minute", selection: $draft.minute) {
                    ForEach(0..<60, id: \.self) { Text(String(format: "%02d", $0)).tag($0) }
                }
                .labelsHidden()
                .pickerStyle(.menu)
                .frame(width: 65)
            }
            Text("Uses the Mac’s local time zone. A missed run is sent once when Alondra starts again.")
                .font(.caption).foregroundStyle(.secondary)
                .frame(maxWidth: 380, alignment: .leading)
                .fixedSize(horizontal: false, vertical: true)
            Toggle("Active", isOn: $draft.enabled)
        }
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
        saving = true
        defer { saving = false }
        do {
            _ = try await client.saveSchedule(draft, id: task?.id)
            await saved()
            dismiss()
        } catch { self.error = error.localizedDescription }
    }
}
