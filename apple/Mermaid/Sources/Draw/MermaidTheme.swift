import SwiftUI

/// A system font at a fixed point size. The caller scales the size for Dynamic Type before
/// laying out, so measuring and drawing use the same size.
public struct MermaidFont: Sendable, Hashable {
    public enum Weight: Sendable, Hashable {
        case regular, medium, semibold

        var coreText: CGFloat {
            switch self {
            case .regular: 0
            case .medium: 0.23
            case .semibold: 0.3
            }
        }

        var swiftUI: Font.Weight {
            switch self {
            case .regular: .regular
            case .medium: .medium
            case .semibold: .semibold
            }
        }
    }

    public var size: CGFloat
    public var weight: Weight

    public init(size: CGFloat, weight: Weight = .regular) {
        self.size = size
        self.weight = weight
    }

    var swiftUI: Font { .system(size: size, weight: weight.swiftUI) }
}

/// The fonts a layout is measured with. Changing them needs a new layout; changing colours does not.
public struct MermaidTypography: Sendable, Hashable {
    public var label: MermaidFont
    public var edgeLabel: MermaidFont
    public var title: MermaidFont

    public init(label: MermaidFont, edgeLabel: MermaidFont, title: MermaidFont) {
        self.label = label
        self.edgeLabel = edgeLabel
        self.title = title
    }

    /// Node labels at `size`, edge labels and titles a step smaller.
    public init(size: CGFloat) {
        self.init(
            label: MermaidFont(size: size),
            edgeLabel: MermaidFont(size: (size * 0.86).rounded()),
            title: MermaidFont(size: (size * 0.86).rounded(), weight: .semibold)
        )
    }

    func font(_ role: MermaidLayout.TextRole) -> MermaidFont {
        switch role {
        case .label: label
        case .strong: MermaidFont(size: label.size, weight: .semibold)
        case .edgeLabel: edgeLabel
        case .title: title
        }
    }
}

/// Colours and line widths a layout is drawn with, supplied by the app.
public struct MermaidTheme: Sendable {
    public var typography: MermaidTypography
    public var text: Color
    public var mutedText: Color
    public var nodeFill: Color
    public var nodeStroke: Color
    public var edge: Color
    public var labelBackground: Color
    public var clusterFill: Color
    public var clusterStroke: Color
    public var noteFill: Color
    public var noteStroke: Color
    public var accent: Color
    public var lineWidth: CGFloat

    public init(
        typography: MermaidTypography,
        text: Color,
        mutedText: Color,
        nodeFill: Color,
        nodeStroke: Color,
        edge: Color,
        labelBackground: Color,
        clusterFill: Color,
        clusterStroke: Color,
        noteFill: Color,
        noteStroke: Color,
        accent: Color,
        lineWidth: CGFloat = 1.25
    ) {
        self.typography = typography
        self.text = text
        self.mutedText = mutedText
        self.nodeFill = nodeFill
        self.nodeStroke = nodeStroke
        self.edge = edge
        self.labelBackground = labelBackground
        self.clusterFill = clusterFill
        self.clusterStroke = clusterStroke
        self.noteFill = noteFill
        self.noteStroke = noteStroke
        self.accent = accent
        self.lineWidth = lineWidth
    }

    func color(_ paint: MermaidLayout.Paint) -> Color {
        switch paint {
        case .text: text
        case .mutedText: mutedText
        case .nodeFill: nodeFill
        case .nodeStroke: nodeStroke
        case .edge: edge
        case .labelBackground: labelBackground
        case .clusterFill: clusterFill
        case .clusterStroke: clusterStroke
        case .noteFill: noteFill
        case .noteStroke: noteStroke
        case .accent: accent
        case .clear: .clear
        }
    }
}
