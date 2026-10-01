import AlondraContract
import AlondraDesign
import AlondraFeatures
import ComposableArchitecture
import SwiftUI

/// The scheduled tasks: each with when it runs next, and run now, pause or resume, and delete.
public struct SchedulesView: View {
    @Bindable var store: StoreOf<SchedulesFeature>

    public init(store: StoreOf<SchedulesFeature>) { self.store = store }

    public var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 0) {
                Text(verbatim: store.list?.title ?? "")
                    .font(.display(.largeTitle, weight: .bold))
                    .foregroundStyle(Palette.ink)
                    .padding(.horizontal, 20)
                    .accessibilityAddTraits(.isHeader)
                content
            }
            .padding(.bottom, 24)
        }
        .background(DawnBackground())
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button("Add Schedule", systemImage: "plus") { store.send(.add) }
                    .disabled(store.list?.add == nil)
            }
        }
        .refreshable { await store.send(.refresh).finish() }
        .task { await store.send(.task).finish() }
        .actionDialogs(store.scope(state: \.actions, action: \.actions))
        .sensoryFeedback(.success, trigger: store.actions.stamp)
    }

    @ViewBuilder private var content: some View {
        if let list = store.list {
            if list.entries.isEmpty {
                ContentUnavailableView {
                    Label(list.empty?.text ?? String(localized: "No scheduled tasks"), systemImage: "calendar.badge.clock")
                } description: {
                    Text("Run a task automatically on a schedule.")
                } actions: {
                    Button("Add Schedule") { store.send(.add) }
                        .buttonStyle(.borderedProminent)
                        .tint(Palette.dawn)
                }
                .padding(.top, 60)
            } else {
                VStack(spacing: 0) {
                    ForEach(Array(list.entries.enumerated()), id: \.element.id) { index, entry in
                        ScheduleRow(
                            entry: entry, first: index == 0, busy: store.actions.inFlight,
                            open: { store.send(.open($0)) }, perform: { store.send(.perform($0)) }
                        )
                    }
                }
                .background(Palette.card.opacity(0.85), in: .rect(cornerRadius: 24, style: .continuous))
                .padding(.horizontal, 16)
                .padding(.top, 20)
            }
        } else {
            ScreenPhaseView(phase: store.screen.phase, missing: "Scheduled tasks are unavailable") { store.send(.task) }
        }
    }
}

private struct ScheduleRow: View {
    let entry: ScheduleList.Entry
    let first: Bool
    let busy: Bool
    let open: (AlondraContract.Link) -> Void
    let perform: (WireAction) -> Void

    var body: some View {
        HStack(spacing: 4) {
            Button {
                if let link = entry.link { open(link) }
            } label: {
                HStack(alignment: .top, spacing: 12) {
                    Glyph(symbol: entry.isPaused ? "pause.fill" : "calendar", tint: entry.isPaused ? Palette.slate : Palette.dawn)
                    VStack(alignment: .leading, spacing: 4) {
                        Text(verbatim: entry.title)
                            .font(.subheadline.weight(.semibold))
                            .foregroundStyle(entry.isPaused ? AnyShapeStyle(.secondary) : AnyShapeStyle(Palette.ink))
                            .multilineTextAlignment(.leading)
                        HStack(spacing: 6) {
                            if let frequency = entry.frequency {
                                Text(verbatim: frequency)
                                    .foregroundStyle(.secondary)
                            }
                            ForEach(entry.badges, id: \.text) { badge in
                                StateChip(label: badge.text, tint: TaskStyle.tint(tone: badge.tone))
                            }
                        }
                        .font(.caption.weight(.semibold))
                        if let nextRun = entry.nextRun {
                            HStack(spacing: 4) {
                                Image(systemName: "clock")
                                Text("Next run \(nextRun, format: .dateTime.weekday(.abbreviated).day().month(.abbreviated).hour().minute())")
                            }
                            .font(.caption)
                            .foregroundStyle(.tertiary)
                        }
                    }
                    Spacer(minLength: 0)
                }
                .contentShape(.rect)
            }
            .buttonStyle(PressStyle())
            Menu {
                if let run = entry.run { Button(run.label, systemImage: "play.fill") { perform(run) } }
                if let toggle = entry.toggle {
                    Button(toggle.label, systemImage: entry.isPaused ? "play.circle" : "pause.circle") { perform(toggle) }
                }
                if let delete = entry.delete {
                    Button(delete.label, systemImage: "trash", role: .destructive) { perform(delete) }
                }
            } label: {
                Label("More", systemImage: "ellipsis")
                    .labelStyle(.iconOnly)
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(.secondary)
                    .frame(minWidth: 36, minHeight: 36)
                    .contentShape(.rect)
            }
            .disabled(busy)
        }
        .padding(.leading, 16)
        .padding(.trailing, 8)
        .padding(.vertical, 12)
        .overlay(alignment: .top) { if !first { Hairline(leading: 62) } }
    }
}
