import AlondraShared
import SwiftUI

struct NewRequestSheet: View {
    let store: FleetStore
    @Environment(\.dismiss) private var dismiss
    @State private var text = ""
    @State private var attachments = Attachments()
    @State private var sending = false
    @State private var options: WorkOptionsModel?
    @FocusState private var focused: Bool

    private var trimmed: String { text.trimmingCharacters(in: .whitespacesAndNewlines) }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            VStack(alignment: .leading, spacing: 4) {
                Text("What do you want done?")
                    .font(.title3.weight(.semibold))
                Text("I’ll pass it to the orchestrator: it prepares the mission and launches the node.")
                    .foregroundStyle(.secondary)
            }
            TextEditor(text: $text)
                .font(.body)
                .scrollContentBackground(.hidden)
                .padding(8)
                .frame(minWidth: 520, minHeight: 180)
                .background(.background, in: .rect(cornerRadius: 8))
                .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(Color.primary.opacity(0.12)))
                .overlay(alignment: .topLeading) {
                    if text.isEmpty {
                        Text("For example: “Find out why Redraft is slow to open and propose a fix”")
                            .foregroundStyle(.tertiary)
                            .padding(.horizontal, 13)
                            .padding(.vertical, 8)
                            .allowsHitTesting(false)
                    }
                }
                .focused($focused)
                .modifier(DropsFiles(attachments: attachments, client: store.client))
            AttachmentStrip(attachments, client: store.client)
            if let options {
                WorkOptionsForm(model: options, client: store.client)
            }
            HStack {
                AttachFilesButton(attachments: attachments, client: store.client)
                Hint(text: String(localized: "⌘↩ sends · esc cancels"))
                Spacer()
                Button("Cancel", role: .cancel) { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Button {
                    send()
                } label: {
                    if sending {
                        ProgressView().controlSize(.small)
                    } else {
                        Text("Send")
                    }
                }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.return, modifiers: .command)
                .disabled(sending || (trimmed.isEmpty && attachments.isEmpty))
            }
        }
        .padding(22)
        .onAppear { focused = true }
        .task {
            let model = WorkOptionsModel(client: store.client)
            options = model
            await model.load()
        }
    }

    private func send() {
        guard !trimmed.isEmpty || !attachments.isEmpty else { return }
        let body = (options?.options ?? WorkOptions()).body(text: trimmed)
        sending = true
        Task {
            if await store.newRequest(body, attachments: attachments) {
                dismiss()
            }
            sending = false
        }
    }
}

private struct WorkOptionsForm: View {
    @Bindable var model: WorkOptionsModel
    let client: AlondraClient
    @State private var picking = false
    @State private var choosingProject = false

    var body: some View {
        Grid(alignment: .leading, horizontalSpacing: 12, verticalSpacing: 8) {
            GridRow {
                Text("Project")
                    .foregroundStyle(.secondary)
                    .gridColumnAlignment(.trailing)
                Button {
                    choosingProject = true
                } label: {
                    HStack(spacing: 4) {
                        Text(model.options.project ?? String(localized: "The orchestrator decides"))
                        Image(systemName: "chevron.up.chevron.down")
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                    }
                }
                .popover(isPresented: $choosingProject, arrowEdge: .bottom) {
                    ProjectPicker(projects: model.projects, current: model.options.project) { project in
                        choosingProject = false
                        Task { await model.choose(project: project) }
                    }
                }
            }
            GridRow {
                Text("Issue")
                    .foregroundStyle(.secondary)
                issueControl
            }
            ForEach(model.options.specs) { spec in
                GridRow {
                    Text(verbatim: spec.title)
                        .foregroundStyle(.secondary)
                    OptionControl(spec: spec, choice: choiceBinding(spec))
                }
            }
            if let error = model.options.error {
                GridRow {
                    Color.clear.gridCellUnsizedAxes([.horizontal, .vertical])
                    Label(error, systemImage: "exclamationmark.triangle")
                        .font(.caption)
                        .foregroundStyle(.orange)
                        .lineLimit(2)
                }
            }
        }
        .opacity(model.loaded ? 1 : 0.4)
        .animation(.default, value: model.loaded)
    }

