import AlondraAttachmentsFeature
import AlondraFleetFeature
import AlondraWorkspaceFeature
import AlondraShared
import ComposableArchitecture
import SwiftUI

struct ServiceNodeView: View {
    let store: StoreOf<NodeDetailFeature>
    let presentation: ServiceTaskPresentation
    let openTask: (String) -> Void
    let focusInbox: (String) -> Void
    @Environment(\.dismiss) private var dismiss
    @FocusState private var focused: Bool
    private var fleet: ServiceTaskPresentation { presentation }
    private var session: String { store.sessionID }
    private var threads: [NodeActivity] { store.threads }
    var body: some View {
        Group {
            if let node = store.node {
                content(node)
            } else {
                ContentUnavailableView("This node is gone", systemImage: "questionmark.circle")
            }
        }
        .task { await store.send(.appeared).finish() }
        .onDisappear { store.send(.disappeared) }
        .background(DawnBackground(glow: store.node?.glyphTint ?? Palette.dawn))
        .navigationBarTitleDisplayMode(.inline)
        .toolbar(.hidden, for: .tabBar)
        .alert("Continue without repeating?", isPresented: Binding(get: { store.acknowledgeConfirmationRunID != nil }, set: { if !$0 { store.send(.acknowledgeCancelled) } })) {
            Button("Continue") { store.send(.acknowledgeConfirmed) }
            Button("Cancel", role: .cancel) { store.send(.acknowledgeCancelled) }
        } message: {
            Text("The interrupted batch will not run again. Inspect any partial effects first. Later messages remain queued and can continue.")
        }
        .alert("Stop the current intervention?", isPresented: Binding(get: { store.cancelConfirmationRunID != nil }, set: { if !$0 { store.send(.cancelCancelled) } })) {
            Button("Stop", role: .destructive) { store.send(.cancelConfirmed) }
            Button("Cancel", role: .cancel) { store.send(.cancelCancelled) }
        } message: {
            Text("Completed actions cannot be undone. Alondra keeps the conversation and pending messages; interrupted work may need review before retrying.")
        }
        .alert("Retry the interrupted work?", isPresented: Binding(get: { store.retryConfirmationRunID != nil }, set: { if !$0 { store.send(.retryCancelled) } })) {
            Button("Retry") { store.send(.retryConfirmed) }
            Button("Cancel", role: .cancel) { store.send(.retryCancelled) }
        } message: {
            Text("Some actions may already have completed. Check the activity and tasks before retrying to avoid repeating work.")
        }
        .toolbar {
            if store.node?.isOrchestrator == false {
                ToolbarItem(placement: .topBarTrailing) {
                    Menu {
                        Button(role: .destructive) {
                            store.send(.closeRequested)
                        } label: {
                            Label("Close Node", systemImage: "xmark.bin")
                        }
                    } label: {
                        Label("More", systemImage: "ellipsis")
                    }
                }
            }
        }
        .closeConfirmation(Binding(get: {
            guard store.showCloseConfirmation, let node = store.node else { return nil }
            return CloseRequest(session: node.sessionId, name: node.displayName)
        }, set: { if $0 == nil { store.send(.closeCancelled) } })) { _ in
            store.send(.closeConfirmed)
        }
        .sensoryFeedback(.success, trigger: store.sentCount)
        .onChange(of: store.sentCount) { _, _ in focused = false }
        .onChange(of: store.shouldDismiss) { _, dismissing in if dismissing { dismiss() } }

    }

