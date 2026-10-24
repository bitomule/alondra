import AppKit
import XCTest

/// A window placed beyond every screen, so the real pointer can never hover a control in a capture.
/// AppKit pulls a titled window back on screen when it is created and when it is shown; this one
/// is put back after it is created and stays there when shown.
private final class OffScreenSnapshotWindow: NSWindow {
    override func constrainFrameRect(_ frameRect: NSRect, to screen: NSScreen?) -> NSRect { frameRect }
}

@MainActor
func snapshotWindow(size: CGSize, styleMask: NSWindow.StyleMask = [.borderless]) -> NSWindow {
    let screens = NSScreen.screens.map(\.frame).reduce(CGRect.null) { $0.union($1) }
    let origin = CGPoint(x: (screens.isNull ? 0 : screens.maxX) + 10_000, y: 0)
    let window = OffScreenSnapshotWindow(contentRect: CGRect(origin: origin, size: size), styleMask: styleMask, backing: .buffered, defer: false)
    window.setFrameOrigin(origin)
    window.isReleasedWhenClosed = false
    return window
}

@MainActor
final class SnapshotWindowTests: XCTestCase {
    func testTheWindowStaysOffEveryScreenSoThePointerCannotHoverIt() {
        for styleMask: NSWindow.StyleMask in [[.borderless], [.titled]] {
            let window = snapshotWindow(size: CGSize(width: 480, height: 420), styleMask: styleMask)
            defer { window.close() }
            window.orderFront(nil)
            for screen in NSScreen.screens {
                XCTAssertFalse(window.frame.intersects(screen.frame), "\(window.frame) overlaps the screen \(screen.frame)")
            }
            XCTAssertFalse(window.frame.contains(NSEvent.mouseLocation))
        }
    }
}
