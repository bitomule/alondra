import AlondraShared
import SwiftUI

struct NewRequestView: View {
    @Environment(FleetStore.self) private var fleet
    @State private var text = ""
    @State private var attachments = Attachments()
    @State private var sending = false
    @State private var sentCount = 0
    @State private var options: WorkOptionsModel?
    @State private var showingOptions = false
    @FocusState private var focused: Bool

    private var trimmed: String { text.trimmingCharacters(in: .whitespacesAndNewlines) }

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 24) {
                    VStack(alignment: .leading, spacing: 6) {
                        Text("New")
                            .font(.display(.largeTitle, weight: .bold))
                        orchestratorStatus
                    }
                    editor
                    if let options {
                        optionsRow(options)
                    }
                    if !fleet.sentToOrchestrator.isEmpty {
                        history
                    }
                }
                .padding(20)
            }
            .scrollDismissesKeyboard(.interactively)
            .background(DawnBackground(glow: Palette.ember))
            .navigationBarTitleDisplayMode(.inline)
            .safeAreaBar(edge: .bottom) { sendBar }
            .sensoryFeedback(.success, trigger: sentCount)
            .task {
                let model = WorkOptionsModel(client: fleet.client)
                options = model
                await model.load()
            }
            .sheet(isPresented: $showingOptions) {
                if let options {
                    WorkOptionsSheet(model: options, client: fleet.client)
                        .presentationDetents([.medium, .large])
                }
            }
        }
    }

    private func optionsRow(_ model: WorkOptionsModel) -> some View {
        Button {
            showingOptions = true
        } label: {
            HStack(spacing: 12) {
                Image(systemName: "slider.horizontal.3")
                    .foregroundStyle(Palette.ember)
                VStack(alignment: .leading, spacing: 2) {
                    Text(model.options.project ?? String(localized: "The orchestrator picks the project"))
                        .font(.subheadline.weight(.semibold))
                    Text(([model.options.issue].compactMap { $0 } + [model.options.summary]).joined(separator: " · "))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                        .multilineTextAlignment(.leading)
                }
                Spacer(minLength: 0)
                Image(systemName: "chevron.right")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(.tertiary)
            }
            .padding(14)
            .background(Palette.card.opacity(0.8), in: .rect(cornerRadius: 18, style: .continuous))
        }
        .buttonStyle(.plain)
        .disabled(!model.loaded)
        .accessibilityHint(Text("Changes the options for this request"))
    }

    private var orchestratorStatus: some View {
        Group {
            if let orchestrator = fleet.orchestrator {
                Label("\(orchestrator.displayName) receives it, prepares the mission and launches the node.", systemImage: "point.3.connected.trianglepath.dotted")
                    .foregroundStyle(.secondary)
            } else if fleet.overview != nil {
                Label("No orchestrator is connected to Alondra, so nothing can be sent right now.", systemImage: "exclamationmark.triangle")
                    .foregroundStyle(Palette.rose)
            }
        }
        .font(.subheadline)
    }

    private var editor: some View {
        ZStack(alignment: .topLeading) {
            if text.isEmpty {
                Text("What do you want done? Describe it as you would to someone on the team.")
                    .font(.display(.title3, weight: .regular))
                    .foregroundStyle(.tertiary)
                    .padding(.horizontal, 5)
                    .padding(.vertical, 8)
                    .allowsHitTesting(false)
            }
            VStack(alignment: .leading, spacing: 10) {
                TextEditor(text: $text)
                    .font(.display(.title3, weight: .regular))
                    .scrollContentBackground(.hidden)
                    .focused($focused)
                    .frame(minHeight: 240)
                HStack(alignment: .center, spacing: 8) {
                    AttachButton(attachments: attachments)
                    AttachmentStrip(attachments, client: fleet.client)
                }
            }
        }
        .padding(16)
        .background(Palette.card, in: .rect(cornerRadius: 28, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 28, style: .continuous).strokeBorder(focused ? Palette.ember.opacity(0.5) : Palette.hairline))
        .shadow(color: .black.opacity(0.12), radius: 18, y: 10)
        .animation(.smooth(duration: 0.25), value: focused)
    }

    private var history: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Sent")
                .font(.footnote.weight(.bold))
                .textCase(.uppercase)
                .kerning(0.6)
                .foregroundStyle(.secondary)
            ForEach(fleet.sentToOrchestrator.prefix(8)) { message in
                VStack(alignment: .leading, spacing: 8) {
                    Text(requestText(message.text))
                        .font(.subheadline)
                        .lineLimit(4)
                    HStack(spacing: 6) {
                        Image(systemName: message.deliveredAt != nil ? "checkmark.circle.fill" : message.handedAt != nil ? "arrow.right.circle" : "clock")
                        Text(message.deliveredAt != nil ? String(localized: "Delivered") : message.handedAt != nil ? String(localized: "On its way") : String(localized: "Queued"))
                        Text(verbatim: "·")
                        Text(relative(message.createdAt))
                    }
                    .font(.caption.weight(.medium))
                    .foregroundStyle(message.deliveredAt != nil ? Palette.sage : .secondary)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(14)
                .background(Palette.card.opacity(0.8), in: .rect(cornerRadius: 18, style: .continuous))
                .transition(.move(edge: .top).combined(with: .opacity))
            }
        }
    }

    private func requestText(_ raw: String) -> String {
        guard raw.hasPrefix("[Alondra] "), let colon = raw.range(of: ": ") else { return raw }
        return String(raw[colon.upperBound...].prefix { $0 != "\n" })
    }

    private var sendBar: some View {
        Button {
            send()
        } label: {
            Label(sending ? String(localized: "Sending…") : String(localized: "Ask the Orchestrator"), systemImage: "paperplane.fill")
                .font(.headline)
                .frame(maxWidth: .infinity)
        }
        .buttonStyle(.glassProminent)
        .buttonBorderShape(.capsule)
        .controlSize(.large)
        .tint(Palette.ember)
        .disabled(trimmed.isEmpty || sending || fleet.orchestrator == nil)
        .padding(.horizontal, 20)
        .padding(.vertical, 8)
    }

    private func send() {
        let body = (options?.options ?? WorkOptions()).body(text: trimmed)
        sending = true
        Task {
            let ok = await fleet.perform(.newRequest) { [attachments] client in
                var body = body
                body.attachments = try await attachments.storedIDs(using: client)
                try await client.newRequest(body)
            }
            sending = false
            if ok {
                withAnimation(.spring(duration: 0.4)) { text = "" }
                attachments = Attachments()
                focused = false
                sentCount += 1
                fleet.notice = Notice(String(localized: "Sent to the orchestrator"))
            }
        }
    }
}

