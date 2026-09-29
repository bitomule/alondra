import AlondraAttachmentsFeature
import AlondraWorkspaceFeature
import AlondraTasksFeature
import ComposableArchitecture
import AlondraShared
import SwiftUI

struct ServiceTaskDetailView: View {
    let store: StoreOf<TaskDetailFeature>
    let presentation: ServiceTaskPresentation
    let openTask: (DomainV2.TaskId) -> Void
    let focusInbox: (String) -> Void
    private var fleet: ServiceTaskPresentation { presentation }

    var body: some View {
        let task = store.task
        Group {
            if let task {
                content(task)
            } else {
                ContentUnavailableView("This task is gone", systemImage: "questionmark.circle")
            }
        }
        .background(DawnBackground(glow: task?.tint ?? Palette.dawn))
        .navigationBarTitleDisplayMode(.inline)
        .toolbar(.hidden, for: .tabBar)
        .onDisappear { store.send(.disappeared) }
        .attachmentDownloadPresentation(store: store.scope(state: \.downloads, action: \.downloads))
        .confirmationDialog("Cancel this task?", isPresented: Binding(get: { store.confirmation == .cancel }, set: { if !$0 { store.send(.cancelConfirmationDismissed) } }), titleVisibility: .visible) {
            Button("Cancel Task", role: .destructive) {
                store.send(.cancelConfirmed)
            }
        } message: {
            Text("Its node stops working on it. You can reopen it later.")
        }
        .sensoryFeedback(.success, trigger: store.successfulActionCount)
    }

