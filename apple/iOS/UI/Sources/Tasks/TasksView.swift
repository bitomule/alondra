import AlondraContract
import AlondraDesign
import AlondraFeatures
import ComposableArchitecture
import SwiftUI

/// The Tasks tab: each request with its task tree, then the finished and archived work folded
/// under its count.
public struct TasksView: View {
    @Bindable var store: StoreOf<TasksFeature>

    public init(store: StoreOf<TasksFeature>) { self.store = store }

    public var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 0) {
                header
                content
            }
            .padding(.bottom, 24)
            .animation(.smooth, value: store.board?.active.requests.map(\.id))
        }
        .background(DawnBackground())
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button("Archive Completed", systemImage: "archivebox") { store.send(.archiveCompleted) }
                    .disabled(store.board?.archiveCompleted == nil)
            }
        }
        .refreshable { await store.send(.refresh).finish() }
        .task { await store.send(.task).finish() }
        .actionDialogs(store.scope(state: \.actions, action: \.actions))
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Tasks")
                .font(.display(.largeTitle, weight: .bold))
                .foregroundStyle(Palette.ink)
            if let board = store.board {
                TasksSummary(board: board)
                    .font(.subheadline.weight(.medium))
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
        }
        .padding(.horizontal, 20)
    }

    @ViewBuilder private var content: some View {
        if let board = store.board {
            if let schedules = board.schedules {
                ScheduledTasksEntry(schedules: schedules) { store.send(.open($0)) }
            }
            if board.isEmpty {
                ContentUnavailableView {
                    Label("No tasks yet", systemImage: "checklist")
                } description: {
                    Text("Ask for something in New: it shows up here at once, and the orchestrator splits it into tasks.")
                }
                .padding(.top, 60)
            } else {
                ForEach(board.active.requests) { request in
                    RequestSection(request: request, open: { store.send(.open($0)) },
                                   perform: { store.send(.perform($0)) })
                }
                if board.active.more != nil {
                    MoreButton(fold: board.active, loading: store.loadingMore.contains(board.active.id)) {
                        store.send(.showMore(board.active.id))
                    }
                }
                FoldedRequests(fold: board.finished, expanded: store.expanded.contains(board.finished.id),
                               loading: store.loadingMore.contains(board.finished.id),
                               toggle: { store.send(.foldToggled(board.finished.id)) },
                               more: { store.send(.showMore(board.finished.id)) },
                               open: { store.send(.open($0)) })
                FoldedRequests(fold: board.archived, expanded: store.expanded.contains(board.archived.id),
                               loading: store.loadingMore.contains(board.archived.id),
                               toggle: { store.send(.foldToggled(board.archived.id)) },
                               more: { store.send(.showMore(board.archived.id)) },
                               open: { store.send(.open($0)) })
            }
        } else {
            ScreenPhaseView(phase: store.screen.phase, missing: "Tasks are unavailable") { store.send(.task) }
        }
    }
}

/// "3 requests · 1 needs you · 2 running", counted from the tasks on the screen.
private struct TasksSummary: View {
    let board: TasksBoard

    private var parts: [LocalizedStringKey] {
        let tasks = board.active.requests.flatMap(\.tasks)
        let needsYou = tasks.count { $0.icon == .action }
        let inReview = tasks.count { $0.icon == .pullRequest }
        let running = tasks.count { $0.icon == .mission }
        var parts: [LocalizedStringKey] = ["\(board.active.count) requests"]
        if needsYou > 0 { parts.append("\(needsYou) need you") }
        if inReview > 0 { parts.append("\(inReview) in review") }
        if running > 0 { parts.append("\(running) running") }
        return parts
    }

    var body: some View {
        HStack(spacing: 5) {
            ForEach(Array(parts.enumerated()), id: \.offset) { index, part in
                if index > 0 { Text(verbatim: "·") }
                Text(part)
            }
        }
        .accessibilityElement(children: .combine)
    }
}

private struct ScheduledTasksEntry: View {
    let schedules: TasksBoard.Schedules
    let open: (AlondraContract.Link) -> Void

    var body: some View {
        Button {
            if let link = schedules.link { open(link) }
        } label: {
            HStack(spacing: 12) {
                Glyph(symbol: "calendar.badge.clock", tint: Palette.sky)
                Text(verbatim: schedules.title)
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(Palette.ink)
                Spacer(minLength: 8)
                if schedules.count > 0 {
                    Text(schedules.count, format: .number)
                        .font(.caption.weight(.bold).monospacedDigit())
                        .foregroundStyle(.secondary)
                }
                Image(systemName: "chevron.right")
                    .font(.caption.weight(.bold))
                    .foregroundStyle(.tertiary)
                    .accessibilityHidden(true)
            }
            .padding(.leading, 16)
            .padding(.trailing, 14)
            .padding(.vertical, 12)
            .background(Palette.card.opacity(0.85), in: .rect(cornerRadius: 24, style: .continuous))
            .contentShape(.rect)
        }
        .buttonStyle(PressStyle())
        .padding(.horizontal, 16)
        .padding(.top, 20)
    }
}

struct MoreButton: View {
    let fold: TasksBoard.Fold
    let loading: Bool
    let action: () -> Void

    var body: some View {
        if let more = fold.more {
            Button(action: action) {
                Text("Show \(Int(more.remaining)) more")
                    .font(.footnote.weight(.semibold))
                    .foregroundStyle(Palette.dawn)
                    .frame(maxWidth: .infinity, minHeight: 36)
            }
            .disabled(loading)
            .padding(.horizontal, 16)
            .padding(.top, 8)
        }
    }
}
