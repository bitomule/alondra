import AlondraShared
import SwiftUI

/// A task: why it is where it is, the request behind it, its PRs in merge order, what it depends
/// on, its documents, options and history. Reopen and cancel live in the toolbar.
struct TaskDetail: View {
    let task: WorkTask
    let store: FleetStore
    let select: (Selection) -> Void

    @State private var details: TaskInFull?
    @State private var confirmingCancel = false
    @State private var acting = false

    var body: some View {
        TaskPage {
            header
            if let now = store.waitingReason(for: task) ?? store.now(for: task) {
                TaskSection(String(localized: "Right now")) {
                    Text(now)
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.horizontal, 12)
                        .padding(.vertical, 10)
                        .background(Color.primary.opacity(0.04), in: .rect(cornerRadius: 10))
                }
            }
            pending
            if let request = store.request(of: task) {
                RequestText(request: request, store: store)
            }
            pullRequests
            dependencies
            documents
            options
            timeline
            if store.taskActionsUnavailable {
                Label("This Alondra can’t cancel or reopen tasks yet.", systemImage: "info.circle")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
        }
        .toolbar {
            ToolbarItemGroup {
                Button {
                    act { await store.reopen(task) }
                } label: {
                    Label("Reopen", systemImage: "arrow.clockwise")
                }
                .disabled(!task.state.isFinished || store.taskActionsUnavailable || acting)
                .help("Reopen a finished or cancelled task")
                Button(role: .destructive) {
                    confirmingCancel = true
                } label: {
                    Label("Cancel Task…", systemImage: "xmark.circle")
                }
                .disabled(task.state.isFinished || store.taskActionsUnavailable || acting)
                .help("Its node stops working on it")
            }
        }
        .confirmationDialog("Cancel “\(task.title)”?", isPresented: $confirmingCancel) {
            Button("Cancel Task", role: .destructive) {
                act { await store.cancel(task) }
            }
        } message: {
            Text("Its node stops working on it. You can reopen it later.")
        }
        .task(id: task.historyKey) {
            details = await store.details(of: task, keeping: details)
        }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 14) {
                if let project = store.project(of: task) {
                    Label(project, systemImage: "folder")
                }
                if let request = store.request(of: task) {
                    Label(store.title(of: request), systemImage: "text.quote")
                        .lineLimit(1)
                }
            }
            .font(.callout)
            .foregroundStyle(.secondary)
            Text(task.title)
                .font(.title.weight(.semibold))
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
            HStack(spacing: 8) {
                StatusDot(color: task.color, size: 9)
                Text(task.state.label.capitalizedFirst)
                Text(verbatim: subtitle)
                    .foregroundStyle(.secondary)
            }
            .font(.callout)
        }
    }

    private var subtitle: String {
        let since = String(localized: "for \(ago(task.stateSince))")
        return ([since] + [store.node(of: task)?.displayName].compactMap(\.self)).map { "· \($0)" }.joined(separator: " ")
    }

    @ViewBuilder
    private var pending: some View {
        let items = store.items(forTask: task)
        if !items.isEmpty {
            TaskSection(String(localized: "Waiting for You")) {
                VStack(alignment: .leading, spacing: 2) {
                    ForEach(items) { item in
                        Button { select(.item(item.id)) } label: {
                            HStack {
                                InboxRow(item: item)
                                Spacer()
                                Image(systemName: "chevron.right")
                                    .font(.caption.weight(.semibold))
                                    .foregroundStyle(.tertiary)
                            }
                            .padding(.horizontal, 10)
                            .padding(.vertical, 4)
                            .contentShape(.rect)
                        }
                        .buttonStyle(.plain)
                    }
                }
                .background(Color.primary.opacity(0.04), in: .rect(cornerRadius: 10))
            }
        }
    }

    @ViewBuilder
    private var pullRequests: some View {
        let train = store.board.mergeOrder(around: task).flatMap { owner in store.pullRequests(of: owner).map { (owner: owner, pr: $0) } }
        if !train.isEmpty {
            TaskSection(String(localized: "Pull requests in merge order")) {
                VStack(spacing: 0) {
                    ForEach(Array(train.enumerated()), id: \.element.pr.id) { index, entry in
                        if index > 0 { Divider().padding(.horizontal, 12) }
                        PullRequestRow(order: index + 1, pr: entry.pr, owner: entry.owner, mine: entry.owner.id == task.id, store: store) {
                            select(.item("p\(entry.pr.id)"))
                        }
                    }
                }
                .background(Color.primary.opacity(0.04), in: .rect(cornerRadius: 10))
            }
        }
    }

    private var dependencies: some View {
        TaskSection(String(localized: "Dependencies")) {
            Grid(alignment: .leadingFirstTextBaseline, horizontalSpacing: 14, verticalSpacing: 8) {
                GridRow {
                    Text("Depends on").foregroundStyle(.secondary)
                    taskLinks(store.board.dependencies(of: task))
                }
                GridRow {
                    Text("Waiting on it").foregroundStyle(.secondary)
                    taskLinks(store.board.dependents(of: task))
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 12)
            .padding(.vertical, 10)
            .background(Color.primary.opacity(0.04), in: .rect(cornerRadius: 10))
        }
    }

    @ViewBuilder
    private func taskLinks(_ tasks: [WorkTask]) -> some View {
        if tasks.isEmpty {
            Text("none").foregroundStyle(.secondary)
        } else {
            VStack(alignment: .leading, spacing: 4) {
                ForEach(tasks) { other in
                    Button { select(.task(other.id)) } label: {
                        HStack(spacing: 6) {
                            StatusDot(color: other.color)
                            Text(other.title)
                            Text(verbatim: "· \(other.state.label)")
                                .foregroundStyle(.secondary)
                        }
                        .contentShape(.rect)
                    }
                    .buttonStyle(.plain)
                }
            }
        }
    }

    @ViewBuilder
    private var documents: some View {
        let reports = store.documents(of: task, in: details)
        if !reports.isEmpty {
            TaskSection(String(localized: "Documents")) {
                VStack(spacing: 0) {
                    ForEach(Array(reports.enumerated()), id: \.element.id) { index, report in
                        if index > 0 { Divider().padding(.horizontal, 12) }
                        HStack(spacing: 10) {
                            Image(systemName: "doc.text")
                                .foregroundStyle(.blue)
                            VStack(alignment: .leading, spacing: 1) {
                                Text(report.title).fontWeight(.medium)
                                Text(verbatim: "\(report.kindLabel) · \(store.isUnread(report) ? String(localized: "unread") : String(localized: "read")) · \(String(localized: "\(ago(report.createdAt)) ago"))")
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                            }
                            Spacer()
                            if store.isUnread(report) {
                                Button("Read") { select(.item("r\(report.id)")) }
                                    .controlSize(.small)
                            }
                        }
                        .padding(.horizontal, 12)
                        .padding(.vertical, 8)
                    }
                }
                .background(Color.primary.opacity(0.04), in: .rect(cornerRadius: 10))
            }
        }
    }

    private var options: some View {
        let alone = task.options?.mergesAlone ?? false
        return TaskSection(String(localized: "Options")) {
            Grid(alignment: .leading, horizontalSpacing: 14, verticalSpacing: 8) {
                GridRow {
                    Text("PR handling").foregroundStyle(.secondary)
                    Text(OptionSpec.prHandling(autoMerge: alone))
                }
                if let effort = task.options?.effort {
                    GridRow {
                        Text("Effort").foregroundStyle(.secondary)
                        Text(effort.capitalizedFirst)
                    }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 12)
            .padding(.vertical, 10)
            .background(Color.primary.opacity(0.04), in: .rect(cornerRadius: 10))
        }
    }

    private var history: [WorkTask.Transition]? { details?.task.history }

    @ViewBuilder
    private var timeline: some View {
        if let history, !history.isEmpty {
            TaskSection(String(localized: "History")) {
                Grid(alignment: .leadingFirstTextBaseline, horizontalSpacing: 10, verticalSpacing: 6) {
                    ForEach(Array(history.enumerated()), id: \.offset) { _, step in
                        GridRow {
                            Text(Date(timeIntervalSince1970: TimeInterval(step.at) / 1000), format: .dateTime.hour().minute())
                                .font(.system(.caption, design: .monospaced))
                                .foregroundStyle(.tertiary)
                            StatusDot(color: step.to.color)
                            (Text(step.to.label).fontWeight(.semibold) + Text(verbatim: " · \(step.summary)").foregroundStyle(.secondary))
                        }
                    }
                }
            }
        }
    }

    private func act(_ work: @escaping () async -> Bool) {
        acting = true
        Task {
            _ = await work()
            acting = false
        }
    }
}

/// A request still being split into tasks: what the user asked, and that it is on its way.
struct RequestDetail: View {
    let request: WorkRequest
    let store: FleetStore
    let select: (Selection) -> Void

    @State private var confirmingWithdraw = false
    @State private var acting = false

    var body: some View {
        TaskPage {
            VStack(alignment: .leading, spacing: 8) {
                if let project = store.project(of: request) {
                    Label(project, systemImage: "folder")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                }
                Text(store.title(of: request))
                    .font(.title.weight(.semibold))
                    .textSelection(.enabled)
                HStack(spacing: 8) {
                    StatusDot(color: request.state == .defining ? .purple : request.state == .done ? .gray : .green, size: 9)
                    Text(request.state.label.capitalizedFirst)
                    Text(verbatim: "· \(String(localized: "\(ago(request.createdAt)) ago"))")
                        .foregroundStyle(.secondary)
                }
                .font(.callout)
            }
            if request.state == .defining {
                TaskSection(String(localized: "Right now")) {
                    Text("The orchestrator is splitting it into tasks.")
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.horizontal, 12)
                        .padding(.vertical, 10)
                        .background(Color.primary.opacity(0.04), in: .rect(cornerRadius: 10))
                }
            }
            let tasks = store.board.tree(for: request.id)?.tasks ?? []
            if !tasks.isEmpty {
                TaskSection(String(localized: "Tasks")) {
                    VStack(alignment: .leading, spacing: 4) {
                        ForEach(tasks) { task in
                            Button { select(.task(task.id)) } label: {
                                HStack(spacing: 6) {
                                    StatusDot(color: task.color)
                                    Text(task.title)
                                    Text(verbatim: "· \(task.state.label)")
                                        .foregroundStyle(.secondary)
                                }
                                .contentShape(.rect)
                            }
                            .buttonStyle(.plain)
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 10)
                    .background(Color.primary.opacity(0.04), in: .rect(cornerRadius: 10))
                }
            }
            RequestText(request: request, store: store)
        }
        .toolbar {
            if request.canWithdraw {
                ToolbarItem {
                    Button(role: .destructive) {
                        confirmingWithdraw = true
                    } label: {
                        Label("Withdraw Request…", systemImage: "xmark.circle")
                    }
                    .disabled(acting)
                    .help("It ends as done, without tasks, and the orchestrator can no longer define it.")
                }
            }
        }
        .confirmationDialog("Withdraw “\(store.title(of: request))”?", isPresented: $confirmingWithdraw) {
            Button("Withdraw Request", role: .destructive) {
                acting = true
                Task {
                    await store.withdraw(request)
                    acting = false
                }
            }
        } message: {
            Text("It ends as done, without tasks, and the orchestrator can no longer define it.")
        }
    }
}

private struct TaskPage<Content: View>: View {
    @ViewBuilder let content: Content

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 22) {
                content
            }
            .padding(.horizontal, 44)
            .padding(.top, 26)
            .padding(.bottom, 28)
            .frame(maxWidth: 800, alignment: .leading)
            .frame(maxWidth: .infinity)
        }
    }
}