    private func content(_ node: Node) -> some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 22) {
                VStack(alignment: .leading, spacing: 10) {
                    HStack(spacing: 10) {
                        StateGlyph(node: node)
                        Text(node.fleetLabel.prefix(1).uppercased() + node.fleetLabel.dropFirst())
                            .font(.subheadline.weight(.semibold))
                            .foregroundStyle(node.glyphTint)
                        Text("since \(relative(node.stateSince))")
                            .font(.subheadline)
                            .foregroundStyle(.secondary)
                    }
                    Text(node.displayName)
                        .font(.display(.largeTitle, weight: .bold))
                        .fixedSize(horizontal: false, vertical: true)
                    if !node.isOrchestrator, let cwd = node.cwd {
                        Text(cwd.replacing(/^\/Users\/[^\/]+\//, with: "~/"))
                            .font(.mono(.caption))
                            .foregroundStyle(.secondary)
                            .lineLimit(2)
                    }
                }

                if let runtime = node.orchestratorRuntime {
                    Text(runtime.availabilityDescription).foregroundStyle(.secondary)
                    if runtime.queueDepth != nil { Text(runtime.queueLabel).font(.subheadline) }
                    if let error = runtime.lastError { Text(error).foregroundStyle(Palette.rose).textSelection(.enabled) }
                    if runtime.canRetry == true {
                        Button("Retry Orchestrator…") { store.send(.retryRequested) }
                            .disabled(!store.canRetryOrchestrator)
                            Button("Continue without repeating…") { store.send(.acknowledgeRequested) }
                                .disabled(!store.canAcknowledgeOrchestrator)
                    }
                    ServiceNodeOutcome(outcome: store.retryOutcome)
                        ServiceNodeOutcome(outcome: store.acknowledgeOutcome)
                    if runtime.canCancel == true {
                        Button("Stop Orchestrator…", role: .destructive) { store.send(.cancelRequested) }
                            .disabled(!store.canCancelOrchestrator)
                    }
                    ServiceNodeOutcome(outcome: store.cancelOutcome)
                }

                let tasks = store.tasks
                if !tasks.isEmpty {
                    section(String(localized: "Tasks"), symbol: "checklist") {
                        VStack(alignment: .leading, spacing: 12) {
                            ForEach(tasks) { task in
                                Button { openTask(task.id) } label: {
                                    HStack(spacing: 10) {
                                        Glyph(task: task, size: .small)
                                        VStack(alignment: .leading, spacing: 2) {
                                            Text(task.fleetRowTitle(nodeDisplayName: node.displayName))
                                                .font(.subheadline.weight(.semibold))
                                                .foregroundStyle(Palette.ink)
                                                .multilineTextAlignment(.leading)
                                            Text(taskLine(task))
                                                .font(.caption)
                                                .foregroundStyle(.secondary)
                                                .lineLimit(1)
                                        }
                                        Spacer(minLength: 0)
                                        Image(systemName: "chevron.right")
                                            .font(.caption.weight(.bold))
                                            .foregroundStyle(.tertiary)
                                    }
                                    .contentShape(.rect)
                                }
                                .buttonStyle(PressStyle())
                            }
                        }
                    }
                }

                if let error = store.threadsError { Text(error).foregroundStyle(Palette.rose) }
                if store.isStale { Text("Connection interrupted. Actions are paused until the fleet refreshes.").foregroundStyle(Palette.ember) }
                if !threads.isEmpty {
                    section(String(localized: "Right now"), symbol: "wrench.and.screwdriver") {
                        VStack(alignment: .leading, spacing: 12) {
                            ForEach(Array(threads.enumerated()), id: \.offset) { _, thread in
                                ThreadLine(activity: thread, task: thread.task.flatMap(fleet.board.task))
                            }
                        }
                    }
                }

                if !node.running.isEmpty {
                    section(BackgroundItem.sectionTitle, symbol: "square.stack.3d.up") {
                        VStack(alignment: .leading, spacing: 12) {
                            ForEach(node.running, id: \.self) { item in
                                BackgroundRow(item: item)
                            }
                        }
                    }
                }

                if let needs = node.daemonNeeds {
                    section(String(localized: "Needs"), symbol: "hand.raised") {
                        Text(needs).font(.body)
                    }
                }

                let pending = fleet.pending(for: session)
                if !pending.isEmpty {
                    section(String(localized: "Waiting for You"), symbol: "tray.full") {
                        VStack(alignment: .leading, spacing: 8) {
                            ForEach(pending) { item in
                                Button { focusInbox(item.id) } label: {
                                    HStack(spacing: 10) {
                                        KindBadge(kind: Kind(item))
                                        Text(item.title)
                                            .font(.subheadline.weight(.medium))
                                            .foregroundStyle(Palette.ink)
                                            .lineLimit(2)
                                            .multilineTextAlignment(.leading)
                                        Spacer(minLength: 0)
                                        Image(systemName: "chevron.right")
                                            .font(.caption.weight(.bold))
                                            .foregroundStyle(.tertiary)
                                    }
                                    .contentShape(.rect)
                                }
                                .buttonStyle(PressStyle())
                                .accessibilityHint("Opens in Waiting for You")
                            }
                        }
                    }
                }

                if let last = node.lastMessage, !last.isEmpty {
                    section(String(localized: "What it said last"), symbol: "text.quote", trailing: node.lastMessageAt.map(relative)) {
                        VStack(alignment: .leading, spacing: 10) {
                            ForEach(MarkdownParser.segments(from: last)) { segment in
                                MarkdownSegmentView(segment: segment)
                                    .frame(maxWidth: .infinity, alignment: .leading)
                            }
                        }
                    }
                }
            }
            .padding(20)
        }
        .scrollDismissesKeyboard(.interactively)
        .safeAreaBar(edge: .bottom) { composer(node) }
    }

    private func taskLine(_ task: WorkTask) -> String {
        guard let request = fleet.request(of: task) else { return task.state.label }
        return "\(task.state.label) · \(String(localized: "from “\(fleet.title(of: request))”"))"
    }

    private func section<Content: View>(_ title: String, symbol: String, trailing: String? = nil, @ViewBuilder content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Label(title, systemImage: symbol)
                    .font(.footnote.weight(.bold))
                    .textCase(.uppercase)
                    .kerning(0.6)
                    .foregroundStyle(.secondary)
                Spacer()
                if let trailing {
                    Text(trailing).font(.caption).foregroundStyle(.tertiary)
                }
            }
            content()
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(16)
                .background(Palette.card.opacity(0.9), in: .rect(cornerRadius: 20, style: .continuous))
        }
    }

    private func composer(_ node: Node) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            ServiceAttachmentStrip(store: store.scope(state: \.attachmentPicker, action: \.attachmentPicker))
                .disabled(store.isSendInFlight || store.sendOutcome?.uncertainID != nil)
            ServiceNodeOutcome(outcome: store.sendOutcome)
            if case .unknown = store.closeOutcome {
                ServiceNodeOutcome(outcome: store.closeOutcome)
            }
            messageBox(node)
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 8)
    }

    private func messageBox(_ node: Node) -> some View {
        HStack(alignment: .bottom, spacing: 10) {
            HStack(alignment: .bottom, spacing: 4) {
                ServiceAttachButton(store: store.scope(state: \.attachmentPicker, action: \.attachmentPicker))
                    .disabled(store.isSendInFlight || store.sendOutcome?.uncertainID != nil)
                    .padding(.leading, 6)
                    .padding(.bottom, 4)
                TextField("Write to \(node.displayName)…", text: Binding(get: { store.message }, set: { store.send(.messageChanged($0)) }), axis: .vertical)
                    .lineLimit(1...5)
                    .focused($focused)
                    .padding(.trailing, 16)
                    .padding(.vertical, 12)
            }
            .glassEffect(.regular.interactive(), in: .rect(cornerRadius: 22))
            Button {
                store.send(.sendTapped)
            } label: {
                Image(systemName: store.isSendInFlight ? "ellipsis" : "arrow.up")
                    .font(.body.weight(.bold))
                    .frame(width: 26, height: 26)
                    .symbolEffect(.variableColor.iterative, isActive: store.isSendInFlight)
            }
            .buttonStyle(.glassProminent)
            .buttonBorderShape(.circle)
            .tint(Palette.dawn)
            .disabled(!store.canSend)
            .accessibilityLabel("Send")
        }
    }

}

