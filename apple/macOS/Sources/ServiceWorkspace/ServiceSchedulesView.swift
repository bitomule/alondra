import AlondraWorkspaceFeature
import AlondraShared
import AlondraTasksFeature
import ComposableArchitecture
import SwiftUI

struct ServiceSchedulesView: View {
    @Bindable var store: StoreOf<ServiceTasksFeature>
    var body: some View {
        NavigationStack {
            List {
                if store.schedules.isLoading { ProgressView("Loading schedules") }
                if let error = store.schedules.error { Text(error).foregroundStyle(.red) }
                ServiceTaskOutcome(outcome: store.schedules.lastMutationOutcome)
                if store.schedules.tasks.isEmpty && !store.schedules.isLoading { Text("No scheduled tasks") }
                ForEach(store.schedules.tasks) { schedule in
                    VStack(alignment: .leading, spacing: 8) {
                        Button(schedule.title) { store.send(.schedules(.editTapped(schedule.id))) }.font(.headline)
                        Text(schedule.text).lineLimit(3)
                        Text(schedule.frequency).font(.caption)
                        if let next = schedule.nextRun { Text("Next: \(Date(timeIntervalSince1970: Double(next) / 1000), format: .dateTime)").font(.caption) }
                        if let error = schedule.lastError { Text(error).foregroundStyle(.red) }
                        Toggle("Active", isOn: Binding(get: { schedule.enabled }, set: { store.send(.schedules(.enabledChanged(schedule.id, $0))) }))
                        HStack {
                            Button("Run", systemImage: "play") { store.send(.schedules(.runTapped(schedule.id))) }
                            Spacer()
                            Button("Delete", systemImage: "trash", role: .destructive) { store.send(.deleteSchedule(schedule.id)) }
                        }
                    }
                    .buttonStyle(.borderless)
                    .disabled(store.schedules.inFlightTargets.contains(.schedule(schedule.id)) || store.schedules.uncertainByTarget[.schedule(schedule.id)] != nil)
                }
            }
            .navigationTitle("Scheduled Tasks")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Done") { store.send(.showSchedules(false)) } }
                ToolbarItem(placement: .primaryAction) { Button("Add Schedule", systemImage: "plus") { store.send(.schedules(.createTapped)) } }
            }
            .refreshable { await store.send(.schedules(.refresh)).finish() }
            .task { await store.send(.schedules(.appeared)).finish() }
            .sheet(isPresented: Binding(get: { store.schedules.draft != nil }, set: { if !$0 { store.send(.schedules(.editorDismissed)) } })) {
                ServiceScheduleEditor(store: store.scope(state: \.schedules, action: \.schedules)).frame(minWidth: 560, minHeight: 520)
            }
            .confirmationDialog("Delete scheduled task?", isPresented: Binding(get: { store.deletingScheduleID != nil }, set: { if !$0 { store.send(.deleteSchedule(nil)) } })) {
                Button("Delete", role: .destructive) { store.send(.confirmDeleteSchedule) }
            } message: { Text("Future runs will stop. Existing requests stay in Tasks.") }
        }
    }
}
private struct ServiceScheduleEditor: View {
    let store: StoreOf<ScheduleFeature>
    private var draft: Binding<ScheduledTaskDraft> {
        Binding(get: { store.draft ?? ScheduledTaskDraft() }, set: { store.send(.draftChanged($0)) })
    }
    var body: some View {
        NavigationStack {
            Form {
                Section("Mission") {
                    TextField("Name", text: draft.title)
                    TextEditor(text: draft.text).frame(minHeight: 112).accessibilityLabel("Mission instructions")
                    Picker("Project", selection: draft.project) {
                        Text("Projects folder").tag(nil as String?)
                        ForEach(store.projects, id: \.self) { Text($0).tag(Optional($0)) }
                    }
                }
                ServiceScheduleTiming(draft: draft)
                Section("Launch options") {
                    if store.optionsLoading { ProgressView("Loading options") }
                    ForEach(store.optionSpecs, id: \.id) { spec in ServiceScheduleOption(spec: spec, draft: draft) }
                }
                Section("Advanced") {
                    TextField("Disallowed tools (one pattern per line)", text: Binding(get: { draft.wrappedValue.disallowedTools.joined(separator: "\n") }, set: { draft.wrappedValue.disallowedTools = $0.components(separatedBy: .newlines) }), axis: .vertical)
                }
                if let error = store.error { Text(error).foregroundStyle(.red) }
                ServiceTaskOutcome(outcome: store.lastMutationOutcome)
            }
            .navigationTitle(store.editingID == nil ? "New Schedule" : "Edit Schedule")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { store.send(.editorDismissed) } }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save") { store.send(.saveTapped) }
                        .disabled(store.isSaving || !store.optionsReady || store.uncertainByTarget[store.editingID.map(ScheduleCommandTarget.schedule) ?? .new] != nil)
                }
            }
        }
    }
}
private struct ServiceScheduleTiming: View {
    @Binding var draft: ScheduledTaskDraft
    var body: some View {
        Section("When") {
            Picker("Repeat", selection: $draft.frequency) {
                Text("Every day").tag("daily")
                Text("Every week").tag("weekly")
                Text("Every few hours").tag("every_hours")
                Text("Every few days").tag("every_days")
                Text("Every few months").tag("every_months")
                Text("Once").tag("once")
            }
            if draft.frequency == "weekly" {
                Picker("Day", selection: $draft.weekday) {
                    ForEach(0..<7, id: \.self) { Text(Calendar.current.weekdaySymbols[$0]).tag(Optional($0)) }
                }
            }
            if draft.frequency.hasPrefix("every_") {
                Stepper("Interval: \(draft.interval ?? 1)", value: Binding(get: { draft.interval ?? 1 }, set: { draft.interval = $0 }), in: 1...365)
            }
            if draft.frequency == "once" || draft.frequency.hasPrefix("every_") {
                DatePicker("Starts", selection: Binding(get: { Date(timeIntervalSince1970: Double(draft.startAt ?? 0) / 1000) }, set: { draft.startAt = Int64($0.timeIntervalSince1970 * 1000) }))
            } else {
                Stepper("Hour: \(draft.hour)", value: $draft.hour, in: 0...23)
                Stepper("Minute: \(draft.minute)", value: $draft.minute, in: 0...59)
            }
            Toggle("Active", isOn: $draft.enabled)
            Text("Uses this device’s local time zone. Missed runs are sent once when Alondra starts again.").font(.caption).foregroundStyle(.secondary)
        }
    }
}
private struct ServiceScheduleOption: View {
    let spec: OptionSpec
    @Binding var draft: ScheduledTaskDraft
    var body: some View {
        if spec.kind == .toggle {
            Toggle(spec.title, isOn: Binding(get: { (draft.options[spec.id] ?? spec.default) == .toggle(true) }, set: { draft.options[spec.id] = .toggle($0) }))
        } else {
            Picker(spec.title, selection: Binding(get: { draft.options[spec.id] ?? spec.default }, set: { draft.options[spec.id] = $0 })) {
                ForEach(spec.values, id: \.id) { value in Text(spec.title(of: .value(value.id))).tag(OptionChoice.value(value.id)) }
            }
        }
    }
}
