import SwiftUI

/// Draws a laid-out diagram at its natural size with the theme's colours.
public struct MermaidCanvas: View {
    private let layout: MermaidLayout
    private let theme: MermaidTheme

    public init(layout: MermaidLayout, theme: MermaidTheme) {
        self.layout = layout
        self.theme = theme
    }

    public var body: some View {
        Canvas { context, _ in
            for item in layout.items {
                draw(item, in: &context)
            }
        }
        .frame(width: layout.size.width, height: layout.size.height)
    }

    private func draw(_ item: MermaidLayout.Item, in context: inout GraphicsContext) {
        switch item {
        case .shape(let shape): draw(shape, in: &context)
        case .line(let line): draw(line, in: &context)
        case .text(let text): draw(text, in: &context)
        case .actor(let frame, let paint): drawActor(frame, color: theme.color(paint), in: &context)
        }
    }

    private func style(_ dash: MermaidLayout.Dash) -> StrokeStyle {
        let width = theme.lineWidth
        switch dash {
        case .solid: return StrokeStyle(lineWidth: width, lineCap: .round, lineJoin: .round)
        case .dashed: return StrokeStyle(lineWidth: width, lineCap: .butt, lineJoin: .round, dash: [5, 4])
        case .dotted: return StrokeStyle(lineWidth: width, lineCap: .round, lineJoin: .round, dash: [1, 4])
        case .thick: return StrokeStyle(lineWidth: width * 2.4, lineCap: .round, lineJoin: .round)
        }
    }

    // MARK: Shapes

    private func draw(_ shape: MermaidLayout.Shape, in context: inout GraphicsContext) {
        let path = Self.outline(shape.outline, in: shape.frame)
        context.fill(path, with: .color(theme.color(shape.fill)))
        context.stroke(path, with: .color(theme.color(shape.stroke)), style: style(shape.dash))
        switch shape.outline {
        case .subroutine:
            var inner = Path()
            for x in [shape.frame.minX + 8, shape.frame.maxX - 8] {
                inner.move(to: CGPoint(x: x, y: shape.frame.minY))
                inner.addLine(to: CGPoint(x: x, y: shape.frame.maxY))
            }
            context.stroke(inner, with: .color(theme.color(shape.stroke)), style: style(.solid))
        case .cylinder:
            let rim = Path(ellipseIn: CGRect(x: shape.frame.minX, y: shape.frame.minY, width: shape.frame.width, height: 14))
            context.stroke(rim, with: .color(theme.color(shape.stroke)), style: style(.solid))
        case .doubleCircle:
            let inner = Path(ellipseIn: shape.frame.insetBy(dx: 4, dy: 4))
            context.fill(inner, with: .color(theme.color(shape.stroke)))
            let ring = Path(ellipseIn: shape.frame.insetBy(dx: 2, dy: 2))
            context.stroke(ring, with: .color(theme.nodeFill), style: StrokeStyle(lineWidth: 1.5))
        default:
            break
        }
    }

