import AlondraContract
import AlondraFeatures
import Darwin
import AlondraiOSUI
import ComposableArchitecture
import SwiftUI
import UIKit
import XCTest

/// VoiceOver reads only the front card of Waiting for You: the cards peeking behind it are shapes,
/// with no buttons, reply field or text to land on.
@MainActor
final class TriageStackAccessibilityTests: XCTestCase {
    private struct Host: View {
        let store: StoreOf<TriageFeature>
        @Namespace private var zoom
        var body: some View { TriageView(store: store, zoom: zoom) }
    }

    private var automationWasEnabled: Int32?

    /// SwiftUI builds its accessibility tree only while an assistive client asks for it; automation
    /// mode is how KIF and AccessibilitySnapshot get it in a test.
    override func setUp() async throws {
        let library = try XCTUnwrap(dlopen(Self.accessibilityLibrary, RTLD_LOCAL))
        automationWasEnabled = automationEnabled(library)()
        setAutomationEnabled(library)(1)
    }

    override func tearDown() async throws {
        guard let automationWasEnabled, let library = dlopen(Self.accessibilityLibrary, RTLD_LOCAL) else { return }
        setAutomationEnabled(library)(automationWasEnabled)
    }

    private static var accessibilityLibrary: String {
        (ProcessInfo.processInfo.environment["IPHONE_SIMULATOR_ROOT"] ?? "") + "/usr/lib/libAccessibility.dylib"
    }

    private func automationEnabled(_ library: UnsafeMutableRawPointer) -> @convention(c) () -> Int32 {
        unsafeBitCast(dlsym(library, "_AXSAutomationEnabled"), to: (@convention(c) () -> Int32).self)
    }

    private func setAutomationEnabled(_ library: UnsafeMutableRawPointer) -> @convention(c) (Int32) -> Void {
        unsafeBitCast(dlsym(library, "_AXSSetAutomationEnabled"), to: (@convention(c) (Int32) -> Void).self)
    }

    func test_onlyTheFrontCardOfAStackIsAccessible() async throws {
        let queue = [Fixtures.questionID, Fixtures.informationID, Fixtures.deliveryID]
        let store = try ScreenStores.triage(queue: queue, cards: try queue.map(Fixtures.card))

        let scene = try XCTUnwrap(UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.first)
        let window = UIWindow(windowScene: scene)
        window.frame = CGRect(x: 0, y: 0, width: 393, height: 852)
        window.rootViewController = UIHostingController(rootView: Host(store: store))
        window.makeKeyAndVisible()
        defer { window.isHidden = true }
        try await Task.sleep(for: .seconds(1))

        let elements = accessibleElements(in: window)
        let identifiers = elements.compactMap { $0.value(forKey: "accessibilityIdentifier") as? String }
        let skips = identifiers.filter { $0.hasPrefix("triage.later.") }
        let sends = identifiers.filter { $0.hasPrefix("triage.send.") }
        print("[stack-proof] views=\(views(in: window)) elements=\(elements.count) skips=\(skips) sends=\(sends)")
        XCTAssertEqual(skips, ["triage.later.\(Fixtures.questionID)"], "only the front card offers Leave for Later")
        XCTAssertEqual(sends.count, 1, "only the front card has a reply field")
    }

    private func accessibleElements(in object: NSObject) -> [NSObject] {
        if object.isAccessibilityElement { return [object] }
        let children: [NSObject]
        if let elements = object.accessibilityElements as? [NSObject] {
            children = elements
        } else if object.accessibilityElementCount() > 0, object.accessibilityElementCount() != NSNotFound {
            children = (0 ..< object.accessibilityElementCount()).compactMap { object.accessibilityElement(at: $0) as? NSObject }
        } else if let view = object as? UIView {
            children = view.subviews
        } else {
            children = []
        }
        return children.flatMap(accessibleElements)
    }

    private func views(in view: UIView) -> Int {
        1 + view.subviews.map(views).reduce(0, +)
    }
}
