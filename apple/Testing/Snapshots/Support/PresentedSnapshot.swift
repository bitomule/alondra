import SnapshotTesting
import SwiftUI
import UIKit
import XCTest

/// A screen with what it presents on top of it, an alert or a dialog, as the system draws it: the
/// view goes into a real window, the capture waits until the presentation has finished (its
/// transition is over), and the window is drawn once it stops changing.
@MainActor
func assertPresented<Content: View>(
    language: SnapshotLanguage = .english,
    style: UIUserInterfaceStyle = .light,
    named name: String,
    file: StaticString = #filePath,
    testName: String = #function,
    line: UInt = #line,
    @ViewBuilder _ content: () -> Content
) async {
    UIView.setAnimationsEnabled(false)
    defer { UIView.setAnimationsEnabled(true) }
    let bounds = CGRect(x: 0, y: 0, width: 393, height: 852)
    let scene = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.first
    let window = scene.map { UIWindow(windowScene: $0) } ?? UIWindow(frame: bounds)
    window.frame = bounds
    window.overrideUserInterfaceStyle = style
    let host = UIHostingController(rootView: content().environment(\.locale, language.locale))
    window.rootViewController = host
    window.makeKeyAndVisible()
    let presented = await settled { [host] in
        guard let shown = host.presentedViewController else { return false }
        return !shown.isBeingPresented && shown.transitionCoordinator == nil && shown.viewIfLoaded?.window != nil
    }
    guard presented else {
        window.isHidden = true
        return XCTFail("nothing was presented, or its presentation never finished", file: file, line: line)
    }
    let image = await steadyImage(of: window, bounds: bounds)
    host.presentedViewController?.dismiss(animated: false)
    window.isHidden = true
    assertAppSnapshot(of: image, as: .image(perceptualPrecision: 0.98), named: name, file: file, testName: testName, line: line)
}

/// The window once it stops changing. The alert's glass and shadow keep stepping in for about half
/// a second after its transition is over, with pauses of up to a quarter of a second between steps
/// that grow on a loaded machine, so the window is drawn until it has not changed for `quiet`.
@MainActor
private func steadyImage(
    of window: UIWindow,
    bounds: CGRect,
    quiet: Duration = .milliseconds(1500),
    limit: Duration = .seconds(10)
) async -> UIImage {
    let format = UIGraphicsImageRendererFormat()
    format.scale = 1
    let renderer = UIGraphicsImageRenderer(bounds: bounds, format: format)
    func draw() -> UIImage {
        renderer.image { _ in window.drawHierarchy(in: bounds, afterScreenUpdates: true) }
    }
    let clock = ContinuousClock()
    let deadline = clock.now.advanced(by: limit)
    var image = draw()
    var data = image.pngData()
    var changed = clock.now
    while clock.now - changed < quiet, clock.now < deadline {
        try? await Task.sleep(for: .milliseconds(100))
        let next = draw()
        let nextData = next.pngData()
        if nextData != data {
            changed = clock.now
        }
        image = next
        data = nextData
    }
    return image
}

/// Lets the main run loop turn, and draw, until `condition` holds; the time limit only ends a wait
/// that will never succeed.
@MainActor
private func settled(within limit: Duration = .seconds(5), _ condition: @MainActor () -> Bool) async -> Bool {
    let deadline = ContinuousClock.now.advanced(by: limit)
    while !condition() {
        guard ContinuousClock.now < deadline else { return false }
        try? await Task.sleep(for: .milliseconds(5))
    }
    return true
}
