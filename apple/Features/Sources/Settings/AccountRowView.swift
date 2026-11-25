import AlondraContract
import AlondraDesign
import SwiftUI

public struct AccountRowView: View {
    public let row: AlondraContract.Row
    public let onOpen: () -> Void
    public init(row: AlondraContract.Row, onOpen: @escaping () -> Void) { self.row = row; self.onOpen = onOpen }

    public var body: some View {
        if let account = row.account {
            Button(action: onOpen) {
                VStack(alignment: .leading, spacing: 4) {
                    ViewThatFits(in: .horizontal) {
                        HStack(alignment: .firstTextBaseline) { identity(account); Spacer(minLength: 12); ListToneChip(label: account.availability); Image(systemName: "chevron.right").font(.caption.weight(.semibold)).foregroundStyle(.secondary).accessibilityHidden(true) }
                        VStack(alignment: .leading, spacing: 6) { identity(account); ListToneChip(label: account.availability) }
                    }
                    if let email = account.email { Text(verbatim: email).font(.footnote).foregroundStyle(.secondary) }
                    if let summary = account.usageSummary {
                        Text(verbatim: summary)
                            .font(.caption.monospacedDigit())
                            .foregroundStyle(.secondary)
                            .fixedSize(horizontal: false, vertical: true)
                        AccountActivityLabel(count: account.activeNodes)
                    } else {
                        if let window = account.windows.first { AccountUsageWindowView(window: window, compact: true) }
                        HStack(alignment: .firstTextBaseline) {
                            Text(verbatim: account.freshness.text).font(.caption).foregroundStyle(.secondary)
                            Spacer(minLength: 8)
                            AccountActivityLabel(count: account.activeNodes)
                        }
                    }
                    if let reason = account.reason { Text(verbatim: reason).font(.footnote).foregroundStyle(.secondary) }
                }
                .foregroundStyle(Palette.ink)
                .padding(.vertical, 4)
                .frame(maxWidth: .infinity, alignment: .leading)
                .contentShape(.rect)
            }
            .buttonStyle(.plain)
            .accessibilityElement(children: .combine)
            .accessibilityHint("Open account details")
            .accessibilityIdentifier("account.row.\(account.key.wireID)")
        }
    }

    private func identity(_ account: AccountPresentation) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(verbatim: row.title).font(.body.weight(.semibold)).fixedSize(horizontal: false, vertical: true)
            Text(verbatim: "\(Int(account.position)). \(account.key.provider.displayName)").font(.caption.weight(.medium)).foregroundStyle(.secondary)
        }
    }

}

struct AccountActivityLabel: View {
    let count: UInt32
    var body: some View {
        Group {
            if count == 1 { Text("1 active node") }
            else { Text("\(Int(count)) active nodes") }
        }.font(.caption).foregroundStyle(.secondary)
    }
}

struct AccountUsageWindowView: View {
    let window: AccountWindowPresentation
    var compact = false
    var body: some View {
        if compact {
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 8) {
                    Text(verbatim: window.label).font(.caption.weight(.medium))
                    ProgressView(value: Double(window.usedPercent), total: 100)
                        .tint(window.stale ? Palette.slate : window.exhausted ? Palette.ember : Palette.sage)
                        .accessibilityHidden(true)
                    Text("\(Int(window.usedPercent))% used").font(.caption.monospacedDigit())
                }
                if window.stale { Text("Previous reading · reset has passed").font(.caption).foregroundStyle(Palette.ember) }
            }
        } else {
        VStack(alignment: .leading, spacing: 4) {
            HStack(alignment: .firstTextBaseline) {
                Text(verbatim: window.label).font(.footnote.weight(.medium))
                Spacer(minLength: 8)
                Text("\(Int(window.usedPercent))% used").font(.footnote.monospacedDigit())
            }
            ProgressView(value: Double(window.usedPercent), total: 100)
                .tint(window.stale ? Palette.slate : window.exhausted ? Palette.ember : Palette.sage)
                .accessibilityLabel(Text(verbatim: window.label))
                .accessibilityValue(Text("\(Int(window.usedPercent))% used"))
            if window.stale { Text("Previous reading · reset has passed").font(.caption).foregroundStyle(Palette.ember) }
            if !compact {
                Text("Reported \(Date(timeIntervalSince1970: Double(window.measuredAt) / 1000).formatted(date: .abbreviated, time: .shortened))")
                    .font(.caption).foregroundStyle(.secondary)
                if let reset = window.resetsAt {
                    Text("Resets \(Date(timeIntervalSince1970: Double(reset) / 1000).formatted(date: .abbreviated, time: .shortened))")
                        .font(.caption).foregroundStyle(.secondary)
                }
            }
        }
        }
    }
}

