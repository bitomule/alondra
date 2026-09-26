import AlondraShared
import ComposableArchitecture
import Foundation

public struct DiffHighlightingClient: Sendable {
    public var render: @Sendable ([DiffFile], Bool) async -> [String: AttributedString]
}

extension DiffHighlightingClient: DependencyKey {
    public static let liveValue = Self { files, dark in
        var lines: [String: AttributedString] = [:]
        for file in files {
            guard !Task.isCancelled else { return [:] }
            lines.merge(await DiffHighlighter.highlight(file, dark: dark)) { _, new in new }
        }
        return lines
    }
    public static let testValue = Self { _, _ in [:] }
}

extension DependencyValues {
    public var diffHighlighting: DiffHighlightingClient {
        get { self[DiffHighlightingClient.self] }
        set { self[DiffHighlightingClient.self] = newValue }
    }
}
