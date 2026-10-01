import AlondraContract
import AlondraDesign
import AlondraFeatures
import ComposableArchitecture
import SwiftUI

/// One request: its state, the tasks it was split into, its text, and withdrawing it.
public struct RequestDetailView: View {
    @Bindable var store: StoreOf<RequestDetailFeature>

    public init(store: StoreOf<RequestDetailFeature>) { self.store = store }

    public var body: some View {
        Group {
            if let detail = store.detail {
                ScrollView {
                    VStack(alignment: .leading, spacing: 24) {
                        header(detail)
                        tasks(detail)
                        text(detail)
                        withdraw(detail)
                    }
                    .padding(.horizontal, 20)
                    .padding(.top, 4)
                    .padding(.bottom, 32)
                }
            } else {
                ScrollView {
                    ScreenPhaseView(phase: store.screen.phase, missing: "This request is gone") { store.send(.task) }
                }
            }
        }
        .background(DawnBackground(glow: Palette.sky))
        .navigationBarTitleDisplayMode(.inline)
        .toolbar(.hidden, for: .tabBar)
        .task { await store.send(.task).finish() }
        .actionDialogs(store.scope(state: \.actions, action: \.actions))
        .sensoryFeedback(.success, trigger: store.actions.stamp)
    }

    private func header(_ detail: RequestDetail) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 10) {
                Glyph(symbol: "paperplane.fill", tint: Palette.sky)
                if let state = detail.state {
                    Text(verbatim: state)
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(Palette.sky)
                }
            }
            Text(verbatim: detail.title)
                .font(.display(.largeTitle, weight: .bold))
                .foregroundStyle(Palette.ink)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    @ViewBuilder private func tasks(_ detail: RequestDetail) -> some View {
        if !detail.tasks.isEmpty {
            DetailSection(title: detail.tasksTitle ?? String(localized: "Tasks"), symbol: "checklist") {
                VStack(spacing: 0) {
                    ForEach(Array(detail.tasks.enumerated()), id: \.element.id) { index, task in
                        Button {
                            if let link = task.link { store.send(.open(link)) }
                        } label: {
                            TaskLine(task: task)
                        }
                        .buttonStyle(PressStyle())
                        .overlay(alignment: .top) { if index > 0 { Hairline(leading: 62) } }
                    }
                }
            }
        }
    }

    @ViewBuilder private func text(_ detail: RequestDetail) -> some View {
        if let text = detail.text {
            DetailSection(title: String(localized: "Request"), symbol: "text.quote") {
                Text(verbatim: text)
                    .font(.body)
                    .foregroundStyle(Palette.ink)
                    .fixedSize(horizontal: false, vertical: true)
                    .textSelection(.enabled)
                    .padding(16)
            }
        }
    }

    @ViewBuilder private func withdraw(_ detail: RequestDetail) -> some View {
        if let withdraw = detail.withdraw {
            ActionButton(
                title: withdraw.label, symbol: "xmark.circle", tint: Palette.rose,
                enabled: withdraw.enabled && !store.actions.inFlight
            ) { store.send(.withdraw) }
        }
    }
}

private struct TaskLine: View {
    let task: RequestDetail.Entry

    var body: some View {
        let tint = TaskStyle.tint(icon: task.icon, tone: task.tone)
        HStack(spacing: 8) {
            HStack(alignment: .top, spacing: 12) {
                Glyph(symbol: TaskStyle.symbol(icon: task.icon, struck: task.struck), tint: tint)
                VStack(alignment: .leading, spacing: 3) {
                    Text(verbatim: task.title)
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(task.struck ? AnyShapeStyle(.secondary) : AnyShapeStyle(Palette.ink))
                        .strikethrough(task.struck)
                        .multilineTextAlignment(.leading)
                    if let state = task.state {
                        Text(verbatim: state)
                            .font(.caption.weight(.semibold))
                            .foregroundStyle(tint)
                    }
                }
            }
            Spacer(minLength: 0)
            HStack(spacing: 8) {
                if task.waiting > 0 { WaitingBadge(count: task.waiting) }
                Image(systemName: "chevron.right")
                    .font(.caption.weight(.bold))
                    .foregroundStyle(.tertiary)
                    .accessibilityHidden(true)
            }
        }
        .padding(.leading, 16)
        .padding(.trailing, 14)
        .padding(.vertical, 12)
        .contentShape(.rect)
    }
}
