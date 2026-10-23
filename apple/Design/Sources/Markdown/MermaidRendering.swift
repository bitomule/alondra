import AlondraMermaid
import Foundation
import Synchronization
import SwiftUI

/// What a Mermaid block becomes: a laid-out diagram with what VoiceOver reads, or why it stays code.
public enum MermaidRendering: Sendable, Equatable {
    case diagram(MermaidLayout, MermaidSummary)
    case notice(MermaidNotice)

    public static func make(_ source: String, size: CGFloat) -> MermaidRendering {
        do {
            let diagram = try Mermaid.parse(source)
            return .diagram(Mermaid.layout(diagram, typography: MermaidTypography(size: size)), diagram.summary)
        } catch {
            switch error {
            case .syntax(let line, _, _): return .notice(.syntax(line: line))
            case .unsupported: return .notice(.unsupported)
            case .tooLarge: return .notice(.tooLarge)
            }
        }
    }
}

public enum MermaidNotice: Sendable, Equatable {
    case syntax(line: Int)
    case unsupported
    case tooLarge

    public var phrase: MermaidPhrase {
        switch self {
        case .syntax(let line): .syntaxError(line: line)
        case .unsupported: .unsupported
        case .tooLarge: .tooLarge
        }
    }
}

/// Renderings by source and text size, so scrolling back to a diagram or reopening its card never
/// lays it out again. Colours are not part of the key: light and dark draw the same layout.
public final class MermaidRenderCache: Sendable {
    public struct Key: Hashable, Sendable {
        public var source: String
        public var size: CGFloat

        public init(source: String, size: CGFloat) {
            self.source = source
            self.size = size
        }
    }

    public static let shared = MermaidRenderCache()
    private static let capacity = 64

    private let entries = Mutex<[Key: MermaidRendering]>([:])

    public init() {}

    public func cached(_ key: Key) -> MermaidRendering? {
        entries.withLock { $0[key] }
    }

    @concurrent
    public func rendering(_ key: Key) async -> MermaidRendering {
        if let cached = cached(key) { return cached }
        let rendering = MermaidRendering.make(key.source, size: key.size)
        entries.withLock { entries in
            if entries.count >= Self.capacity { entries.removeAll() }
            entries[key] = rendering
        }
        return rendering
    }
}

/// The diagram fits the width it is given, but its labels never shrink below 11 pt: from there on it
/// keeps that scale and scrolls sideways.
public enum MermaidScale {
    public static let smallestText: CGFloat = 11

    public static func fitting(_ diagramWidth: CGFloat, in available: CGFloat?, textSize: CGFloat) -> CGFloat {
        guard let available, available > 0, diagramWidth > available else { return 1 }
        return max(available / diagramWidth, min(1, smallestText / textSize))
    }
}

extension MermaidTheme {
    static func alondra(size: CGFloat) -> MermaidTheme {
        MermaidTheme(
            typography: MermaidTypography(size: size),
            text: Palette.ink,
            mutedText: Palette.slate,
            nodeFill: Palette.diagramNode,
            nodeStroke: Palette.slate.opacity(0.7),
            edge: Palette.slate,
            labelBackground: Palette.card,
            clusterFill: Palette.night.opacity(0.6),
            clusterStroke: Palette.slate.opacity(0.35),
            noteFill: Palette.gold.opacity(0.14),
            noteStroke: Palette.gold.opacity(0.6),
            accent: Palette.dawn
        )
    }
}
