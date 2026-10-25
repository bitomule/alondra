import SwiftUI

/// A text field that shows `lines.lowerBound` lines when empty and grows up to `lines.upperBound`.
///
/// On the Mac, SwiftUI turns a `lineLimit` range into heights through a line-height cache keyed by
/// the font's address without retaining it, so a new font can take a freed font's line height and
/// the field comes out taller. There the field is plain, its minimum height comes from the line
/// height of the font it resolves to, and the bezel, which would not stretch to it, is drawn here.
/// A plain field draws its placeholder as dark as typed text, so the placeholder is drawn here too.
public struct MultilineTextField: View {
    private let title: String
    @Binding private var text: String
    private let lines: ClosedRange<Int>
    private let focus: FocusState<Bool>.Binding?
    @FocusState private var ownFocus: Bool

    public init(_ title: String, text: Binding<String>, lines: ClosedRange<Int>, focus: FocusState<Bool>.Binding? = nil) {
        self.title = title
        _text = text
        self.lines = lines
        self.focus = focus
    }

    private var focused: FocusState<Bool>.Binding { focus ?? $ownFocus }

    #if os(macOS)
    @Environment(\.font) private var font
    @Environment(\.fontResolutionContext) private var fontContext

    private var lineHeight: CGFloat {
        NSLayoutManager().defaultLineHeight(for: (font ?? .body).resolve(in: fontContext).ctFont as NSFont)
    }

    private let bezel = RoundedRectangle(cornerRadius: 5, style: .continuous)

    public var body: some View {
        ZStack(alignment: .topLeading) {
            Text(verbatim: title)
                .foregroundStyle(Color(nsColor: .placeholderTextColor))
                .opacity(text.isEmpty ? 1 : 0)
                .accessibilityHidden(true)
            TextField(title, text: $text, prompt: Text(verbatim: ""), axis: .vertical)
                .textFieldStyle(.plain)
                .lineLimit(lines.upperBound)
                .focused(focused)
        }
        .frame(minHeight: CGFloat(lines.lowerBound) * lineHeight, alignment: .topLeading)
        .padding(.vertical, 4)
        .padding(.horizontal, 6)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background {
            bezel.fill(Color(nsColor: .textBackgroundColor))
                .overlay { bezel.strokeBorder(Color(nsColor: .separatorColor)) }
                .padding(-1)
        }
        .overlay {
            if focused.wrappedValue {
                RoundedRectangle(cornerRadius: 7, style: .continuous)
                    .strokeBorder(Color.accentColor.opacity(0.5), lineWidth: 3)
                    .padding(-3)
            }
        }
    }
    #else
    public var body: some View {
        TextField(title, text: $text, axis: .vertical)
            .lineLimit(lines)
            .focused(focused)
    }
    #endif
}
