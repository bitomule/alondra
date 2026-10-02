import AlondraAttachments
import AlondraContract
import AlondraDesign
import AlondraFeatures
import ComposableArchitecture
import SwiftUI

/// The New tab: describe what you want done, attach what helps, choose how to launch it, and see
/// what you already sent.
public struct NewRequestView: View {
    @Bindable var store: StoreOf<NewRequestFeature>
    @FocusState private var focused: Bool

    public init(store: StoreOf<NewRequestFeature>) { self.store = store }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 24) {
                VStack(alignment: .leading, spacing: 6) {
                    Text("New")
                        .font(.display(.largeTitle, weight: .bold))
                        .foregroundStyle(Palette.ink)
                        .accessibilityAddTraits(.isHeader)
                    orchestratorStatus
                }
                if store.form != nil {
                    editor
                    optionsRow
                    recentRequests
                } else {
                    ScreenPhaseView(phase: store.screen.phase, missing: "This screen is no longer available") { store.send(.task) }
                }
            }
            .padding(20)
        }
        .scrollDismissesKeyboard(.interactively)
        .background(DawnBackground(glow: Palette.ember))
        .navigationBarTitleDisplayMode(.inline)
        .safeAreaBar(edge: .bottom) { sendBar }
        .sheet(item: $store.scope(state: \.options, action: \.options)) { options in
            RequestOptionsSheet(store: options)
                .presentationDetents([.medium, .large])
        }
        .task { await store.send(.task).finish() }
        .actionDialogs(store.scope(state: \.actions, action: \.actions))
        .sensoryFeedback(.success, trigger: store.actions.stamp)
    }

    @ViewBuilder private var orchestratorStatus: some View {
        if let orchestrator = store.orchestrator {
            Label {
                Text(verbatim: orchestrator.text)
            } icon: {
                Image(systemName: "point.3.connected.trianglepath.dotted")
            }
            .font(.subheadline)
            .foregroundStyle(orchestrator.tone == .critical ? Palette.rose : Color.secondary)
        }
    }

    private var editor: some View {
        VStack(alignment: .leading, spacing: 10) {
            ZStack(alignment: .topLeading) {
                if store.text.isEmpty {
                    Text("What do you want done? Describe it as you would to someone on the team.")
                        .font(.display(.title3, weight: .regular))
                        .foregroundStyle(.tertiary)
                        .padding(.horizontal, 5)
                        .padding(.vertical, 8)
                        .allowsHitTesting(false)
                }
                TextEditor(text: Binding(get: { store.text }, set: { store.send(.textChanged($0)) }))
                    .font(.display(.title3, weight: .regular))
                    .scrollContentBackground(.hidden)
                    .focused($focused)
                    .frame(minHeight: 240)
                    .accessibilityLabel("What do you want done?")
            }
            HStack(alignment: .center, spacing: 8) {
                AttachButton(store: attachments)
                AttachmentStrip(store: attachments)
            }
        }
        .padding(16)
        .background(Palette.card, in: .rect(cornerRadius: 28, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 28, style: .continuous).strokeBorder(focused ? Palette.ember.opacity(0.5) : Palette.hairline))
        .shadow(color: .black.opacity(0.12), radius: 18, y: 10)
        .animation(.smooth(duration: 0.25), value: focused)
    }

    private var attachments: StoreOf<AttachmentFeature> {
        store.scope(state: \.actions.attachmentPicker, action: \.actions.attachmentPicker)
    }

    @ViewBuilder private var optionsRow: some View {
        if !store.optionFields.isEmpty {
            Button { store.send(.optionsTapped) } label: {
                HStack(spacing: 12) {
                    Image(systemName: "slider.horizontal.3")
                        .foregroundStyle(Palette.ember)
                    VStack(alignment: .leading, spacing: 2) {
                        if let project = store.project {
                            Text(verbatim: project).font(.subheadline.weight(.semibold))
                        } else {
                            Text("The orchestrator picks the project").font(.subheadline.weight(.semibold))
                        }
                        Text(verbatim: store.optionsSummary.joined(separator: " · "))
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .lineLimit(2)
                            .multilineTextAlignment(.leading)
                    }
                    .foregroundStyle(Palette.ink)
                    Spacer(minLength: 0)
                    Image(systemName: "chevron.right")
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(.tertiary)
                }
                .padding(14)
                .background(Palette.card.opacity(0.8), in: .rect(cornerRadius: 18, style: .continuous))
            }
            .buttonStyle(.plain)
            .accessibilityHint(Text("Changes the options for this request"))
        }
    }

    @ViewBuilder private var recentRequests: some View {
        if !store.recent.isEmpty {
            VStack(alignment: .leading, spacing: 10) {
                recentHeading
                    .font(.footnote.weight(.bold))
                    .textCase(.uppercase)
                    .kerning(0.6)
                    .foregroundStyle(.secondary)
                    .accessibilityAddTraits(.isHeader)
                ForEach(store.recent, id: \.id) { row in
                    VStack(alignment: .leading, spacing: 8) {
                        Text(verbatim: row.title)
                            .font(.subheadline)
                            .foregroundStyle(Palette.ink)
                            .lineLimit(4)
                            .multilineTextAlignment(.leading)
                        HStack(spacing: 6) {
                            ForEach(row.badges, id: \.text) { badge in
                                StateChip(label: badge.text, tint: TaskStyle.tint(tone: badge.tone))
                            }
                            if let sent = row.trailing?.instant {
                                RelativeTime(date: sent).font(.caption.weight(.medium)).foregroundStyle(.secondary)
                            }
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(14)
                    .background(Palette.card.opacity(0.8), in: .rect(cornerRadius: 18, style: .continuous))
                }
            }
        }
    }

    @ViewBuilder private var recentHeading: some View {
        if let title = store.recentTitle { Text(verbatim: title) } else { Text("Recent Requests") }
    }

    private var sendBar: some View {
        Button { store.send(.send) } label: {
            Label {
                if store.actions.inFlight { Text("Sending…") } else { Text("Ask the Orchestrator") }
            } icon: {
                Image(systemName: "paperplane.fill")
            }
            .font(.headline)
            .frame(maxWidth: .infinity)
        }
        .buttonStyle(.glassProminent)
        .buttonBorderShape(.capsule)
        .controlSize(.large)
        .tint(Palette.ember)
        .disabled(!store.canSend)
        .padding(.horizontal, 20)
        .padding(.vertical, 8)
    }
}
