import AlondraAttachmentsFeature
import AlondraViews
import ComposableArchitecture
import SwiftUI

/// Reusable editor for the server-declared inputs associated with a view action.
public struct ActionInputView: View {
    @Bindable public var store: StoreOf<ActionFeature>
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
                input(action.options.first(where: { $0.id == store.selectedOptionID })?.input ?? action.input)
                if action.attachments || (form?.fields.contains(where: { if case .attachments = $0.kind { true } else { false } }) == true) {
                    AttachmentPicker(store: attachmentStore)
                }
                if let commentSpec = action.comments {
                    if showCommentEditor {
                        ActionCommentsEditor(comments: store.comments, kind: commentSpec.kind) { store.send(.commentsChanged($0)) }
                    } else if !store.comments.isEmpty {
                        VStack(alignment: .leading, spacing: 8) {
                            Text("Your comments").font(.subheadline.weight(.semibold))
                            ForEach(store.comments, id: \.anchor) { comment in
                                Text(comment.text).font(.callout)
                                    .frame(maxWidth: .infinity, alignment: .leading)
                                    .padding(8).background(.quaternary.opacity(0.35), in: .rect(cornerRadius: 8))
                            }
                        }
                    }
                }
                if let form, form.id == "schedule" {
                    scheduleForm(form.fields)
                } else if let form {
                    formFields(form.fields)
                }
                if let error = store.error { Text(error).font(.callout).foregroundStyle(.red) }
                if let stamp = store.stamp { Label(stamp, systemImage: "checkmark.circle.fill").foregroundStyle(.green) }
            }
        }
    }

    private var attachmentStore: StoreOf<AttachmentFeature> {
        store.scope(state: \.attachmentPicker, action: \.attachmentPicker)
    }

    @ViewBuilder private func input(_ input: AlondraViews.Input) -> some View {
        switch input {
        case .none: EmptyView()
        case let .text(placeholder, _, multiline):
            if multiline {
                TextEditor(text: Binding(get: { store.text }, set: { store.send(.textChanged($0)) }))
                    .frame(minHeight: 110).overlay(RoundedRectangle(cornerRadius: 8).stroke(.quaternary))
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

    private func scheduleForm(_ fields: [Field]) -> some View {
        let mission = fields.filter { ["name", "mission", "project"].contains($0.id) }
        let timing = fields.filter { ["repeat", "weekday", "interval", "start_at", "time", "active"].contains($0.id) }
        let advanced = fields.filter { $0.id == "disallowed_tools" }
        let launch = fields.filter { !["name", "mission", "project", "repeat", "weekday", "interval", "start_at", "time", "active", "disallowed_tools"].contains($0.id) }
        return Form {
            Section("Mission") { formFields(mission) }
            Section("When") { formFields(timing) }
            if !launch.isEmpty { Section("Launch options") { formFields(launch) } }
            if !advanced.isEmpty { Section("Advanced") { formFields(advanced) } }
        }
        .formStyle(.grouped)
        .frame(minHeight: 260)
    }

    @ViewBuilder private func fieldControl(_ field: Field) -> some View {
        let value = store.fields[field.id] ?? field.value
        switch field.kind {
        case let .text(placeholder), let .multiline(placeholder), let .lines(placeholder):
            if case .multiline = field.kind {
                TextField(placeholder ?? "", text: stringBinding(field, value: value), axis: .vertical)
                    .lineLimit(4...8)
            } else if case .lines = field.kind {
                TextField(placeholder ?? "", text: stringBinding(field, value: value), axis: .vertical)
                    .lineLimit(2...6)
            } else {
                TextField(placeholder ?? "", text: stringBinding(field, value: value))
            }
        case .attachments:
            EmptyView()
        case .toggle:
            Toggle(field.label, isOn: boolBinding(field, value: value)).labelsHidden()
        case let .choice(items):
            Picker(field.label, selection: stringBinding(field, value: value)) {
                ForEach(items, id: \.id) { item in Text(item.label).tag(item.id) }
            }.labelsHidden()
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
                TextField(source, text: stringBinding(field, value: value)).textFieldStyle(.roundedBorder)
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
