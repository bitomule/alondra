import AppKit
import XCTest

@MainActor
func alignSnapshotViewportToTop(_ view: NSView) {
    view.layoutSubtreeIfNeeded()
    func reset(_ view: NSView) {
        if let scroll = view as? NSScrollView, let document = scroll.documentView {
            let clip = scroll.contentView
            let top = document.isFlipped ? document.bounds.minY : max(document.bounds.minY, document.bounds.maxY - clip.bounds.height)
            clip.scroll(to: CGPoint(x: document.bounds.minX, y: top))
            scroll.reflectScrolledClipView(clip)
        }
        for child in view.subviews { reset(child) }
    }
    reset(view)
}

private final class FlippedSnapshotDocument: NSView {
    override var isFlipped: Bool { true }
}

@MainActor
final class SnapshotViewportTests: XCTestCase {
    func testExplicitTopViewportReplacesAStaleOffsetForEitherDocumentOrientation() {
        for flipped in [false, true] {
            let scroll = NSScrollView(frame: CGRect(x: 0, y: 0, width: 400, height: 300))
            let document = flipped ? FlippedSnapshotDocument(frame: CGRect(x: 0, y: 0, width: 600, height: 2_000))
                                   : NSView(frame: CGRect(x: 0, y: 0, width: 600, height: 2_000))
            scroll.documentView = document
            scroll.contentView.scroll(to: CGPoint(x: 35, y: 400))
            XCTAssertEqual(scroll.contentView.bounds.origin.y, 400)
            alignSnapshotViewportToTop(scroll)
            XCTAssertEqual(scroll.contentView.bounds.origin.x, 0)
            XCTAssertEqual(scroll.contentView.bounds.origin.y, flipped ? 0 : document.bounds.maxY - scroll.contentView.bounds.height)
            XCTAssertEqual(document.frame.height, 2_000, "alignment must not crop or replace the document")
        }
    }
}
