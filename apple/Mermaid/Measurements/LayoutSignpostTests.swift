import AlondraMermaid
import XCTest

/// `bazelisk test //apple/Mermaid:AlondraMermaidSignposts --test_output=all` prints how long the
/// `layout` signpost interval lasts for a 30-node flowchart, and for 100 nodes.
final class LayoutSignpostTests: XCTestCase {
    private static func flowchart(nodes: Int) -> Diagram {
        var lines = ["flowchart TD"]
        for index in 1..<nodes {
            lines.append("    n\(index)[Step \(index)] --> n\(index + 1)[Step \(index + 1)]")
            if index % 4 == 0, index > 4 { lines.append("    n\(index) -->|back| n\(index - 3)") }
            if index % 5 == 0, index + 3 <= nodes { lines.append("    n\(index) --> n\(index + 3)") }
        }
        return try! Mermaid.parse(lines.joined(separator: "\n"))
    }

    private func measureLayout(nodes: Int) {
        let diagram = Self.flowchart(nodes: nodes)
        let typography = MermaidTypography(size: 14)
        let options = XCTMeasureOptions()
        options.iterationCount = 20
        measure(
            metrics: [XCTOSSignpostMetric(subsystem: "com.bitomule.alondra.mermaid", category: "layout", name: "layout")],
            options: options
        ) {
            _ = Mermaid.layout(diagram, typography: typography)
        }
    }

    func test_layoutThirtyNodes() {
        measureLayout(nodes: 30)
    }

    func test_layoutHundredNodes() {
        measureLayout(nodes: 100)
    }
}