public struct AccountDetailView: View {
    public let row: AlondraContract.Row
    public let onAction: (Action) -> Void
    public let onLink: (Link) -> Void
    public let onOption: (Action, String) -> Void
    public let onReorder: (() -> Void)?
    public let keyField: AccountKeyField?
    @Environment(\.dismiss) private var dismiss
    public init(row: AlondraContract.Row, onAction: @escaping (Action) -> Void, onLink: @escaping (Link) -> Void,
                onOption: @escaping (Action, String) -> Void, onReorder: (() -> Void)? = nil, keyField: AccountKeyField? = nil) {
        self.row = row; self.onAction = onAction; self.onLink = onLink; self.onOption = onOption; self.onReorder = onReorder; self.keyField = keyField
    }
    public var body: some View {
        NavigationStack {
            List {
                if let account = row.account {
                    Section("Account") {
                        LabeledContent("Provider", value: account.key.provider.displayName)
                        if let email = account.email { LabeledContent("Email", value: email) }
                        if let plan = account.plan { LabeledContent("Account plan", value: plan) }
                        ListToneChip(label: account.availability)
                        if let reason = account.reason { Text(verbatim: reason).foregroundStyle(.secondary) }
                    }
                    Section("Usage") {
                        if account.windows.isEmpty { Text("Usage not reported").foregroundStyle(.secondary) }
                        ForEach(account.windows) { AccountUsageWindowView(window: $0) }
                        Text(verbatim: account.freshness.text).font(.caption).foregroundStyle(.secondary)
                        if let source = account.quotaSource { Text(verbatim: source).font(.caption).foregroundStyle(.secondary) }
                    }
                    if let credential = account.credential, let keyField {
                        AccountKeySection(account: account, credential: credential, actions: row.actions, field: keyField)
                    }
                    Section("Activity") { AccountActivityLabel(count: account.activeNodes) }
                    Section("Models") {
                        if let catalog = account.modelCatalog {
                            ForEach(catalog.models.filter { !$0.hidden }) { model in
                                VStack(alignment: .leading, spacing: 4) {
                                    Text(verbatim: model.displayName).font(.body.weight(.medium))
                                    Text(verbatim: model.model).font(.caption.monospaced()).foregroundStyle(.secondary)
                                    if !model.reasoningEfforts.isEmpty { Text(verbatim: model.reasoningEfforts.joined(separator: " · ")).font(.caption).foregroundStyle(.secondary) }
                                    if model.isDefault || catalog.nativeDefault == model.model { Text("Native default").font(.caption).foregroundStyle(Palette.sage) }
                                    if let context = model.contextWindow { Text("Context: \(context.formatted()) tokens").font(.caption).foregroundStyle(.secondary) }
                                }
                            }
                        } else { Text("Models have not been checked yet").foregroundStyle(.secondary) }
                    }
                    Section("Actions") {
                        ListRowView(row: row, onAction: onAction, onLink: onLink, onOption: onOption,
                                    omitting: { keyField != nil && $0.command == AccountKeySection.command })
                        if let onReorder { Button("Move to position", action: onReorder) }
                    }
                }
            }
            .navigationTitle(Text(verbatim: row.title))
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
        }
        #if os(macOS)
        .frame(minWidth: 480, minHeight: 520)
        #endif
    }
}