    static func outline(_ outline: MermaidLayout.Outline, in frame: CGRect) -> Path {
        switch outline {
        case .rectangle, .subroutine, .bar:
            return Path(frame)
        case .rounded:
            return Path(roundedRect: frame, cornerRadius: min(8, frame.height / 3))
        case .stadium:
            return Path(roundedRect: frame, cornerRadius: frame.height / 2)
        case .circle, .doubleCircle:
            return Path(ellipseIn: frame)
        case .diamond:
            var path = Path()
            path.move(to: CGPoint(x: frame.midX, y: frame.minY))
            path.addLine(to: CGPoint(x: frame.maxX, y: frame.midY))
            path.addLine(to: CGPoint(x: frame.midX, y: frame.maxY))
            path.addLine(to: CGPoint(x: frame.minX, y: frame.midY))
            path.closeSubpath()
            return path
        case .hexagon:
            let inset = min(frame.height / 2, frame.width / 4)
            var path = Path()
            path.move(to: CGPoint(x: frame.minX + inset, y: frame.minY))
            path.addLine(to: CGPoint(x: frame.maxX - inset, y: frame.minY))
            path.addLine(to: CGPoint(x: frame.maxX, y: frame.midY))
            path.addLine(to: CGPoint(x: frame.maxX - inset, y: frame.maxY))
            path.addLine(to: CGPoint(x: frame.minX + inset, y: frame.maxY))
            path.addLine(to: CGPoint(x: frame.minX, y: frame.midY))
            path.closeSubpath()
            return path
        case .cylinder:
            let rim: CGFloat = 7
            var path = Path()
            path.move(to: CGPoint(x: frame.minX, y: frame.minY + rim))
            path.addLine(to: CGPoint(x: frame.minX, y: frame.maxY - rim))
            path.addQuadCurve(to: CGPoint(x: frame.maxX, y: frame.maxY - rim), control: CGPoint(x: frame.midX, y: frame.maxY + rim))
            path.addLine(to: CGPoint(x: frame.maxX, y: frame.minY + rim))
            path.addQuadCurve(to: CGPoint(x: frame.minX, y: frame.minY + rim), control: CGPoint(x: frame.midX, y: frame.minY - rim))
            path.closeSubpath()
            return path
        case .note:
            let fold: CGFloat = 8
            var path = Path()
            path.move(to: CGPoint(x: frame.minX, y: frame.minY))
            path.addLine(to: CGPoint(x: frame.maxX - fold, y: frame.minY))
            path.addLine(to: CGPoint(x: frame.maxX, y: frame.minY + fold))
            path.addLine(to: CGPoint(x: frame.maxX, y: frame.maxY))
            path.addLine(to: CGPoint(x: frame.minX, y: frame.maxY))
            path.closeSubpath()
            return path
        }
    }

    private func drawActor(_ frame: CGRect, color: Color, in context: inout GraphicsContext) {
        var path = Path()
        let head = frame.width * 0.38
        path.addEllipse(in: CGRect(x: frame.midX - head / 2, y: frame.minY, width: head, height: head))
        let neck = CGPoint(x: frame.midX, y: frame.minY + head)
        let hip = CGPoint(x: frame.midX, y: frame.minY + frame.height * 0.66)
        path.move(to: neck)
        path.addLine(to: hip)
        path.move(to: CGPoint(x: frame.minX, y: frame.minY + head + 6))
        path.addLine(to: CGPoint(x: frame.maxX, y: frame.minY + head + 6))
        path.move(to: CGPoint(x: frame.minX + 2, y: frame.maxY))
        path.addLine(to: hip)
        path.addLine(to: CGPoint(x: frame.maxX - 2, y: frame.maxY))
        context.stroke(path, with: .color(color), style: style(.solid))
    }

    // MARK: Lines

    private func draw(_ line: MermaidLayout.Line, in context: inout GraphicsContext) {
        guard line.points.count >= 2 else { return }
        var points = line.points
        let color = theme.color(line.paint)
        let headLength = 8 + theme.lineWidth * 2
        if line.end != .none { points[points.count - 1] = Self.pulled(points[points.count - 1], from: points[points.count - 2], by: Self.inset(line.end, headLength)) }
        if line.start != .none { points[0] = Self.pulled(points[0], from: points[1], by: Self.inset(line.start, headLength)) }
        var path = Path()
        path.move(to: points[0])
        if line.curved, points.count > 2 {
            for index in 1..<(points.count - 1) {
                let next = points[index + 1]
                let middle = index == points.count - 2 ? next : CGPoint(x: (points[index].x + next.x) / 2, y: (points[index].y + next.y) / 2)
                path.addQuadCurve(to: middle, control: points[index])
            }
        } else {
            for point in points.dropFirst() { path.addLine(to: point) }
        }
        context.stroke(path, with: .color(color), style: style(line.dash))
        drawMarker(line.end, tip: line.points[line.points.count - 1], from: line.points[line.points.count - 2], length: headLength,
                   color: color, in: &context)
        drawMarker(line.start, tip: line.points[0], from: line.points[1], length: headLength, color: color, in: &context)
    }

