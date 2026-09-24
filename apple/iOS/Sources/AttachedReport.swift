import AlondraShared
import SwiftUI

/// What the node reported about the PR it sits in, read right there without leaving the PR.
struct AttachedReport: View {
    let report: Report
    var showsBody = false

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Label("The node says", systemImage: "text.bubble.fill")
                .font(.caption.weight(.bold))
                .textCase(.uppercase)
                .foregroundStyle(Palette.sage)
            Text(report.title)
                .font(.headline)
                .foregroundStyle(Palette.ink)
                .fixedSize(horizontal: false, vertical: true)
            if showsBody, let body = report.body, !body.isEmpty {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(MarkdownParser.blocks(from: body)) { block in
                        MarkdownBlockView(block: block)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                }
                .foregroundStyle(Palette.ink.opacity(0.9))
            }
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Palette.sage.opacity(0.1), in: .rect(cornerRadius: 20, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 20, style: .continuous).strokeBorder(Palette.sage.opacity(0.35)))
    }
}
