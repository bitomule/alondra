import AlondraContract
import AlondraDesign
import AlondraFeatures
import ComposableArchitecture
import SwiftUI

/// The Tasks tab: each request with its task tree, then the completed work folded under its count.
public struct TasksView: View {
    @Bindable var store: StoreOf<TasksFeature>
    @Environment(\.connectionState) private var connection

    public init(store: StoreOf<TasksFeature>) { self.store = store }

    public var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 0) {
                header
                content
            }
            .padding(.bottom, 24)
            .animation(.smooth, value: store.board)
        }
        .background(DawnBackground())
        .navigationBarTitleDisplayMode(.inline)
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
                ConnectionLine(ConnectionIndicators(connection, hasSavedData: true).line, alert: Palette.rose) {
                    TasksSummary(board: board).foregroundStyle(.secondary)
                }
                .font(.subheadline.weight(.medium))
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
                    Text("Ask for something in New: it shows up here at once, split into tasks.")
                }
                .padding(.top, 60)
            } else {
                if board.active.blocks.isEmpty {
                    ForEach(board.active.requests) { request in
                        RequestSection(request: request, openChains: store.openChains, open: { store.send(.open($0)) },
                                       toggleChain: { store.send(.chainToggled($0), animation: .snappy) },
                                       perform: { store.send(.perform($0)) })
                    }
                } else {
                    ForEach(board.active.blocks) { block in
                        TaskBlockSection(block: block, openChains: store.openChains, open: { store.send(.open($0)) },
                                         toggleChain: { store.send(.chainToggled($0), animation: .snappy) },
                                         perform: { store.send(.perform($0)) })
                    }
                }
                if board.active.more != nil {
                    MoreButton(fold: board.active, loading: store.loadingMore.contains(board.active.id)) {
                        store.send(.showMore(board.active.id))
                    }
                }
                FoldedRequests(fold: board.finished, expanded: store.expanded.contains(board.finished.id),
                               loading: store.loadingMore.contains(board.finished.id),
                               clearable: board.clearCompleted != nil,
                               toggle: { store.send(.foldToggled(board.finished.id)) },
                               clear: { store.send(.clearCompleted) },
                               more: { store.send(.showMore(board.finished.id)) },
                               open: { store.send(.open($0)) })
            }
        } else {
            ScreenPhaseView(phase: store.screen.phase, missing: "Tasks are unavailable") { store.send(.task) }
        }
    }
}

/// "4 for you · 3 in progress · 1 waiting", read from the same blocks the list shows; from an older daemon,
/// "3 requests · 1 needs you · 2 running".
private struct TasksSummary: View {
    let board: TasksBoard

    private var parts: [Text] {
        if !board.active.blocks.isEmpty {
            return board.active.blocks.map { Text(verbatim: "\($0.count) \($0.title.lowercased())") }
        }
        let tasks = board.active.requests.flatMap(\.tasks)
        let needsYou = tasks.count { $0.icon == .action }
        let inReview = tasks.count { $0.icon == .pullRequest }
        let running = tasks.count { $0.icon == .mission }
        var parts: [Text] = [Text("\(board.active.count) requests")]
        if needsYou > 0 { parts.append(Text("\(needsYou) need you")) }
        if inReview > 0 { parts.append(Text("\(inReview) in review")) }
        if running > 0 { parts.append(Text("\(running) running")) }
        return parts
    }

    var body: some View {
        ViewThatFits(in: .horizontal) {
            joined(parts).fixedSize()
            VStack(alignment: .leading, spacing: 2) {
                joined(Array(parts.prefix(1))).fixedSize()
                joined(Array(parts.dropFirst())).fixedSize()
            }
            VStack(alignment: .leading, spacing: 2) {
                ForEach(Array(parts.enumerated()), id: \.offset) { _, part in part }
            }
        }
        .accessibilityElement(children: .combine)
    }

    private func joined(_ parts: [Text]) -> some View {
        HStack(spacing: 5) {
            ForEach(Array(parts.enumerated()), id: \.offset) { index, part in
                if index > 0 { Text(verbatim: "·") }
                part.lineLimit(1)
            }
        }
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
