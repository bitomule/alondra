import SwiftUI

/// Five squares split between added and removed lines in proportion, as GitHub draws them.
public struct DiffStatBar: View {
    let additions: Int
    let deletions: Int

    public init(additions: Int, deletions: Int) {
        self.additions = additions
        self.deletions = deletions
    }

    public static let squares = 5

    public static func split(additions: Int, deletions: Int) -> (added: Int, removed: Int) {
        let total = additions + deletions
        guard total > 0 else { return (0, 0) }
        let added = Int((Double(additions) / Double(total) * Double(squares)).rounded())
        let removed = min(squares - added, Int((Double(deletions) / Double(total) * Double(squares)).rounded()))
        return (added, removed)
    }

    public var body: some View {
        let (added, removed) = Self.split(additions: additions, deletions: deletions)
        HStack(spacing: 2) {
            ForEach(0..<Self.squares, id: \.self) { index in
                RoundedRectangle(cornerRadius: 1.5, style: .continuous)
                    .fill(index < added ? Palette.sage : index < added + removed ? Palette.rose : Palette.hairline)
                    .frame(width: 7, height: 7)
            }
        }
        .accessibilityHidden(true)
    }
}
