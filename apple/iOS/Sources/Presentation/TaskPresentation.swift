import AlondraShared
import SwiftUI

struct StateChip: View {
    let label: String
    let tint: Color

    var body: some View {
        Text(label)
            .font(.caption.weight(.semibold))
            .foregroundStyle(tint)
            .padding(.horizontal, 9)
            .padding(.vertical, 4)
            .background(tint.opacity(0.14), in: .capsule)
            .lineLimit(1)
            .fixedSize()
            .padding(.bottom, 2)
    }
}

struct StateLine: View {
    let label: String
    let tint: Color
    let since: Int64

    var body: some View {
        HStack(spacing: 4) {
            Text(label)
                .foregroundStyle(tint)
            Text(verbatim: "· \(ago(since))")
                .foregroundStyle(.tertiary)
        }
        .font(.caption.weight(.semibold))
        .lineLimit(1)
    }
}

struct NowLine: View {
    let text: String

    var body: some View {
        Text(text)
            .font(.subheadline)
            .foregroundStyle(.secondary)
            .lineLimit(2)
            .multilineTextAlignment(.leading)
    }
}