private struct TaskSection<Content: View>: View {
    let title: String
    @ViewBuilder let content: Content

    init(_ title: String, @ViewBuilder content: () -> Content) {
        self.title = title
        self.content = content()
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(title)
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(.secondary)
            content
        }
    }
}

private struct RequestText: View {
    let request: WorkRequest
    let store: FleetStore

    var body: some View {
        if let text = request.text, !text.isEmpty {
            TaskSection(String(localized: "Request")) {
                VStack(alignment: .leading, spacing: 6) {
                    Text(text)
                        .textSelection(.enabled)
                    Text(verbatim: "\(String(localized: "\(ago(request.createdAt)) ago")) · \(String(localized: "\(store.board.tree(for: request.id)?.rows.count ?? 0) tasks"))")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, 12)
                .padding(.vertical, 10)
                .background(Color.primary.opacity(0.04), in: .rect(cornerRadius: 10))
            }
        }
    }
}

private struct PullRequestRow: View {
    let order: Int
    let pr: PullRequest
    let owner: WorkTask
    let mine: Bool
    let store: FleetStore
    let open: () -> Void

    var body: some View {
        HStack(spacing: 10) {
            Text(order, format: .number)
                .font(.caption.weight(.semibold))
                .monospacedDigit()
                .frame(width: 18, height: 18)
                .background(Color.primary.opacity(0.08), in: .circle)
            Image(systemName: "arrow.triangle.pull")
                .foregroundStyle(.green)
            VStack(alignment: .leading, spacing: 1) {
                HStack(spacing: 6) {
                    Text(verbatim: "#\(pr.number) \(pr.title)")
                        .fontWeight(.medium)
                    if mine {
                        Text("this task")
                            .font(.caption2.weight(.semibold))
                            .foregroundStyle(.blue)
                            .padding(.horizontal, 6)
                            .padding(.vertical, 1)
                            .background(.blue.opacity(0.14), in: .capsule)
                    }
                }
                Text(verbatim: pr.stackedOn.map { "\($0) · \(pr.summary)" } ?? pr.summary)
                    .font(.system(.caption, design: .monospaced))
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            Spacer(minLength: 8)
            if let reason = pr.mergeBlockedReason {
                Text(reason)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            } else {
                StatusDot(color: owner.color)
                Text(verbatim: pr.progress(mergesAlone: owner.mergesAlone))
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            Button("Merge…", action: open)
                .controlSize(.small)
                .disabled(pr.mergeBlockedReason != nil || !store.inbox.contains { $0.id == "p\(pr.id)" })
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
    }
}
