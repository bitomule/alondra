import SwiftUI

/// One diff line: old and new numbers, the +/− mark, and the code, coloured when known, with what
/// changed inside the line marked more strongly.
public struct DiffCodeLine: View {
    let line: DiffLine
    let coloured: AttributedString?
    let wraps: Bool
    let digits: Int

    public init(line: DiffLine, coloured: AttributedString?, wraps: Bool = true, digits: Int = 3) {
        self.line = line
        self.coloured = coloured
        self.wraps = wraps
        self.digits = digits
    }

    public var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 0) {
            LineNumber(value: line.old, digits: digits)
            LineNumber(value: line.new, digits: digits)
            Text(verbatim: mark)
                .foregroundStyle(line.kind == .added ? Palette.sage : Palette.rose)
                .frame(width: 14)
                .accessibilityHidden(true)
            code
                .lineLimit(wraps ? nil : 1)
                .fixedSize(horizontal: !wraps, vertical: true)
            Spacer(minLength: 0)
        }
        .font(.caption.monospaced())
        .padding(.vertical, 1)
        .padding(.trailing, 8)
        .background(background)
    }

    @ViewBuilder
    private var code: some View {
        switch line.kind {
        case .hunk, .note:
            Text(verbatim: line.text)
                .foregroundStyle(.secondary)
        case .context, .added, .removed:
            Text(marked)
        }
    }

    private var marked: AttributedString {
        var text = coloured ?? AttributedString(line.text)
        guard let changed = line.changed, changed.upperBound <= text.characters.count else { return text }
        let start = text.characters.index(text.startIndex, offsetBy: changed.lowerBound)
        let end = text.characters.index(text.startIndex, offsetBy: changed.upperBound)
        text[start..<end].backgroundColor = (line.kind == .added ? Palette.sage : Palette.rose).opacity(0.32)
        return text
    }

    private var mark: String {
        switch line.kind {
        case .added: "+"
        case .removed: "−"
        case .hunk, .context, .note: ""
        }
    }

    private var background: Color {
        switch line.kind {
        case .added: Palette.sage.opacity(0.13)
        case .removed: Palette.rose.opacity(0.11)
        case .hunk: Palette.sky.opacity(0.09)
        case .context, .note: .clear
        }
    }
}

private struct LineNumber: View {
    let value: Int?
    let digits: Int

    var body: some View {
        Text(verbatim: String(repeating: "8", count: max(digits, 2)))
            .hidden()
            .overlay(alignment: .trailing) {
                Text(verbatim: value.map(String.init) ?? "")
                    .foregroundStyle(.tertiary)
            }
            .padding(.horizontal, 4)
            .accessibilityHidden(true)
    }
}
