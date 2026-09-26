import AlondraAttachmentsFeature
import AlondraContracts
import AlondraDomain
import AlondraNewWorkFeature
import AlondraShared
import ComposableArchitecture
import SwiftUI

struct ServiceNewRequestView: View {
    let store: StoreOf<NewRequestFeature>
    @FocusState private var focused: Bool
    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 24) {
                VStack(alignment: .leading, spacing: 6) {
                    Text("New").font(.display(.largeTitle, weight: .bold))
                    orchestratorStatus
                }
                editor.disabled(store.isSubmitting || store.submissionUncertain)
                optionsRow
                if let error = store.submissionError {
                    Text(error).font(.footnote).foregroundStyle(Palette.rose)
                }
                if !store.sentMessages.isEmpty { history }
            }.padding(20)
        }
        .scrollDismissesKeyboard(.interactively)
        .background(DawnBackground(glow: Palette.ember))
        .navigationBarTitleDisplayMode(.inline)
        .safeAreaBar(edge: .bottom) { sendBar }
        .sensoryFeedback(.success, trigger: store.successfulSubmissionCount)
        .onChange(of: store.successfulSubmissionCount) { focused = false }
        .task { await store.send(.appeared).finish() }
        .sheet(isPresented: Binding(get: { store.optionsPresented }, set: { store.send(.optionsPresented($0)) })) {
            ServiceWorkOptionsSheet(store: store).presentationDetents([.medium, .large])
        }
    }
    private var optionsRow: some View {
        Button {
            store.send(.optionsPresented(true))
        } label: {
            HStack(spacing: 12) {
                Image(systemName: "slider.horizontal.3")
                    .foregroundStyle(Palette.ember)
                VStack(alignment: .leading, spacing: 2) {
                    Text(store.selection.project ?? String(localized: "The orchestrator picks the project"))
                        .font(.subheadline.weight(.semibold))
                    Text(([store.selection.issue].compactMap { $0 } + [store.selection.summary]).joined(separator: " · "))
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
        .disabled(!store.catalogIsCurrent)
        .accessibilityHint(Text("Changes the options for this request"))
    }

    private var orchestratorStatus: some View {
        Group {
            if let name = store.orchestratorName {
                Label("\(name) receives it, prepares the mission and launches the node.", systemImage: "point.3.connected.trianglepath.dotted")
                    .foregroundStyle(.secondary)
            } else if store.hasSnapshot {
                Label("No orchestrator is connected to Alondra, so nothing can be sent right now.", systemImage: "exclamationmark.triangle")
                    .foregroundStyle(Palette.rose)
            }
        }
        .font(.subheadline)
    }

    private var editor: some View {
        ZStack(alignment: .topLeading) {
            if store.text.isEmpty {
                Text("What do you want done? Describe it as you would to someone on the team.")
                    .font(.display(.title3, weight: .regular))
                    .foregroundStyle(.tertiary)
                    .padding(.horizontal, 5)
                    .padding(.vertical, 8)
                    .allowsHitTesting(false)
            }
            VStack(alignment: .leading, spacing: 10) {
                TextEditor(text: Binding(get: { store.text }, set: { store.send(.textChanged($0)) }))
                    .font(.display(.title3, weight: .regular))
                    .scrollContentBackground(.hidden)
                    .focused($focused)
                    .frame(minHeight: 240)
                HStack(alignment: .center, spacing: 8) {
                    ServiceAttachButton(store: store.scope(state: \.attachmentPicker, action: \.attachmentPicker))
                    ServiceAttachmentStrip(store: store.scope(state: \.attachmentPicker, action: \.attachmentPicker))
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
            ForEach(store.sentMessages.prefix(8)) { message in
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
            store.send(.submitTapped)
        } label: {
            Label(store.isSubmitting ? String(localized: "Sending…") : String(localized: "Ask the Orchestrator"), systemImage: "paperplane.fill")
                .font(.headline)
                .frame(maxWidth: .infinity)
        }
        .buttonStyle(.glassProminent)
        .buttonBorderShape(.capsule)
        .controlSize(.large)
        .tint(Palette.ember)
        .disabled(!store.submitEnabled)
        .padding(.horizontal, 20)
        .padding(.vertical, 8)
    }

}

private struct ServiceWorkOptionsSheet: View {
    let store: StoreOf<NewRequestFeature>
    var body: some View {
        NavigationStack {
            Form {
                Section {
                    NavigationLink {
                        ServiceProjectPicker(store: store)
                    } label: {
                        LabeledContent("Project", value: store.selection.project ?? String(localized: "The orchestrator picks"))
                    }
                }
                Section("Issue") {
                    if let issue = store.selection.issue {
                        HStack {
                            Label(issue, systemImage: "smallcircle.filled.circle").foregroundStyle(Palette.sage)
                            Spacer()
                            Button("Remove", role: .destructive) { store.send(.issueSelected(nil)) }.buttonStyle(.borderless)
                        }
                    } else {
                        NavigationLink("Choose an issue") { ServiceIssuePicker(store: store) }
                    }
                }
                Section {
                    ForEach(store.selection.specs) { spec in
                        if spec.kind == .toggle {
                            Toggle(spec.title, isOn: Binding(get: { store.selection.current(spec) == .toggle(true) }, set: { store.send(.optionSelected(spec.id, .toggle($0))) }))
                        } else {
                            Picker(spec.title, selection: Binding(get: { store.selection.current(spec) }, set: { store.send(.optionSelected(spec.id, $0)) })) {
                                ForEach(spec.choices, id: \.self) { value in
                                    VStack(alignment: .leading, spacing: 2) {
                                        Text(verbatim: spec.title(of: value))
                                        if let description = spec.description(of: value) {
                                            Text(verbatim: description).font(.caption).foregroundStyle(.secondary)
                                        }
                                    }.tag(value)
                                }
                            }.pickerStyle(.navigationLink)
                        }
                    }
                } footer: {
                    if let error = store.optionsError { Label(error, systemImage: "exclamationmark.triangle") }
                }
            }
            .disabled(store.isSubmitting || store.submissionUncertain)
            .navigationTitle("Options")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) {
                Button("Done") { store.send(.optionsPresented(false)) }
            } }
        }
    }
}
private struct ServiceProjectPicker: View {
    let store: StoreOf<NewRequestFeature>
    @Environment(\.dismiss) private var dismiss
    var body: some View {
        let shown = ProjectList.filter(store.projects, query: store.projectQuery)
        List {
            if store.projectQuery.isEmpty { row(nil, title: String(localized: "The orchestrator picks")) }
            ForEach(shown, id: \.self) { row($0, title: $0) }
        }
        .overlay { if shown.isEmpty && !store.projectQuery.isEmpty { ContentUnavailableView.search(text: store.projectQuery) } }
        .searchable(text: Binding(get: { store.projectQuery }, set: { store.send(.projectQueryChanged($0)) }), prompt: Text("Filter projects"))
        .navigationTitle("Project")
    }
    private func row(_ project: String?, title: String) -> some View {
        Button { store.send(.projectSelected(project)); dismiss() } label: {
            HStack {
                Text(verbatim: title).foregroundStyle(.primary)
                Spacer()
                if project == store.selection.project { Image(systemName: "checkmark").foregroundStyle(Palette.ember) }
            }
        }
    }
}
private struct ServiceIssuePicker: View {
    let store: StoreOf<NewRequestFeature>
    @Environment(\.dismiss) private var dismiss
    var body: some View {
        List {
            if let error = store.issuesError { Label(error, systemImage: "exclamationmark.triangle").font(.footnote).foregroundStyle(Palette.rose) }
            if let pasted = store.pastedIssue {
                Button { store.send(.issueSelected(pasted)); dismiss() } label: { Label("Use \(pasted)", systemImage: "link") }
            }
            ForEach(store.visibleIssues) { issue in
                Button { store.send(.issueSelected(issue.reference)); dismiss() } label: {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(verbatim: issue.title).foregroundStyle(.primary).lineLimit(3)
                        Text(verbatim: ([issue.reference] + issue.labels).joined(separator: " · ")).font(.caption).foregroundStyle(.secondary)
                    }
                }
            }
        }
        .overlay {
            if store.isLoadingIssues && store.issues.isEmpty { ProgressView() }
            else if store.issuesLoaded && store.visibleIssues.isEmpty && store.pastedIssue == nil {
                ContentUnavailableView("No open issues match", systemImage: "magnifyingglass")
            }
        }
        .searchable(text: Binding(get: { store.issueQuery }, set: { store.send(.issueQueryChanged($0)) }), prompt: Text("Filter, or paste an issue link"))
        .refreshable { await store.send(.issuesRefreshRequested).finish() }
        .navigationTitle("Issues")
        .task { await store.send(.issuesOpened).finish() }
    }
}
