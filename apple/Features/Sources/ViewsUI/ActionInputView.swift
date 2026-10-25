import AlondraAttachments
import AlondraContract
import AlondraDesign
import ComposableArchitecture
import SwiftUI

/// Reusable editor for the server-declared inputs associated with a view action.
public struct ActionInputView: View {
    @Bindable public var store: StoreOf<ActionFeature>
    @FocusState private var textFocused: Bool
    public let form: WireForm?
    public let showCommentEditor: Bool
    public var onReload: ((Field, JSONValue) -> Void)?
    public var onPicker: ((Field, String) -> Void)?

    public init(store: StoreOf<ActionFeature>, form: WireForm? = nil, showCommentEditor: Bool = true,
                onReload: ((Field, JSONValue) -> Void)? = nil,
                onPicker: ((Field, String) -> Void)? = nil) {
        self.store = store
        self.form = form
        self.showCommentEditor = showCommentEditor
        self.onReload = onReload
        self.onPicker = onPicker
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            if let action = store.action {
                if !action.options.isEmpty {
                    OptionGrid(options: action.options, selection: store.selectedOptionID) {
                        store.send(.chooseOption($0.id))
                    }
                }
                let input = action.options.first(where: { $0.id == store.selectedOptionID })?.input ?? action.input
                self.input(input, attaches: takesAttachments(action))
                if takesAttachments(action), !embedsAttachments(input) {
                    attachments
                }
                if let commentSpec = action.comments {
                    if showCommentEditor {
                        ActionCommentsEditor(comments: store.comments, kind: commentSpec.kind) { store.send(.commentsChanged($0)) }
                    } else if !store.comments.isEmpty {
                        commentList
                    }
                }
                if let form, form.id == "schedule" {
                    scheduleForm(form.fields)
                } else if let form {
                    formFields(form.fields)
                }
                if let error = store.error { Text(error).font(.callout).foregroundStyle(.red) }
                StallNoticeView(store: store)
                if let stamp = store.stamp { Label(stamp, systemImage: "checkmark.circle.fill").foregroundStyle(.green) }
            }
        }
    }

    private func takesAttachments(_ action: WireAction) -> Bool {
        return action.attachments
            || form?.fields.contains(where: { if case .attachments = $0.kind { true } else { false } }) == true
    }

    /// On the phone a written answer carries its paperclip inside the writing card, as the New
    /// tab and the node composer do.
    private func embedsAttachments(_ input: AlondraContract.Input) -> Bool {
        #if os(iOS)
        if case .text(_, _, true) = input { return true }
        #endif
        return false
    }

    @ViewBuilder private var attachments: some View {
        #if os(iOS)
        HStack(alignment: .center, spacing: 8) {
            AttachButton(store: attachmentStore)
            AttachmentStrip(store: attachmentStore)
        }
        #else
        AttachmentPicker(store: attachmentStore)
        #endif
    }

    private var commentList: some View {
        VStack(alignment: .leading, spacing: 10) {
            Label("Your comments", systemImage: "text.bubble")
                .font(.footnote.weight(.bold))
                .textCase(.uppercase)
                .kerning(0.6)
                .foregroundStyle(.secondary)
            ForEach(store.comments, id: \.anchor) { comment in
                Text(comment.text)
                    .font(.callout)
                    .foregroundStyle(ViewPalette.ink)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(14)
                    .background(ViewPalette.card.opacity(0.9), in: .rect(cornerRadius: 20, style: .continuous))
            }
        }
        .padding(.top, 6)
    }

    private var attachmentStore: StoreOf<AttachmentFeature> {
        store.scope(state: \.attachmentPicker, action: \.attachmentPicker)
    }

    @ViewBuilder private func input(_ input: AlondraContract.Input, attaches: Bool) -> some View {
        switch input {
        case .none: EmptyView()
        case let .text(placeholder, _, multiline):
            if multiline {
                VStack(alignment: .leading, spacing: 12) {
                    MultilineTextField(placeholder ?? store.action?.label ?? "",
                                       text: Binding(get: { store.text }, set: { store.send(.textChanged($0)) }),
                                       lines: 4...12, focus: $textFocused)
                        .font(.display(.title3, weight: .regular))
                    if attaches, embedsAttachments(input) { attachments }
                }
                .padding(16)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(ViewPalette.card, in: .rect(cornerRadius: 28, style: .continuous))
                .overlay(RoundedRectangle(cornerRadius: 28, style: .continuous)
                    .strokeBorder(textFocused ? ViewPalette.dawn.opacity(0.5) : ViewPalette.hairline))
                .shadow(color: .black.opacity(0.12), radius: 18, y: 10)
                .animation(.smooth(duration: 0.25), value: textFocused)
                .onAppear { textFocused = true }
            } else {
                TextField(placeholder ?? "", text: Binding(get: { store.text }, set: { store.send(.textChanged($0)) }))
                    .textFieldStyle(.roundedBorder)
            }
        }
    }

    @ViewBuilder private func formFields(_ fields: [Field]) -> some View {
        ForEach(fields, id: \.id) { field in
            if visible(field) {
                VStack(alignment: .leading, spacing: 5) {
                    Text(field.label).font(.subheadline.weight(.semibold))
                    fieldControl(field)
                    if let help = field.help { Text(help).font(.caption).foregroundStyle(.secondary) }
                }
            }
        }
    }

    @ViewBuilder private func scheduleForm(_ fields: [Field]) -> some View {
        let mission = fields.filter { ["name", "mission", "project"].contains($0.id) }
        let timing = fields.filter { ["repeat", "weekday", "interval", "start_at", "time", "active"].contains($0.id) }
        let advanced = fields.filter { $0.id == "disallowed_tools" }
        let launch = fields.filter { !["name", "mission", "project", "repeat", "weekday", "interval", "start_at", "time", "active", "disallowed_tools"].contains($0.id) }
        VStack(alignment: .leading, spacing: 20) {
            scheduleSection("Mission", fields: mission)
            scheduleSection("When", fields: timing)
            if !launch.isEmpty { scheduleSection("Launch options", fields: launch) }
            if !advanced.isEmpty { scheduleSection("Advanced", fields: advanced) }
        }
        .frame(minHeight: 260)
    }

    private func scheduleSection(_ title: LocalizedStringKey, fields: [Field]) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(title).font(.headline)
            formFields(fields)
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(ViewPalette.card.opacity(0.9), in: .rect(cornerRadius: 18))
    }

    @ViewBuilder private func fieldControl(_ field: Field) -> some View {
        let value = store.fields[field.id] ?? field.value
        switch field.kind {
        case let .text(placeholder), let .multiline(placeholder), let .lines(placeholder):
            if case .multiline = field.kind {
                MultilineTextField(placeholder ?? "", text: stringBinding(field, value: value), lines: 4...8)
                    .padding(10).background(Color.primary.opacity(0.06), in: .rect(cornerRadius: 10))
            } else if case .lines = field.kind {
                MultilineTextField(placeholder ?? "", text: stringBinding(field, value: value), lines: 2...6)
                    .padding(10).background(Color.primary.opacity(0.06), in: .rect(cornerRadius: 10))
            } else {
                TextField(placeholder ?? "", text: stringBinding(field, value: value))
                    .padding(10).background(Color.primary.opacity(0.06), in: .rect(cornerRadius: 10))
            }
        case .attachments:
            EmptyView()
        case .toggle:
            Toggle(field.label, isOn: boolBinding(field, value: value)).labelsHidden()
        case let .choice(items):
            if case .integer = value {
                Picker(field.label, selection: integerBinding(field, value: value)) {
                    ForEach(items, id: \.id) { item in
                        if let number = Int64(item.id) { Text(item.label).tag(number) }
                    }
                }.labelsHidden().pickerStyle(.menu)
            } else {
                Picker(field.label, selection: stringBinding(field, value: value)) {
                    ForEach(items, id: \.id) { item in Text(item.label).tag(item.id) }
                }.labelsHidden().pickerStyle(.menu)
            }
        case let .picker(source):
            if let onPicker {
                Button {
                    onPicker(field, source)
                } label: {
                    HStack {
                        if case let .string(selected) = value, !selected.isEmpty { Text(selected) }
                        else { Text("Choose an issue") }
                        Spacer()
                        Image(systemName: "chevron.right").foregroundStyle(.secondary)
                    }
                }
                .buttonStyle(.bordered)
            } else {
                TextField(field.label, text: stringBinding(field, value: value)).textFieldStyle(.roundedBorder)
            }
        case .date:
            DatePicker(field.label, selection: dateBinding(field, value: value), displayedComponents: .date)
                .labelsHidden()
        case .time:
            DatePicker(field.label, selection: dateBinding(field, value: value), displayedComponents: .hourAndMinute)
                .labelsHidden()
        case let .stepper(minimum, maximum):
            Stepper("\(integerValue(value, fallback: minimum))", value: integerBinding(field, value: value), in: minimum...maximum)
        }
    }

    private func visible(_ field: Field) -> Bool {
        guard let condition = field.visibleWhen else { return true }
        guard let value = store.fields[condition.field] ?? form?.fields.first(where: { $0.id == condition.field })?.value else { return false }
        return condition.matches(value)
    }

    private func stringBinding(_ field: Field, value: JSONValue) -> Binding<String> {
        Binding(get: { if case let .string(text) = store.fields[field.id] ?? value { text } else { "" } },
                set: { update(field, .string($0)) })
    }

    private func boolBinding(_ field: Field, value: JSONValue) -> Binding<Bool> {
        Binding(get: { if case let .bool(value) = store.fields[field.id] ?? value { value } else { false } },
                set: { update(field, .bool($0)) })
    }

    private func integerBinding(_ field: Field, value: JSONValue) -> Binding<Int64> {
        Binding(get: { integerValue(store.fields[field.id] ?? value, fallback: 0) },
                set: { update(field, .integer($0)) })
    }

    private func dateBinding(_ field: Field, value: JSONValue) -> Binding<Date> {
        Binding(get: {
            switch store.fields[field.id] ?? value {
            case .integer(let millis): return Date(timeIntervalSince1970: Double(millis) / 1000)
            case .string(let string) where field.kind == .time:
                let parts = string.split(separator: ":").compactMap { Int($0) }
                return Calendar.current.date(bySettingHour: parts.first ?? 9, minute: parts.dropFirst().first ?? 0, second: 0, of: .now) ?? .now
            case .string(let string):
                let formatter = ISO8601DateFormatter()
                return formatter.date(from: string) ?? .now
            default: return .now
            }
        }, set: { date in
            if field.kind == .time {
                let parts = Calendar.current.dateComponents([.hour, .minute], from: date)
                update(field, .string(String(format: "%02d:%02d", parts.hour ?? 9, parts.minute ?? 0)))
            } else {
                update(field, .integer(Int64((date.timeIntervalSince1970 * 1000).rounded())))
            }
        })
    }

    private func integerValue(_ value: JSONValue, fallback: Int64) -> Int64 {
        if case let .integer(number) = value { number } else { fallback }
    }

    private func update(_ field: Field, _ value: JSONValue) {
        var fields = store.fields
        fields[field.id] = value
        store.send(.fieldsChanged(fields))
        if field.reloadOnChange { onReload?(field, value) }
    }
}