    private func content(_ task: DomainV2.Task) -> some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 24) {
                header(task)
                pending(task)
                request(task)
                pullRequests(task)
                dependencies(task)
                documents(task)
                options(task)
                timeline
                actions(task)
            }
            .padding(.horizontal, 20)
            .padding(.top, 4)
            .padding(.bottom, 32)
        }
    }

    private func header(_ task: DomainV2.Task) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 10) {
                Glyph(task: task)
                Text(task.state.label.prefix(1).uppercased() + task.state.label.dropFirst())
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(task.tint)
                if let since = fleet.stateSince(of: task) {
                    Text("since \(relative(since))")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                }
            }
            Text(task.title)
                .font(.display(.largeTitle, weight: .bold))
                .foregroundStyle(Palette.ink)
                .fixedSize(horizontal: false, vertical: true)
            if let origin = origin(task) {
                Text(origin)
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
            }
            if let node = fleet.node(task.agentId) {
                Text(node.displayName)
                    .font(.mono(.caption))
                    .foregroundStyle(.secondary)
            }
            if let now = fleet.waitingReason(for: task) ?? fleet.now(for: task) {
                Label {
                    Text(now)
                } icon: {
                    Image(systemName: "clock")
                }
                .font(.mono(.footnote))
                .foregroundStyle(Palette.sky)
                .lineLimit(3)
            }
        }
    }

    private func origin(_ task: DomainV2.Task) -> String? {
        let request = fleet.request(of: task).map { String(localized: "from “\(fleet.title(of: $0))”") }
        let parts = [fleet.project(of: task), request].compactMap(\.self)
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }

    @ViewBuilder
    private func pending(_ task: DomainV2.Task) -> some View {
        let items = fleet.items(forTask: task)
        if !items.isEmpty {
            DetailSection(title: String(localized: "Waiting for You"), symbol: "tray.full") {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(items, id: \.id) { item in
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
                .padding(16)
            }
        }
    }

    @ViewBuilder
    private func request(_ task: DomainV2.Task) -> some View {
        if let request = fleet.request(of: task), case let text = request.body, !text.isEmpty {
            DetailSection(title: String(localized: "Request"), symbol: "text.quote") {
                VStack(alignment: .leading, spacing: 10) {
                    Text(text)
                        .font(.body)
                        .foregroundStyle(Palette.ink)
                        .fixedSize(horizontal: false, vertical: true)
                        .textSelection(.enabled)
                    Text(verbatim: [fleet.createdAt(of: request).map(relative), String(localized: "\(fleet.board.tree(for: request.id)?.rows.count ?? 0) tasks")].compactMap { $0 }.joined(separator: " · "))
                        .font(.caption)
                        .foregroundStyle(.tertiary)
                }
                .padding(16)
            }
        }
    }

    @ViewBuilder
    private func pullRequests(_ task: DomainV2.Task) -> some View {
        let train = fleet.board.mergeOrder(around: task).flatMap { owner in fleet.pullRequests(of: owner).map { (owner: owner, pr: $0) } }
        if !train.isEmpty {
            DetailSection(title: String(localized: "PRs in merge order"), symbol: "arrow.triangle.merge") {
                VStack(spacing: 0) {
                    ForEach(Array(train.enumerated()), id: \.element.pr.id) { index, entry in
                        ServicePullRequestLine(order: index + 1, pr: entry.pr, observation: fleet.observation(of: entry.pr), mine: entry.owner.id == task.id, waitingItem: fleet.inboxItem(for: entry.pr.id)?.id, open: focusInbox)
                            .overlay(alignment: .top) {
                                if index > 0 { Palette.hairline.frame(height: 1).padding(.leading, 16) }
                            }
                    }
                }
            }
        }
    }

    private func dependencies(_ task: DomainV2.Task) -> some View {
        DetailSection(title: String(localized: "Dependencies"), symbol: "link") {
            VStack(spacing: 0) {
                dependencyRow(String(localized: "Depends on"), fleet.board.dependencies(of: task))
                Palette.hairline.frame(height: 1).padding(.leading, 16)
                dependencyRow(String(localized: "Waiting on it"), fleet.board.dependents(of: task))
            }
        }
    }

    private func dependencyRow(_ title: String, _ tasks: [DomainV2.Task]) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 12) {
            Text(title)
                .foregroundStyle(Palette.ink)
            Spacer(minLength: 8)
            if tasks.isEmpty {
                Text("none")
                    .foregroundStyle(.secondary)
            } else {
                VStack(alignment: .trailing, spacing: 6) {
                    ForEach(tasks, id: \.id) { other in
                        Button { openTask(other.id) } label: {
                            HStack(spacing: 6) {
                                Glyph(task: other, size: .tiny)
                                Text(other.title)
                                    .fontWeight(.medium)
                                    .foregroundStyle(other.tint)
                                    .multilineTextAlignment(.trailing)
                            }
                        }
                        .buttonStyle(PressStyle())
                    }
                }
            }
        }
        .font(.body)
        .padding(.horizontal, 16)
        .padding(.vertical, 13)
    }

    @ViewBuilder
    private func documents(_ task: DomainV2.Task) -> some View {
        let reports = fleet.documents(of: task)
        if !reports.isEmpty {
            DetailSection(title: String(localized: "Documents"), symbol: "doc.text") {
                VStack(spacing: 0) {
                    ForEach(Array(reports.enumerated()), id: \.element.id) { index, report in
                        documentRow(report, of: task)
                            .overlay(alignment: .top) {
                                if index > 0 { Palette.hairline.frame(height: 1).padding(.leading, 16) }
                            }
                    }
                }
            }
        }
    }

    private func documentRow(_ report: TaskDocument, of task: DomainV2.Task) -> some View {
        let waiting = fleet.reviewItem(for: report.id, of: task.id)
        let unread = waiting != nil
        return Button {
            if let waiting { focusInbox(waiting.id) }
        } label: {
            HStack(spacing: 12) {
                Image(systemName: "doc.text")
                    .font(.title3)
                    .foregroundStyle(Palette.sky)
                    .frame(width: 22)
                VStack(alignment: .leading, spacing: 2) {
                    Text(report.document.title)
                        .font(.body.weight(.semibold))
                        .foregroundStyle(Palette.ink)
                        .multilineTextAlignment(.leading)
                    Text(unread ? String(localized: "unread") : String(localized: "read"))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Spacer(minLength: 0)
                if unread {
                    Image(systemName: "chevron.right")
                        .font(.caption.weight(.bold))
                        .foregroundStyle(.tertiary)
                }
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 13)
            .contentShape(.rect)
        }
        .buttonStyle(PressStyle())
        .disabled(!unread)
    }

    private func options(_ task: DomainV2.Task) -> some View {
        DetailSection(title: String(localized: "Options"), symbol: "slider.horizontal.3") {
            VStack(spacing: 0) {
                HStack {
                    Text("PR handling")
                    Spacer()
                    Text(OptionSpec.prHandling(autoMerge: !task.reviewPrs))
                        .foregroundStyle(.secondary)
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 13)
                if let effort = fleet.runtime?.task(task.id)?.effort, !effort.isEmpty {
                    Palette.hairline.frame(height: 1).padding(.leading, 16)
                    HStack {
                        Text("Effort")
                        Spacer()
                        Text(effort.prefix(1).uppercased() + effort.dropFirst())
                            .foregroundStyle(.secondary)
                    }
                    .padding(.horizontal, 16)
                    .padding(.vertical, 13)
                }
            }
        }
    }

    private var history: [DomainV2.TaskTransition]? { store.task.flatMap { fleet.runtime?.task($0.id)?.history } }

    @ViewBuilder
    private var timeline: some View {
        if let history, !history.isEmpty {
            DetailSection(title: String(localized: "History"), symbol: "clock.arrow.circlepath") {
                VStack(alignment: .leading, spacing: 0) {
                    ForEach(Array(history.enumerated()), id: \.offset) { index, step in
                        HistoryLine(step: step, first: index == 0, last: index == history.count - 1)
                    }
                }
                .padding(.vertical, 6)
            }
        }
    }

    private func actions(_ task: DomainV2.Task) -> some View {
        VStack(spacing: 10) {
            ServiceActionButton(title: String(localized: "Cancel Task"), symbol: "xmark.circle", tint: Palette.rose,
                         enabled: !task.state.isFinished && !store.isActing) {
                store.send(.cancelTapped)
            }
            ServiceActionButton(title: String(localized: "Reopen"), symbol: "arrow.clockwise", tint: Palette.sky,
                         enabled: task.state.isFinished && !store.isActing) {
                store.send(.reopenTapped)
            }
            ServiceTaskOutcome(outcome: store.mutationOutcome)
        }
    }
}

