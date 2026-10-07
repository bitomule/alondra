import SnapshotTesting
import SwiftUI
import UIKit
import XCTest

/// A screen with what it presents on top of it, an alert or a dialog, as the system draws it: the
/// view goes into a real window, the presentation is given time to finish, and the window is drawn.
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
    for _ in 0..<80 where host.presentedViewController == nil {
        try? await Task.sleep(for: .milliseconds(50))
    }
    XCTAssertNotNil(host.presentedViewController, "nothing was presented", file: file, line: line)
    try? await Task.sleep(for: .milliseconds(400))
    let format = UIGraphicsImageRendererFormat()
    format.scale = 1
    let image = UIGraphicsImageRenderer(bounds: bounds, format: format).image { _ in
        window.drawHierarchy(in: bounds, afterScreenUpdates: true)
    }
    host.presentedViewController?.dismiss(animated: false)
    window.isHidden = true
    assertAppSnapshot(of: image, as: .image(perceptualPrecision: 0.98), named: name, file: file, testName: testName, line: line)
}
