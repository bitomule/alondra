import CoreGraphics

/// Where a line from a node's centre leaves the node's outline.
enum Boundary {
    /// 1 on the outline, below 1 inside, above 1 outside.
    static func norm(_ point: CGPoint, frame: CGRect, outline: MermaidLayout.Outline) -> CGFloat {
        let halfWidth = max(frame.width / 2, 0.001)
        let halfHeight = max(frame.height / 2, 0.001)
        let x = abs(point.x - frame.midX) / halfWidth
        let y = abs(point.y - frame.midY) / halfHeight
        switch outline {
        case .diamond: return x + y
        case .circle, .doubleCircle: return (x * x + y * y).squareRoot()
        default: return max(x, y)
        }
    }

    static func clip(from center: CGPoint, toward target: CGPoint, frame: CGRect, outline: MermaidLayout.Outline) -> CGPoint {
        let dx = target.x - center.x
        let dy = target.y - center.y
        guard dx != 0 || dy != 0 else { return center }
        let unit = norm(CGPoint(x: frame.midX + dx, y: frame.midY + dy), frame: frame, outline: outline)
        guard unit > 0 else { return center }
        return CGPoint(x: center.x + dx / unit, y: center.y + dy / unit)
    }
}