/// What the key section of an account's settings shows and does: the draft typed into it, how
/// the last send went, and what saving or removing does.
public struct AccountKeyField {
    public var draft: Binding<String>
    public var send: CredentialSend?
    public var onSave: () -> Void
    public var onRemove: () -> Void
    public init(draft: Binding<String>, send: CredentialSend?, onSave: @escaping () -> Void, onRemove: @escaping () -> Void) {
        self.draft = draft; self.send = send; self.onSave = onSave; self.onRemove = onRemove
    }
}

/// The key of a named Antigravity account: whether one is set, a secure field that is never
/// prefilled, and once a key is sent or removed, what the daemon said and the account check that
/// ran right after.
public struct AccountKeySection: View {
    static let command = "account_credential"
    let account: AccountPresentation
    let credential: AccountCredentialPresentation
    let actions: [Action]
    let field: AccountKeyField
    @State private var confirmingRemoval = false

    public init(account: AccountPresentation, credential: AccountCredentialPresentation, actions: [Action], field: AccountKeyField) {
        self.account = account; self.credential = credential; self.actions = actions; self.field = field
    }

    private var setAction: Action? { actions.first { $0.command == Self.command && $0.optionId == nil } }
    private var clearAction: Action? { actions.first { $0.command == Self.command && $0.optionId == "clear" } }
    private var sending: Bool { field.send?.phase == .sending }
    private var typed: Bool { !field.draft.wrappedValue.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }

    public var body: some View {
        Section {
            LabeledContent("Status") { ListToneChip(label: credential.state) }
            if let setAction {
                SecureField(credential.label, text: field.draft)
                    .privacySensitive()
                    .autocorrectionDisabled()
                    #if os(iOS)
                    .textInputAutocapitalization(.never)
                    #endif
                    .disabled(sending || !setAction.enabled)
                    .onSubmit { if typed { field.onSave() } }
                    .accessibilityIdentifier("account.key.field")
                HStack(spacing: 10) {
                    Button { field.onSave() } label: { Text(verbatim: setAction.label) }
                        .buttonStyle(.borderedProminent).tint(Palette.dawn)
                        .disabled(!typed || sending || !setAction.enabled)
                        .accessibilityIdentifier("account.key.save")
                    if let clearAction {
                        Button(role: .destructive) { confirmingRemoval = true } label: { Text(verbatim: clearAction.label) }
                            .buttonStyle(.bordered)
                            .disabled(sending || !clearAction.enabled)
                            .accessibilityIdentifier("account.key.remove")
                    }
                }
                if let description = setAction.description { Text(verbatim: description).font(.footnote).foregroundStyle(.secondary) }
            }
            outcome
        } header: {
            Text(verbatim: credential.label)
        }
        .confirmationDialog(Text(verbatim: clearAction?.confirm?.title ?? ""), isPresented: $confirmingRemoval, titleVisibility: .visible) {
            if let clearAction {
                Button(role: .destructive) { field.onRemove() } label: { Text(verbatim: clearAction.confirm?.button ?? clearAction.label) }
            }
        } message: {
            if let message = clearAction?.confirm?.message { Text(verbatim: message) }
        }
    }

    @ViewBuilder private var outcome: some View {
        switch field.send?.phase {
        case .sending?:
            HStack(spacing: 8) {
                ProgressView().controlSize(.small)
                Text(field.send?.removing == true ? "Removing the key…" : "Saving the key and checking the account…")
                    .font(.footnote).foregroundStyle(.secondary)
            }
        case .done(let message)?:
            Label { Text(verbatim: message) } icon: { Image(systemName: "checkmark.circle.fill") }
                .font(.footnote).foregroundStyle(Palette.sage)
            LabeledContent("Account check") { ListToneChip(label: account.availability) }
            if let reason = account.reason { Text(verbatim: reason).font(.footnote).foregroundStyle(.secondary) }
        case .failed(let message)?:
            Label { Text(verbatim: message) } icon: { Image(systemName: "exclamationmark.triangle.fill") }
                .font(.footnote).foregroundStyle(Palette.rose)
        case nil:
            EmptyView()
        }
    }
}