    private static func inset(_ marker: MermaidLayout.Marker, _ length: CGFloat) -> CGFloat {
        switch marker {
        case .none, .openArrow, .cross: 0
        case .arrow, .triangle: length - 1
        case .circle: 8
        case .diamond, .filledDiamond: length * 1.6
        }
    }

    private static func pulled(_ point: CGPoint, from other: CGPoint, by distance: CGFloat) -> CGPoint {
        let dx = other.x - point.x
        let dy = other.y - point.y
        let length = (dx * dx + dy * dy).squareRoot()
        guard length > distance, distance > 0 else { return point }
        return CGPoint(x: point.x + dx / length * distance, y: point.y + dy / length * distance)
    }

    private func drawMarker(
        _ marker: MermaidLayout.Marker, tip: CGPoint, from: CGPoint, length: CGFloat, color: Color, in context: inout GraphicsContext
    ) {
        guard marker != .none else { return }
        let dx = tip.x - from.x
        let dy = tip.y - from.y
        let norm = max((dx * dx + dy * dy).squareRoot(), 0.001)
        let along = CGPoint(x: dx / norm, y: dy / norm)
        let side = CGPoint(x: -along.y, y: along.x)
        func at(_ back: CGFloat, _ across: CGFloat) -> CGPoint {
            CGPoint(x: tip.x - along.x * back + side.x * across, y: tip.y - along.y * back + side.y * across)
        }
        let half = length * 0.45
        var path = Path()
        switch marker {
        case .none:
            return
        case .arrow, .triangle:
            path.move(to: tip)
            path.addLine(to: at(length, half))
            path.addLine(to: at(length, -half))
            path.closeSubpath()
            context.fill(path, with: .color(marker == .arrow ? color : theme.nodeFill))
            context.stroke(path, with: .color(color), style: style(.solid))
        case .openArrow:
            path.move(to: at(length, half))
            path.addLine(to: tip)
            path.addLine(to: at(length, -half))
            context.stroke(path, with: .color(color), style: style(.solid))
        case .cross:
            path.move(to: at(length, half))
            path.addLine(to: at(0, -half))
            path.move(to: at(length, -half))
            path.addLine(to: at(0, half))
            context.stroke(path, with: .color(color), style: style(.solid))
        case .circle:
            let center = at(4, 0)
            path.addEllipse(in: CGRect(x: center.x - 4, y: center.y - 4, width: 8, height: 8))
            context.fill(path, with: .color(theme.nodeFill))
            context.stroke(path, with: .color(color), style: style(.solid))
        case .diamond, .filledDiamond:
            let long = length * 1.6
            path.move(to: tip)
            path.addLine(to: at(long / 2, half))
            path.addLine(to: at(long, 0))
            path.addLine(to: at(long / 2, -half))
            path.closeSubpath()
            context.fill(path, with: .color(marker == .filledDiamond ? color : theme.nodeFill))
            context.stroke(path, with: .color(color), style: style(.solid))
        }
    }

    // MARK: Text

    private func draw(_ text: MermaidLayout.Text, in context: inout GraphicsContext) {
        if let background = text.background {
            context.fill(Path(roundedRect: background, cornerRadius: 4), with: .color(theme.labelBackground))
        }
        let font = theme.typography.font(text.role).swiftUI
        let color = theme.color(text.paint)
        let top = text.position.y - text.lineHeight * CGFloat(text.lines.count) / 2
        for (index, line) in text.lines.enumerated() {
            let y = top + text.lineHeight * (CGFloat(index) + 0.5)
            let resolved = context.resolve(Text(verbatim: line).font(font).foregroundStyle(color))
            switch text.alignment {
            case .center: context.draw(resolved, at: CGPoint(x: text.position.x, y: y), anchor: .center)
            case .leading: context.draw(resolved, at: CGPoint(x: text.position.x, y: y), anchor: .leading)
            }
        }
    }
}
