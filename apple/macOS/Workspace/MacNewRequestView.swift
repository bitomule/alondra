import AlondraAttachments
import AlondraContract
import AlondraDesign
import AlondraFeatures
import ComposableArchitecture
import SwiftUI

/// Asking for something new on the Mac: what you want done and its attachments are written here
/// and go at once; the launch options come from Alondra; below, the requests on their way.
public struct MacNewRequestView: View {
    @Bindable var store: StoreOf<NewRequestFeature>
    @FocusState private var focused: Bool

    public init(store: StoreOf<NewRequestFeature>) { self.store = store }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                Text("Ask for Something New").font(.title2.weight(.semibold))
                editor
                HStack(alignment: .center, spacing: 12) {
                    optionsButton
                    Spacer(minLength: 12)
                    Button {
                        store.send(.send, animation: .smooth(duration: 0.3))
                        focused = true
                    } label: {
                        Label("Send", systemImage: "paperplane.fill").padding(.horizontal, 6)
                    }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.large)
                    .tint(Palette.ember)
                    .keyboardShortcut(.return, modifiers: .command)
                    .disabled(!store.canSend)
                    .help(Text("Send (⌘↩)"))
                }
                queue
            }
            .frame(maxWidth: 900, alignment: .leading)
            .padding(24)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .sheet(item: $store.scope(state: \.options, action: \.options)) { options in
            MacRequestOptionsSheet(store: options)
                .frame(minWidth: 460, minHeight: 380)
        }
        .task { await store.send(.task).finish() }
        .onAppear { focused = true }
    }

    private var editor: some View {
        VStack(alignment: .leading, spacing: 12) {
            ZStack(alignment: .topLeading) {
                if store.text.isEmpty {
                    Text("What do you want done? Describe it as you would to someone on the team.")
                        .font(.title3)
                        .foregroundStyle(.tertiary)
                        .padding(.horizontal, 5)
                        .padding(.vertical, 1)
                        .allowsHitTesting(false)
                }
                TextEditor(text: Binding(get: { store.text }, set: { store.send(.textChanged($0)) }))
                    .font(.title3)
                    .scrollContentBackground(.hidden)
                    .focused($focused)
                    .frame(minHeight: 180)
                    .accessibilityLabel("What do you want done?")
            }
            AttachmentPicker(store: store.scope(state: \.attachments, action: \.attachments))
        }
        .padding(16)
        .background(Palette.card, in: .rect(cornerRadius: 20, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 20, style: .continuous)
            .strokeBorder(focused ? Palette.ember.opacity(0.5) : Palette.hairline))
        .animation(.smooth(duration: 0.25), value: focused)
    }

    @ViewBuilder private var optionsButton: some View {
        if store.form != nil {
            if !store.optionFields.isEmpty {
                Button { store.send(.optionsTapped) } label: {
                    optionsLabel {
                        VStack(alignment: .leading, spacing: 2) {
                            if let project = store.project {
                                Text(verbatim: project).font(.callout.weight(.semibold))
                            } else {
                                Text("Alondra picks the project").font(.callout.weight(.semibold))
                            }
                            if !store.optionsSummary.isEmpty {
                                Text(verbatim: store.optionsSummary.joined(separator: " · "))
                                    .font(.caption).foregroundStyle(.secondary).lineLimit(1)
                            }
                        }
                        .foregroundStyle(Palette.ink)
                    } trailing: {
                        Image(systemName: "chevron.right").font(.caption.weight(.semibold)).foregroundStyle(.tertiary)
                    }
                }
                .buttonStyle(.plain)
                .help(Text("Changes the options for this request"))
            }
        } else if case .loading = store.screen.phase {
            optionsLabel {
                Text("Loading options…").font(.callout.weight(.semibold)).foregroundStyle(.secondary)
            } trailing: {
                Image(systemName: "hourglass").foregroundStyle(.tertiary)
            }
        } else {
            Button { store.send(.screen(.retry)) } label: {
                optionsLabel {
                    Text(verbatim: ScreenFeature.State.failure).font(.callout).foregroundStyle(Palette.ink)
                } trailing: {
                    Image(systemName: "arrow.clockwise").foregroundStyle(Palette.ember)
                }
            }
            .buttonStyle(.plain)
        }
    }

    private func optionsLabel(@ViewBuilder _ content: () -> some View, @ViewBuilder trailing: () -> some View) -> some View {
        HStack(spacing: 10) {
            Image(systemName: "slider.horizontal.3").foregroundStyle(Palette.ember)
            content()
            Spacer(minLength: 8)
            trailing()
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 9)
        .frame(maxWidth: 420, alignment: .leading)
        .background(Palette.card.opacity(0.8), in: .rect(cornerRadius: 12, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 12, style: .continuous).strokeBorder(Palette.hairline))
        .contentShape(.rect)
    }

    @ViewBuilder private var queue: some View {
        if !store.unshown.isEmpty || !store.recent.isEmpty {
            VStack(alignment: .leading, spacing: 8) {
                queueHeading
                    .font(.caption.weight(.bold))
                    .textCase(.uppercase)
                    .kerning(0.6)
                    .foregroundStyle(.secondary)
                    .accessibilityAddTraits(.isHeader)
                ForEach(store.unshown) { entry in
                    MacPendingRequestRow(entry: entry,
                                         retry: { store.send(.retry(entry.id)) },
                                         discard: { store.send(.discard(entry.id), animation: .smooth(duration: 0.3)) })
                        .transition(.move(edge: .top).combined(with: .opacity))
                }
                ForEach(store.recent, id: \.id) { row in
                    MacPendingRequestRow(row: row)
                }
            }
            .padding(.top, 6)
            .animation(.smooth(duration: 0.3), value: store.unshown)
        }
    }

    @ViewBuilder private var queueHeading: some View {
        if let title = store.recentTitle { Text(verbatim: title) } else { Text("Pending Requests") }
    }
}