private struct ServicePullRequestLine: View {
    let order: Int
    let pr: DomainV2.PullRequest
    let observation: PullRequest?
    let mine: Bool

    let waitingItem: String?
    let open: (String) -> Void

    var body: some View {
        Button {
            if let waitingItem { open(waitingItem) }
        } label: {
            HStack(alignment: .top, spacing: 10) {
                Text(order, format: .number)
                    .font(.caption.weight(.bold).monospacedDigit())
                    .frame(width: 24, height: 24)
                    .background(Palette.hairline, in: .circle)
                VStack(alignment: .leading, spacing: 4) {
                    HStack(spacing: 6) {
                        Text(verbatim: "#\(pr.number) \(observation?.title ?? pr.repository)")
                            .font(.body.weight(.semibold))
                            .foregroundStyle(Palette.ink)
                            .multilineTextAlignment(.leading)
                        if mine {
                            Text("this task")
                                .font(.caption2.weight(.bold))
                                .textCase(.uppercase)
                                .foregroundStyle(Palette.dawn)
                                .padding(.horizontal, 7)
                                .padding(.vertical, 3)
                                .background(Palette.dawn.opacity(0.14), in: .capsule)
                                .fixedSize()
                        }
                    }
                    Text(observation?.summary ?? "\(pr.repository) #\(pr.number)")
                        .font(.mono(.caption))
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.leading)
                    if let checks = observation?.checks, !checks.isEmpty {
                        FlowChecks(checks: checks)
                    }
                    if let stacked = observation?.stackedOn {
                        Label(stacked, systemImage: "square.stack.3d.up")
                            .font(.footnote.weight(.medium))
                            .foregroundStyle(.secondary)
                    }
                }
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 13)
            .contentShape(.rect)
        }
        .buttonStyle(PressStyle())
        .disabled(waitingItem == nil)
    }
}

private struct FlowChecks: View {
    let checks: [PRCheck]

    var body: some View {
        ViewThatFits(in: .horizontal) {
            HStack(spacing: 12) { marks }
            VStack(alignment: .leading, spacing: 4) { marks }
        }
        .font(.footnote)
    }

    @ViewBuilder
    private var marks: some View {
        ForEach(checks, id: \.self) { check in
            Label(check.name, systemImage: check.passed ? "checkmark" : check.failed ? "xmark" : "clock")
                .foregroundStyle(check.passed ? Palette.sage : check.failed ? Palette.rose : Palette.dawn)
                .lineLimit(1)
        }
    }
}

private struct HistoryLine: View {
    let step: DomainV2.TaskTransition
    let first: Bool
    let last: Bool

    var body: some View {
        let tint = step.to.tint
        HStack(alignment: .top, spacing: 10) {
            Text(Date(millis: step.at), format: .dateTime.hour().minute())
                .font(.mono(.caption))
                .foregroundStyle(.tertiary)
                .frame(width: 44, alignment: .leading)
            Glyph(symbol: step.to.symbol, tint: tint, size: .tiny)
                .background(Palette.card, in: .circle)
            (Text(step.to.label).fontWeight(.semibold).foregroundStyle(tint) + Text(verbatim: " · \(step.summary)"))
                .font(.subheadline)
                .foregroundStyle(Palette.ink.opacity(0.86))
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 9)
        .background(alignment: .topLeading) {
            GeometryReader { proxy in
                Path { path in
                    let x: CGFloat = 16 + 44 + 10 + 9
                    path.move(to: CGPoint(x: x, y: first ? 18 : 0))
                    path.addLine(to: CGPoint(x: x, y: last ? 18 : proxy.size.height))
                }
                .stroke(Palette.hairline, lineWidth: 2)
            }
        }
    }
}


struct ServiceTaskOutcome: View {
    let outcome: TaskMutationOutcome?
    var body: some View {
        switch outcome {
        case .unknown: Text("We couldn’t confirm the result. Try again; it won’t be applied twice.").foregroundStyle(Palette.ember)
        case .rejected: EmptyView() // Root presents the original failure toast.
        case .obsolete: Text("The connection changed. Open this task again.").foregroundStyle(.secondary)
        case .confirmed, nil: EmptyView()
        }
    }
}
