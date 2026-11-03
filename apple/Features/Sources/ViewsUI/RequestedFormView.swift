import AlondraContract
import SwiftUI

/// The form an MCP server asks to fill, one control per field of the type it asks for. The values
/// go with the card's option that accepts the form.
public struct RequestedFormView: View {
    public let form: RequestedForm
    @Binding public var values: [String: JSONValue]

    public init(form: RequestedForm, values: Binding<[String: JSONValue]>) {
        self.form = form
        _values = values
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            ForEach(form.fields, id: \.name) { field in
                VStack(alignment: .leading, spacing: 6) {
                    if !field.isSwitch {
                        HStack(spacing: 6) {
                            Text(verbatim: field.shownTitle).font(.subheadline.weight(.semibold)).foregroundStyle(ViewPalette.ink)
                            if !field.required { Text("Optional").font(.caption).foregroundStyle(.secondary) }
                        }
                    }
                    control(field)
                    if let description = field.description {
                        Text(verbatim: description).font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                    }
                }
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier("requestedForm.\(field.name)")
            }
        }
    }

    @ViewBuilder private func control(_ field: RequestedField) -> some View {
        switch field.kind {
        case let .text(format, _, _, _):
            TextField(field.shownTitle, text: text(field))
                .requestedFieldStyle()
                #if os(iOS)
                .keyboardType(format == .email ? .emailAddress : format == .uri ? .URL : .default)
                .textInputAutocapitalization(format == .email || format == .uri ? .never : .sentences)
                #endif
        case let .number(integer, minimum, maximum, _):
            RequestedNumberField(title: field.shownTitle, integer: integer, minimum: minimum, maximum: maximum, value: $values[field.name])
        case .boolean:
            Toggle(isOn: Binding(get: { values[field.name] == .bool(true) }, set: { values[field.name] = .bool($0) })) {
                Text(verbatim: field.shownTitle).font(.subheadline.weight(.semibold)).foregroundStyle(ViewPalette.ink)
            }
        case let .choice(options, _):
            Picker(field.shownTitle, selection: choice(field)) {
                if !field.required || values[field.name] == nil { Text("Choose…").tag(String?.none) }
                ForEach(options, id: \.value) { option in Text(verbatim: option.shownTitle).tag(Optional(option.value)) }
            }
            .pickerStyle(.menu)
            .labelsHidden()
        case let .choices(options, _, _, _):
            VStack(alignment: .leading, spacing: 8) {
                ForEach(options, id: \.value) { option in
                    Toggle(isOn: picked(field, option.value)) { Text(verbatim: option.shownTitle) }
                }
            }
        }
    }

    private func text(_ field: RequestedField) -> Binding<String> {
        Binding(get: { if case let .string(text) = values[field.name] { text } else { "" } },
                set: { values[field.name] = $0.isEmpty ? nil : .string($0) })
    }

    private func choice(_ field: RequestedField) -> Binding<String?> {
        Binding(get: { if case let .string(value) = values[field.name] { value } else { nil } },
                set: { values[field.name] = $0.map(JSONValue.string) })
    }

    private func picked(_ field: RequestedField, _ option: String) -> Binding<Bool> {
        let current: [JSONValue] = if case let .array(items) = values[field.name] { items } else { [] }
        return Binding(get: { current.contains(.string(option)) }, set: { on in
            let next = on ? current + [.string(option)] : current.filter { $0 != .string(option) }
            values[field.name] = next.isEmpty ? nil : .array(next)
        })
    }
}

extension RequestedField {
    var isSwitch: Bool { if case .boolean = kind { true } else { false } }
}

/// A number typed as text: the value is set only while the text reads as a number of the field's kind.
struct RequestedNumberField: View {
    let title: String
    let integer: Bool
    let minimum: Double?
    let maximum: Double?
    @Binding var value: JSONValue?
    @State private var text = ""

    var body: some View {
        TextField(title, text: $text, prompt: Text(verbatim: range ?? title))
            .requestedFieldStyle()
            #if os(iOS)
            .keyboardType(integer ? .numberPad : .decimalPad)
            #endif
            .onAppear { text = Self.text(of: value) }
            .onChange(of: text) { _, typed in value = Self.value(of: typed, integer: integer) }
    }

    private var range: String? {
        switch (minimum, maximum) {
        case let (min?, max?): String(localized: "\(min.formatted()) to \(max.formatted())")
        case let (min?, nil): String(localized: "At least \(min.formatted())")
        case let (nil, max?): String(localized: "At most \(max.formatted())")
        default: nil
        }
    }

    static func text(of value: JSONValue?) -> String {
        switch value {
        case let .integer(n): String(n)
        case let .number(n): n.formatted(.number.grouping(.never))
        default: ""
        }
    }

    static func value(of text: String, integer: Bool) -> JSONValue? {
        let typed = text.trimmingCharacters(in: .whitespaces)
        if integer { return Int64(typed).map(JSONValue.integer) }
        return (try? Double(typed, format: .number)).map(JSONValue.number)
    }
}

extension View {
    @ViewBuilder func requestedFieldStyle() -> some View {
        #if os(iOS)
        padding(.horizontal, 14).padding(.vertical, 11)
            .background(ViewPalette.card.opacity(0.9), in: .rect(cornerRadius: 12, style: .continuous))
        #else
        textFieldStyle(.roundedBorder)
        #endif
    }
}
