import AlondraShared
import AlondraWorkspaceFeature
import ComposableArchitecture
import SwiftUI

struct ServiceRequestDetail: View {
    let request: DomainV2.Request
    let store: StoreOf<ServiceTasksFeature>
    let selectTask: (DomainV2.TaskId) -> Void

    private var confirmingWithdraw: Binding<Bool> {
        Binding(get: { store.withdrawingRequestID == request.id }, set: { if !$0 { store.send(.withdrawCancelled) } })
    }

    private var tasks: [DomainV2.Task] { store.taskBoard.tree(for: request.id)?.tasks ?? [] }
    private var defining: Bool { tasks.isEmpty && !request.closed }
    private var finished: Bool { request.closed || (!tasks.isEmpty && tasks.allSatisfy { $0.state.isFinished }) }

    var body: some View {
        ServiceTaskPage {
            VStack(alignment: .leading, spacing: 8) {
                if let project = store.presentation.project(of: request) {
                    Label(project, systemImage: "folder")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                }
                Text(store.presentation.title(of: request))
                    .font(.title.weight(.semibold))
                    .textSelection(.enabled)
                HStack(spacing: 8) {
                    StatusDot(color: defining ? .purple : finished ? .gray : .green, size: 9)
                    Text((defining || tasks.isEmpty ? store.presentation.definingLabel(of: request) : request.stateLabel).capitalizedFirst)
                    if let created = store.presentation.createdAt(of: request) {
                        Text(verbatim: "· \(String(localized: "\(ago(created)) ago"))")
                            .foregroundStyle(.secondary)
                    }
                }
                .font(.callout)
            }
            if defining {
                ServiceTaskSection(String(localized: "Right now")) {
                    Text(String(localized: "The orchestrator is splitting it into tasks."))
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.horizontal, 12)
                        .padding(.vertical, 10)
                        .background(Color.primary.opacity(0.04), in: .rect(cornerRadius: 10))
                }
            }
            if !tasks.isEmpty {
                ServiceTaskSection(String(localized: "Tasks")) {
                    VStack(alignment: .leading, spacing: 4) {
                        ForEach(tasks) { task in
                            Button { selectTask(task.id) } label: {
                                HStack(spacing: 6) {
                                    TaskDot(task: task)
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
            ServiceRequestText(request: request, createdAt: store.presentation.createdAt(of: request), taskCount: tasks.count)
        }
        .toolbar {
            if store.state.canWithdraw(request.id) {
                ToolbarItem {
                    Button(role: .destructive) {
                        store.send(.withdrawRequested(request.id))
                    } label: {
                        Label("Withdraw Request…", systemImage: "xmark.circle")
                    }
                    .disabled(store.isWithdrawing)
                    .help("It ends as done, without tasks, and the orchestrator can no longer define it.")
                }
            }
        }
        .confirmationDialog("Withdraw “\(store.presentation.title(of: request))”?", isPresented: confirmingWithdraw) {
            Button("Withdraw Request", role: .destructive) {
                store.send(.withdrawConfirmed)
            }
        } message: {
            Text("It ends as done, without tasks, and the orchestrator can no longer define it.")
        }
    }
}

private struct ServiceRequestText: View {
    let request: DomainV2.Request
    let createdAt: Int64?
    let taskCount: Int

    var body: some View {
        if case let text = request.body, !text.isEmpty {
            ServiceTaskSection(String(localized: "Request")) {
                VStack(alignment: .leading, spacing: 6) {
                    Text(text)
                        .textSelection(.enabled)
                    Text(verbatim: [createdAt.map { String(localized: "\(ago($0)) ago") }, String(localized: "\(taskCount) tasks")].compactMap { $0 }.joined(separator: " · "))
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
