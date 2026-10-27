import AlondraAttachments
import AlondraContract
import AlondraDesign
import AlondraFeatures
import ComposableArchitecture
import SwiftUI

/// The New tab: describe what you want done and attach what helps, here and at once; choose how
/// to launch it from the options Alondra offers; and see the requests on their way.
public struct NewRequestView: View {
    @Bindable var store: StoreOf<NewRequestFeature>
    @FocusState private var focused: Bool
    @State private var viewport: CGFloat = 0

    public init(store: StoreOf<NewRequestFeature>) { self.store = store }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 24) {
                Text("New")
                    .font(.display(.largeTitle, weight: .bold))
                    .foregroundStyle(Palette.ink)
                    .accessibilityAddTraits(.isHeader)
                editor
                optionsRow
                queue
            }
            .padding(20)
            .frame(maxWidth: .infinity, minHeight: viewport, alignment: .topLeading)
            .contentShape(Rectangle())
            .onTapGesture { focused = false }
        }
        .onScrollGeometryChange(for: CGFloat.self) { $0.containerSize.height } action: { _, height in viewport = height }
        .scrollDismissesKeyboard(.interactively)
        .background(DawnBackground(glow: Palette.ember))
        .navigationBarTitleDisplayMode(.inline)
        .safeAreaBar(edge: .bottom) { sendBar }
        .sheet(item: $store.scope(state: \.options, action: \.options)) { options in
            RequestOptionsSheet(store: options)
                .presentationDetents([.medium, .large])
        }
        .task { await store.send(.task).finish() }
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
        store.scope(state: \.attachments, action: \.attachments)
    }

    @ViewBuilder private var optionsRow: some View {
        if store.form != nil {
            if !store.optionFields.isEmpty {
                Button { store.send(.optionsTapped) } label: {
                    optionsLabel {
                        VStack(alignment: .leading, spacing: 2) {
                            if let model = store.model {
                                Label { Text(verbatim: model) } icon: { Image(systemName: "cpu") }
                                    .accessibilityIdentifier("request.model")
                            }
                            if let notice = store.legacyPresetNotice { Text(verbatim: notice).font(.caption).foregroundStyle(.secondary) }
                            if let reason = store.selectionUnavailableReason {
                                Text(verbatim: reason).font(.caption).foregroundStyle(Palette.ember)
                            }
                            if let project = store.project {
                                Text(verbatim: project).font(.subheadline.weight(.semibold))
                            } else {
                                Text("Alondra picks the project").font(.subheadline.weight(.semibold))
                            }
                            Text(verbatim: store.optionsSummary.joined(separator: " · "))
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .lineLimit(2)
                                .multilineTextAlignment(.leading)
                        }
                        .foregroundStyle(Palette.ink)
                    } trailing: {
                        Image(systemName: "chevron.right")
                            .font(.caption.weight(.semibold))
                            .foregroundStyle(.tertiary)
                    }
                }
                .buttonStyle(.plain)
                .accessibilityHint(Text("Changes the options for this request"))
            }
        } else if case .loading = store.screen.phase {
            optionsLabel {
                Text("Loading options…").font(.subheadline.weight(.semibold)).foregroundStyle(.secondary)
            } trailing: {
                Image(systemName: "hourglass").foregroundStyle(.tertiary)
            }
        } else {
            Button { store.send(.screen(.retry)) } label: {
                optionsLabel {
                    Text(verbatim: ScreenFeature.State.failure)
                        .font(.subheadline)
                        .foregroundStyle(Palette.ink)
                        .multilineTextAlignment(.leading)
                } trailing: {
                    Image(systemName: "arrow.clockwise")
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(Palette.ember)
                }
            }
            .buttonStyle(.plain)
        }
    }

    private func optionsLabel(@ViewBuilder _ content: () -> some View, @ViewBuilder trailing: () -> some View) -> some View {
        HStack(spacing: 12) {
            Image(systemName: "slider.horizontal.3")
                .foregroundStyle(Palette.ember)
            content()
            Spacer(minLength: 0)
            trailing()
        }
        .padding(14)
        .background(Palette.card.opacity(0.8), in: .rect(cornerRadius: 18, style: .continuous))
    }

    @ViewBuilder private var queue: some View {
        if !store.unshown.isEmpty || !store.recent.isEmpty {
            VStack(alignment: .leading, spacing: 10) {
                queueHeading
                    .font(.footnote.weight(.bold))
                    .textCase(.uppercase)
                    .kerning(0.6)
                    .foregroundStyle(.secondary)
                    .accessibilityAddTraits(.isHeader)
                ForEach(store.unshown) { entry in
                    PendingRequestRow(entry: entry,
                                      retry: { store.send(.retry(entry.id)) },
                                      discard: { store.send(.discard(entry.id)) })
                        .transition(.move(edge: .top).combined(with: .opacity))
                }
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
            .animation(.smooth(duration: 0.3), value: store.unshown)
        }
    }

    @ViewBuilder private var queueHeading: some View {
        if let title = store.recentTitle { Text(verbatim: title) } else { Text("Pending Requests") }
    }

    private var sendBar: some View {
        Button {
            focused = false
            store.send(.send, animation: .smooth(duration: 0.3))
        } label: {
            Label {
                Text("Send")
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
