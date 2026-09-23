import SwiftUI

/// One diff line: old and new numbers, the +/− mark, and the code, coloured when known.
public struct DiffCodeLine: View {
    let line: DiffLine
    let coloured: AttributedString?

    public init(line: DiffLine, coloured: AttributedString?) {
        self.line = line
        self.coloured = coloured
    }

    public var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 0) {
            LineNumber(value: line.old)
            LineNumber(value: line.new)
            Text(verbatim: mark)
                .foregroundStyle(line.kind == .added ? .green : .red)
                .frame(width: 14)
            code
                .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 0)
        }
        .font(.caption.monospaced())
        .padding(.vertical, 1)
        .background(background)
    }

    @ViewBuilder
    private var code: some View {
        switch line.kind {
        case .hunk, .note:
            Text(verbatim: line.text)
                .foregroundStyle(.secondary)
        case .context, .added, .removed:
            if let coloured {
                Text(coloured)
            } else {
                Text(verbatim: line.text)
            }
        }
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
        case .added: .green.opacity(0.14)
        case .removed: .red.opacity(0.14)
        case .hunk: .blue.opacity(0.08)
        case .context, .note: .clear
        }
    }
}

private struct LineNumber: View {
    let value: Int?

    var body: some View {
        Text(verbatim: value.map(String.init) ?? "")
            .foregroundStyle(.tertiary)
            .frame(width: 36, alignment: .trailing)
            .padding(.trailing, 4)
    }
}
