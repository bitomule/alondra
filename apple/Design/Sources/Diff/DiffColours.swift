import Foundation
import Observation

/// The coloured text of a whole diff, filled file by file in the background so the list shows
/// plain code first and never waits for highlighting.
@MainActor
@Observable
public final class DiffColours {
    public private(set) var lines: [String: AttributedString] = [:]

    public init() {}

    public func load(_ files: [DiffFile], dark: Bool) async {
        lines = [:]
        for file in files {
            if Task.isCancelled { return }
            let coloured = await DiffHighlighter.highlight(file, dark: dark)
            lines.merge(coloured) { _, new in new }
        }
    }
}