private struct WorkOptionsSheet: View {
    @Bindable var model: WorkOptionsModel
    let client: AlondraClient
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    NavigationLink {
                        ProjectPickerList(projects: model.projects, current: model.options.project) { project in
                            Task { await model.choose(project: project) }
                        }
                    } label: {
                        LabeledContent("Project", value: model.options.project ?? String(localized: "The orchestrator picks"))
                    }
                }
                Section("Issue") {
                    if let issue = model.options.issue {
                        HStack {
                            Label(issue, systemImage: "smallcircle.filled.circle")
                                .foregroundStyle(Palette.sage)
                            Spacer()
                            Button("Remove", role: .destructive) {
                                Task { await model.choose(issue: nil) }
                            }
                            .buttonStyle(.borderless)
                        }
                    } else {
                        NavigationLink("Choose an issue") {
                            IssuePickerList(client: client) { issue in
                                Task { await model.choose(issue: issue) }
                            }
                        }
                    }
                }
                Section {
                    ForEach(model.options.specs) { spec in
                        switch spec.kind {
                        case .toggle:
                            Toggle(spec.title, isOn: Binding(get: { model.options.current(spec) == .toggle(true) }, set: { model.options.set(spec, to: .toggle($0)) }))
                        case .choice:
                            Picker(spec.title, selection: Binding(get: { model.options.current(spec) }, set: { model.options.set(spec, to: $0) })) {
                                ForEach(spec.choices, id: \.self) { value in
                                    VStack(alignment: .leading, spacing: 2) {
                                        Text(verbatim: spec.title(of: value))
                                        if let description = spec.description(of: value) {
                                            Text(verbatim: description)
                                                .font(.caption)
                                                .foregroundStyle(.secondary)
                                        }
                                    }
                                    .tag(value)
                                }
                            }
                            .pickerStyle(.navigationLink)
                        }
                    }
                } footer: {
                    if let error = model.options.error {
                        Label(error, systemImage: "exclamationmark.triangle")
                    }
                }
            }
            .navigationTitle("Options")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
        }
    }

}

private struct IssuePickerList: View {
    @State private var issues: IssuesModel
    @Environment(\.dismiss) private var dismiss
    let pick: (String) -> Void

    init(client: AlondraClient, pick: @escaping (String) -> Void) {
        _issues = State(initialValue: IssuesModel(client: client))
        self.pick = pick
    }

    var body: some View {
        List {
            if let error = issues.error {
                Label(error, systemImage: "exclamationmark.triangle")
                    .font(.footnote)
                    .foregroundStyle(Palette.rose)
            }
            if let pasted = issues.pasted {
                Button {
                    choose(pasted)
                } label: {
                    Label("Use \(pasted)", systemImage: "link")
                }
            }
            ForEach(issues.visible) { issue in
                Button {
                    choose(issue.reference)
                } label: {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(verbatim: issue.title)
                            .foregroundStyle(.primary)
                            .lineLimit(3)
                        Text(verbatim: ([issue.reference] + issue.labels).joined(separator: " · "))
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
            }
        }
        .overlay {
            if issues.loading && issues.issues.isEmpty {
                ProgressView()
            } else if issues.loaded && issues.visible.isEmpty && issues.pasted == nil {
                ContentUnavailableView("No open issues match", systemImage: "magnifyingglass")
            }
        }
        .searchable(text: $issues.query, prompt: Text("Filter, or paste an issue link"))
        .refreshable { await issues.load(refresh: true) }
        .navigationTitle("Issues")
        .task { await issues.load() }
    }

    private func choose(_ reference: String) {
        pick(reference)
        dismiss()
    }
}

/// The projects, most recently used first, filtered as you type.
private struct ProjectPickerList: View {
    let projects: [String]
    let current: String?
    let pick: (String?) -> Void
    @State private var query = ""
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        let shown = ProjectList.filter(projects, query: query)
        List {
            if query.isEmpty {
                row(nil, title: String(localized: "The orchestrator picks"))
            }
            ForEach(shown, id: \.self) { row($0, title: $0) }
        }
        .overlay {
            if shown.isEmpty && !query.isEmpty {
                ContentUnavailableView.search(text: query)
            }
        }
        .searchable(text: $query, prompt: Text("Filter projects"))
        .navigationTitle("Project")
    }

    private func row(_ project: String?, title: String) -> some View {
        Button {
            pick(project)
            dismiss()
        } label: {
            HStack {
                Text(verbatim: title)
                    .foregroundStyle(.primary)
                Spacer()
                if project == current {
                    Image(systemName: "checkmark")
                        .foregroundStyle(Palette.ember)
                }
            }
        }
    }
}