    @ViewBuilder
    private var issueControl: some View {
        if let issue = model.options.issue {
            HStack(spacing: 6) {
                Label(issue, systemImage: "smallcircle.filled.circle")
                    .foregroundStyle(.green)
                Button {
                    Task { await model.choose(issue: nil) }
                } label: {
                    Image(systemName: "xmark.circle.fill")
                }
                .buttonStyle(.borderless)
                .help(Text("Remove the issue"))
                .accessibilityLabel(Text("Remove the issue"))
            }
        } else {
            Button("From an issue…") { picking = true }
                .popover(isPresented: $picking, arrowEdge: .bottom) {
                    IssuePicker(client: client) { issue in
                        picking = false
                        Task { await model.choose(issue: issue) }
                    }
                }
        }
    }

    private func choiceBinding(_ spec: OptionSpec) -> Binding<OptionChoice> {
        Binding(get: { model.options.current(spec) }, set: { model.options.set(spec, to: $0) })
    }
}

/// The option's current value; its menu lists every value with the line that explains it.
private struct OptionControl: View {
    let spec: OptionSpec
    @Binding var choice: OptionChoice

    var body: some View {
        Menu {
            ForEach(spec.choices, id: \.self) { value in
                Button {
                    choice = value
                } label: {
                    if value == choice {
                        Image(systemName: "checkmark")
                    }
                    Text(verbatim: spec.title(of: value))
                    if let description = spec.description(of: value) {
                        Text(verbatim: description)
                    }
                }
            }
        } label: {
            Text(verbatim: spec.title(of: choice))
        }
        .menuStyle(.button)
        .fixedSize()
    }
}

/// The projects, most recently used first, filtered as you type.
private struct ProjectPicker: View {
    let projects: [String]
    let current: String?
    let pick: (String?) -> Void
    @State private var query = ""

    private var shown: [String] { ProjectList.filter(projects, query: query) }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            TextField("Filter projects", text: $query)
                .textFieldStyle(.roundedBorder)
                .onSubmit { if let first = shown.first { pick(first) } }
            List {
                if query.isEmpty {
                    row(nil, title: String(localized: "The orchestrator decides"))
                }
                ForEach(shown, id: \.self) { row($0, title: $0) }
            }
            .listStyle(.plain)
            .overlay {
                if shown.isEmpty && !query.isEmpty {
                    Text("No project matches")
                        .foregroundStyle(.secondary)
                }
            }
        }
        .padding(12)
        .frame(width: 320, height: 340)
    }

    private func row(_ project: String?, title: String) -> some View {
        Button {
            pick(project)
        } label: {
            HStack {
                Text(verbatim: title)
                Spacer()
                if project == current {
                    Image(systemName: "checkmark")
                        .foregroundStyle(.tint)
                }
            }
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
    }
}

private struct IssuePicker: View {
    @State private var issues: IssuesModel
    let pick: (String) -> Void

    init(client: AlondraClient, pick: @escaping (String) -> Void) {
        _issues = State(initialValue: IssuesModel(client: client))
        self.pick = pick
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                TextField("Filter, or paste an issue link", text: $issues.query)
                    .textFieldStyle(.roundedBorder)
                    .onSubmit { if let first = issues.pasted ?? issues.visible.first?.reference { pick(first) } }
                Button {
                    Task { await issues.load(refresh: true) }
                } label: {
                    Image(systemName: "arrow.clockwise")
                }
                .help(Text("Search GitHub again"))
                .accessibilityLabel(Text("Search GitHub again"))
                .disabled(issues.loading)
            }
            if let error = issues.error {
                Label(error, systemImage: "exclamationmark.triangle")
                    .font(.caption)
                    .foregroundStyle(.orange)
                    .lineLimit(2)
            }
            List {
                if let pasted = issues.pasted {
                    Button {
                        pick(pasted)
                    } label: {
                        Label("Use \(pasted)", systemImage: "link")
                    }
                    .buttonStyle(.plain)
                }
                ForEach(issues.visible) { issue in
                    Button {
                        pick(issue.reference)
                    } label: {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(verbatim: issue.title)
                                .lineLimit(2)
                            Text(verbatim: ([issue.reference] + issue.labels).joined(separator: " · "))
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .contentShape(.rect)
                    }
                    .buttonStyle(.plain)
                }
            }
            .listStyle(.plain)
            .overlay {
                if issues.loading && issues.issues.isEmpty {
                    ProgressView()
                } else if issues.loaded && issues.visible.isEmpty && issues.pasted == nil {
                    Text("No open issues match")
                        .foregroundStyle(.secondary)
                }
            }
        }
        .padding(12)
        .frame(width: 440, height: 360)
        .task { await issues.load() }
    }
}
