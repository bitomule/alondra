import AlondraContract
import AlondraDesign
import ComposableArchitecture
import SwiftUI

/// The sections of a server-drawn part of Settings, each a list of `ListRowView`s under its title;
/// a notice in a section is its footer. Absent until the view arrives; the screen showing it starts
/// `.task`.
public struct ServerSettingsSection: View {
    @Bindable var store: StoreOf<ServerSettingsFeature>
    private let showsTitle: Bool

    public init(store: StoreOf<ServerSettingsFeature>, showsTitle: Bool = true) { self.store = store; self.showsTitle = showsTitle }

    public var body: some View {
        Group {
            if let document = store.document {
                if document.sections.isEmpty, let empty = document.empty {
                    Section { Text(verbatim: empty.text).foregroundStyle(.secondary) } header: { title(document) }
                }
                ForEach(Array(document.sections.enumerated()), id: \.element.id) { index, section in
                    Section {
                        ForEach(rows(section), id: \.id) { row in
                            ListRowView(row: row, busy: busy, onAction: { store.send(.perform($0)) }, onLink: { store.send(.follow($0)) },
                                        onOption: { store.send(.performOption($0, optionID: $1)) })
                        }
                        if index == document.sections.count - 1 { messages }
                    } header: {
                        VStack(alignment: .leading, spacing: 6) {
                            if index == 0 { title(document) }
                            if let heading = section.title { Text(verbatim: heading) }
                        }
                    } footer: {
                        if let note = notes(section).first { Text(verbatim: note) }
                    }
                }
            }
        }
        .actionDialogs(store.scope(state: \.actions, action: \.actions))
        .sheet(item: $store.scope(state: \.signIn, action: \.signIn)) { signIn in
            McpSignInSheet(store: signIn)
        }
        .sheet(item: $store.claudeSignIn.sending(\.claudeSignInPresented)) { signIn in
            ClaudeSignInSheet(signIn: signIn) { store.send(.claudeSignInPresented(nil)) }
        }
        .alert("Name of the new account", isPresented: newAccountShown) {
            TextField("Work", text: newAccountName)
            Button("Add") { store.send(.newAccountAdded) }.disabled(store.newAccount?.canAdd != true)
            Button("Cancel", role: .cancel) { store.send(.newAccountCancelled) }
        } message: {
            Text("Then you sign it in to Claude in Terminal on the Mac.")
        }
    }

    private var newAccountShown: Binding<Bool> {
        Binding(get: { store.newAccount != nil }, set: { if !$0 { store.send(.newAccountCancelled) } })
    }

    private var newAccountName: Binding<String> {
        Binding(get: { store.newAccount?.name ?? "" }, set: { store.send(.newAccountNameChanged($0)) })
    }

    private var busy: Set<String> {
        Set([store.actions.pending?.actionId].compactMap { $0 })
    }

    @ViewBuilder private func title(_ document: ViewDocument) -> some View {
        if showsTitle, let title = document.title { Text(verbatim: title).font(.headline).foregroundStyle(Palette.ink).textCase(nil) }
    }

    @ViewBuilder private var messages: some View {
        if let notice = store.notice {
            Label { Text(verbatim: notice) } icon: { Image(systemName: "checkmark.circle.fill") }.font(.footnote).foregroundStyle(Palette.sage)
        }
        if let error = store.error ?? store.actions.error {
            Label { Text(verbatim: error) } icon: { Image(systemName: "exclamationmark.triangle.fill") }.font(.footnote).foregroundStyle(Palette.rose)
        }
    }

    private func rows(_ section: ViewSection) -> [AlondraContract.Row] {
        let hidden = store.hiddenRows
        return section.items.compactMap { item in if case .row(let row) = item, !hidden.contains(row.id) { row } else { nil } }
    }

    private func notes(_ section: ViewSection) -> [String] {
        section.items.compactMap { item in if case .notice(let notice) = item { notice.text } else { nil } }
    }
}