/// One thread of a node: the main one or a subagent, with the task it works on.
private struct ThreadLine: View {
    let activity: NodeActivity
    let task: WorkTask?

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: activity.isMainThread ? "terminal" : "person.2")
                .font(.footnote)
                .foregroundStyle(.secondary)
                .frame(width: 20)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 3) {
                HStack(alignment: .firstTextBaseline, spacing: 4) {
                    Text(activity.thread)
                        .font(.subheadline.weight(.semibold))
                    if !activity.isMainThread, let task {
                        Text("· subagent of “\(task.title)”")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                    }
                    Spacer(minLength: 4)
                    Text(relative(activity.since))
                        .font(.caption)
                        .foregroundStyle(.tertiary)
                }
                if let doing = activity.doing {
                    Text(doing)
                        .font(.mono(.footnote))
                        .foregroundStyle(Palette.sky)
                        .lineLimit(2)
                }
            }
        }
    }
}

struct ServiceNodeOutcome: View {
    let outcome: NodeMutationOutcome?
    var body: some View {
        switch outcome {
        case .confirmed, .rejected, nil: EmptyView()
        case .unknown: Text("The result is unknown. Your draft has been kept while the service reconciles it.").foregroundStyle(Palette.ember)
        case .obsolete: Text("The connection changed. Reopen the node.")
        }
    }
}
