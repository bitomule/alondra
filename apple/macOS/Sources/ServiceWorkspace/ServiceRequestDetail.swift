import AlondraShared
import AlondraWorkspaceFeature
import ComposableArchitecture
import SwiftUI

struct ServiceRequestDetail: View {
    let request: WorkRequest
    let store: StoreOf<ServiceTasksFeature>
    let selectTask: (String) -> Void

    private var confirmingWithdraw: Binding<Bool> {
        Binding(get: { store.withdrawingRequestID == request.id }, set: { if !$0 { store.send(.withdrawCancelled) } })
    }

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
                    StatusDot(color: request.state == .defining ? .purple : request.state == .done ? .gray : .green, size: 9)
                    Text(request.state.label.capitalizedFirst)
                    Text(verbatim: "· \(String(localized: "\(ago(request.createdAt)) ago"))")
                        .foregroundStyle(.secondary)
                }
                .font(.callout)
            }
            if request.state == .defining {
                ServiceTaskSection(String(localized: "Right now")) {
                    Text(
                        request.isLeftDefining()
                            ? String(localized: "The orchestrator left it without tasks and did not close it: withdraw it if it is no longer needed.")
                            : String(localized: "The orchestrator is splitting it into tasks.")
                    )
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.horizontal, 12)
                        .padding(.vertical, 10)
                        .background(Color.primary.opacity(0.04), in: .rect(cornerRadius: 10))
                }
            }
            let tasks = store.taskBoard.tree(for: request.id)?.tasks ?? []
            if !tasks.isEmpty {
                ServiceTaskSection(String(localized: "Tasks")) {
                    VStack(alignment: .leading, spacing: 4) {
                        ForEach(tasks) { task in
                            Button { selectTask(task.id) } label: {
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
            ServiceRequestText(request: request, board: store.taskBoard)
        }
        .toolbar {
            if request.canWithdraw {
                ToolbarItem {
                    Button(role: .destructive) {
                        store.send(.withdrawRequested(request.id))
                    } label: {
                        Label("Withdraw Request…", systemImage: "xmark.circle")
                    }
                    .disabled(store.isWithdrawing || store.withdrawOutcome?.uncertainID != nil)
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
    let request: WorkRequest
    let board: TaskBoard

    var body: some View {
        if let text = request.text, !text.isEmpty {
            ServiceTaskSection(String(localized: "Request")) {
                VStack(alignment: .leading, spacing: 6) {
                    Text(text)
                        .textSelection(.enabled)
                    Text(verbatim: "\(String(localized: "\(ago(request.createdAt)) ago")) · \(String(localized: "\(board.tree(for: request.id)?.rows.count ?? 0) tasks"))")
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
