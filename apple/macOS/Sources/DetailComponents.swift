import AlondraShared
import SwiftUI

struct DetailScaffold<Content: View, Bar: View>: View {
    @ViewBuilder let content: Content
    @ViewBuilder let bar: Bar

    var body: some View {
        ScrollView {
            content
                .padding(.horizontal, 44)
                .padding(.top, 26)
                .padding(.bottom, 28)
                .frame(maxWidth: 800, alignment: .leading)
                .frame(maxWidth: .infinity)
        }
        .scrollEdgeEffectStyle(.soft, for: .bottom)
        .safeAreaInset(edge: .bottom, spacing: 0) {
            bar
                .padding(.horizontal, 44)
                .padding(.top, 12)
                .padding(.bottom, 16)
                .frame(maxWidth: 800)
                .frame(maxWidth: .infinity)
                .background(.bar)
                .overlay(alignment: .top) { Divider() }
        }
    }
}

struct DetailHeader: View {
    let style: KindStyle
    let node: String?
    let date: Int64
    let title: String

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                KindChip(style: style)
                Text("\(node ?? String(localized: "unknown node")) · \(ago(date)) ago")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            Text(title)
                .font(.title.weight(.semibold))
                .fixedSize(horizontal: false, vertical: true)
                .textSelection(.enabled)
                .accessibilityAddTraits(.isHeader)
        }
    }
}

struct KindChip: View {
    let style: KindStyle

    var body: some View {
        Label(style.label, systemImage: style.symbol)
            .font(.caption.weight(.semibold))
            .foregroundStyle(style.tint)
            .padding(.horizontal, 8)
            .padding(.vertical, 3)
            .background(style.tint.opacity(0.13), in: .capsule)
    }
}

struct KeyCap: View {
    let key: String
    var onAccent = false

    var body: some View {
        Text(key)
            .font(.caption.monospaced().weight(.semibold))
            .foregroundStyle(onAccent ? AnyShapeStyle(.white.opacity(0.9)) : AnyShapeStyle(.secondary))
            .frame(minWidth: 18, minHeight: 18)
            .background(onAccent ? AnyShapeStyle(.white.opacity(0.18)) : AnyShapeStyle(.quaternary.opacity(0.6)), in: .rect(cornerRadius: 4))
            .accessibilityHidden(true)
    }
}

struct OptionButtonStyle: ButtonStyle {
    var prominent = false
    var selected = false

    func makeBody(configuration: Configuration) -> some View {
        OptionButtonBody(configuration: configuration, prominent: prominent, selected: selected)
    }
}

private struct OptionButtonBody: View {
    let configuration: ButtonStyleConfiguration
    let prominent: Bool
    let selected: Bool
    @State private var hovering = false
    @Environment(\.isEnabled) private var isEnabled

    var body: some View {
        configuration.label
            .padding(.horizontal, 12)
            .padding(.vertical, 9)
            .frame(maxWidth: .infinity, minHeight: 46, alignment: .leading)
            .foregroundStyle(prominent ? AnyShapeStyle(.white) : AnyShapeStyle(.primary))
            .background(fill, in: .rect(cornerRadius: 10))
            .overlay {
                RoundedRectangle(cornerRadius: 10)
                    .strokeBorder(selected ? Color.accentColor : Color.primary.opacity(prominent ? 0 : 0.07), lineWidth: selected ? 2 : 1)
            }
            .contentShape(.rect(cornerRadius: 10))
            .scaleEffect(configuration.isPressed ? 0.985 : 1)
            .opacity(isEnabled ? 1 : 0.5)
            .onHover { hovering = $0 }
            .animation(.snappy(duration: 0.12), value: configuration.isPressed)
            .animation(.snappy(duration: 0.12), value: hovering)
    }

    private var fill: Color {
        if prominent {
            return Color.accentColor.opacity(configuration.isPressed ? 0.8 : hovering ? 0.92 : 1)
        }
        return Color.primary.opacity(configuration.isPressed ? 0.13 : hovering ? 0.085 : 0.055)
    }
}

struct OptionButton: View {
    let index: Int
    let label: String
    let detail: String?
    var prominent = false
    var selected = false
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 10) {
                KeyCap(key: String(index + 1), onAccent: prominent)
                VStack(alignment: .leading, spacing: 1) {
                    Text(label)
                        .font(.body.weight(.semibold))
                    if let detail, !detail.isEmpty {
                        Text(detail)
                            .font(.caption)
                            .opacity(0.75)
                            .lineLimit(2)
                    }
                }
                Spacer(minLength: 0)
            }
        }
        .buttonStyle(OptionButtonStyle(prominent: prominent, selected: selected))
        .help("Key \(index + 1)")
    }
}

struct Composer: View {
    let placeholder: String
    @Binding var text: String
    var focus: FocusState<DetailField?>.Binding
    let sendTitle: String?
    let canSend: Bool
    let send: () -> Void
    var escape: () -> Void = {}

    var body: some View {
        HStack(alignment: .bottom, spacing: 8) {
            TextField(placeholder, text: $text, axis: .vertical)
                .textFieldStyle(.plain)
                .lineLimit(1...8)
                .focused(focus, equals: .reply)
                .onSubmit { if canSend { send() } }
                .onExitCommand {
                    focus.wrappedValue = .pane
                    escape()
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 8)
                .background(.background, in: .rect(cornerRadius: 10))
                .overlay {
                    RoundedRectangle(cornerRadius: 10)
                        .strokeBorder(focus.wrappedValue == .reply ? Color.accentColor.opacity(0.7) : Color.primary.opacity(0.12))
                }
            if let sendTitle, canSend {
                Button(action: send) {
                    Label(sendTitle, systemImage: "arrow.up")
                        .labelStyle(.titleAndIcon)
                        .padding(.horizontal, 4)
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
                .keyboardShortcut(.return, modifiers: .command)
                .help(Text(verbatim: "\(sendTitle) (⌘↩)"))
                .transition(.opacity.combined(with: .scale(scale: 0.9, anchor: .trailing)))
            }
        }
        .animation(.snappy(duration: 0.18), value: canSend)
    }
}

struct Hint: View {
    let text: String

    var body: some View {
        Text(text)
            .font(.caption)
            .foregroundStyle(.tertiary)
            .lineLimit(1)
    }
}

struct LinksRow: View {
    let links: [LinkRef]
    let attachments: [Attachment]

    var body: some View {
        if !links.isEmpty || !attachments.isEmpty {
            VStack(alignment: .leading, spacing: 6) {
                ForEach(links, id: \.self) { link in
                    if let url = URL(string: link.url) {
                        SwiftUI.Link(destination: url) {
                            Label(link.title ?? link.url, systemImage: "arrow.up.right.square")
                                .lineLimit(1)
                        }
                        .help(link.url)
                    }
                }
                ForEach(attachments, id: \.self) { file in
                    Button {
                        NSWorkspace.shared.open(URL(fileURLWithPath: file.storedPath ?? file.sourcePath))
                    } label: {
                        Label(file.name, systemImage: "paperclip")
                            .lineLimit(1)
                    }
                    .buttonStyle(.link)
                    .help(file.storedPath ?? file.sourcePath)
                }
            }
            .font(.callout)
        }
    }
}