public struct ActionCommentsEditor: View {
    public let comments: [ActionComment]
    public let kind: CommentKind
    public var onChange: ([ActionComment]) -> Void
    @State private var anchor = ""
    @State private var text = ""

    public init(comments: [ActionComment], kind: CommentKind = .blocks, onChange: @escaping ([ActionComment]) -> Void) {
        self.comments = comments
        self.kind = kind
        self.onChange = onChange
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(kind == .diffLines ? "Line comments" : "Comments").font(.subheadline.weight(.semibold))
            ForEach(Array(comments.enumerated()), id: \.offset) { index, comment in
                VStack(alignment: .leading, spacing: 4) {
                    Text(comment.anchor).font(.caption.monospaced()).foregroundStyle(.secondary)
                    Text(comment.text).font(.callout)
                    HStack {
                        Button("Edit") { anchor = comment.anchor; text = comment.text }
                        Button("Remove", role: .destructive) {
                            var updated = comments; updated.remove(at: index); onChange(updated)
                        }
                    }.buttonStyle(.plain).font(.caption)
                }.padding(8).frame(maxWidth: .infinity, alignment: .leading).background(.quaternary.opacity(0.35), in: .rect(cornerRadius: 8))
            }
            TextField("Anchor", text: $anchor).textFieldStyle(.roundedBorder)
            TextField("Comment", text: $text, axis: .vertical).textFieldStyle(.roundedBorder)
            Button(anchor.isEmpty ? "Add comment" : "Save comment") {
                guard !anchor.isEmpty, !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }
                var updated = comments
                if let index = updated.firstIndex(where: { $0.anchor == anchor }) {
                    updated[index] = ActionComment(anchor: anchor, text: text,
                                                   quote: updated[index].quote)
                } else {
                    updated.append(ActionComment(anchor: anchor, text: text))
                }
                onChange(updated); anchor = ""; text = ""
            }.disabled(anchor.isEmpty || text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
        }
    }
}
