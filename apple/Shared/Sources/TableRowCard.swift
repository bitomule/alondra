import Foundation

/// A table row drawn as a card when the table is too wide for its grid: the first cell is the
/// title and every other filled cell sits under its column name.
public struct TableRowCard: Hashable, Sendable {
    public struct Field: Hashable, Sendable, Identifiable {
        public let column: Int
        public let name: AttributedString
        public let value: AttributedString

        public var id: Int { column }
    }

    public let title: AttributedString
    public let fields: [Field]

    public init(_ row: MarkdownBlock.TableRow) {
        title = row.cells.first ?? AttributedString()
        fields = row.cells.indices.dropFirst().compactMap { column in
            let value = row.cells[column]
            guard !value.characters.allSatisfy(\.isWhitespace) else { return nil }
            let name = row.header.indices.contains(column) ? row.header[column] : AttributedString()
            return Field(column: column, name: name, value: value)
        }
    }
}
