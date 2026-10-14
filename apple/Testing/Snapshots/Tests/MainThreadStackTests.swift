import AlondraContract
import AlondraFeatures
import AlondraFeed
import AlondraiOSUI
import ComposableArchitecture
import Darwin
import SwiftUI
import UIKit
import XCTest

/// Drawing a card fits the iPhone's main thread stack. The device gives the main thread 1 MB (the
/// build 47 crash report shows a 1008 KB stack over a 16 KB guard), the simulator 8 MB, so a screen
/// whose drawing overflows the phone still passes every snapshot test. Build 47 crashed on launch
/// because TriageCard's ViewThatFits held nine copies of a card-sized layout on the stack at once
/// (1.1 MB). Each screen here is measured against the same window drawing nothing, and what the
/// screen itself adds is held to a quarter of what the phone has.
@MainActor
final class MainThreadStackTests: XCTestCase {
    private static let budget = 256 * 1024

    private struct Triage: View {
        let store: StoreOf<TriageFeature>
        @Namespace private var zoom
        var body: some View { TriageView(store: store, zoom: zoom) }
    }

    override func setUp() async throws { ScreenStores.pinTime() }

    func test_drawingAScreenFitsTheIPhoneMainThreadStack() throws {
        let nothing = try peakStack { Color.clear }
        print("[stack] nothing: \(nothing / 1024) KB")
        for (name, document) in try cards() {
            for stamped in [false, true] {
                let store = try triage(document, stamped: stamped)
                assertFits("Waiting for You, \(name)\(stamped ? " under the stamp" : "")", over: nothing) { Triage(store: store) }
            }
            let detail = ScreenStores.cardDetail(document)
            assertFits("Card detail, \(name)", over: nothing) { CardDetailView(store: detail) }
        }
        let task = try taskDetail(SampleViews.document("task-detail"))
        assertFits("Task detail", over: nothing) { TaskDetailView(store: task) }
    }

    private func cards() throws -> [(String, ViewDocument)] {
        [
            ("question", try Fixtures.card(Fixtures.questionID)),
            ("crowded decision", try Fixtures.crowdedDecision()),
            ("group", try Fixtures.document("card-group")),
            ("authorize", try Fixtures.authorizeCard()),
            ("problem", try Fixtures.problemCard()),
            ("plan", try Fixtures.planCard()),
            ("orchestrator decide", try Fixtures.orchestratorDecide()),
            ("information", try Fixtures.card(Fixtures.informationID)),
            ("delivery", try Fixtures.card(Fixtures.deliveryID)),
            ("pull request", try Fixtures.card(Fixtures.pullRequestID)),
        ]
    }

    private func assertFits<Screen: View>(_ name: String, over nothing: Int, @ViewBuilder _ screen: () -> Screen,
                                          line: UInt = #line) {
        do {
            let added = try peakStack(screen) - nothing
            print("[stack] \(name): +\(added / 1024) KB")
            XCTAssertLessThan(added, Self.budget, "\(name) adds \(added / 1024) KB of stack", line: line)
        } catch {
            XCTFail("\(name): \(error)", line: line)
        }
    }

    private func triage(_ document: ViewDocument, stamped: Bool) throws -> StoreOf<TriageFeature> {
        let id = document.key.replacingOccurrences(of: "card:", with: "")
        return try ScreenStores.triage(queue: [id], cards: [document], home: try Fixtures.home(queue: [id])) { state in
            guard stamped, let card = state.cards.first, let action = card.card?.actions.first else { return }
            state.cards[id: card.id]?.actions.action = action
            state.cards[id: card.id]?.actions.stamp = "Sent"
        }
    }

    private func taskDetail(_ document: ViewDocument) -> StoreOf<TaskDetailFeature> {
        let key = ViewKey(rawValue: document.key)
        var state = TaskDetailFeature.State(key: key)
        state.screen = .showing(document, key: key)
        return Store(initialState: state) { TaskDetailFeature() } withDependencies: {
            $0.viewsClient.observe = ViewsClient.serving(document)
        }
    }

    private func peakStack<Screen: View>(@ViewBuilder _ screen: () -> Screen) throws -> Int {
        let scene = try XCTUnwrap(UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.first)
        let window = UIWindow(windowScene: scene)
        window.frame = CGRect(x: 0, y: 0, width: 393, height: 852)
        defer { window.isHidden = true }
        let host = UIHostingController(rootView: screen())
        return StackProbe.peak {
            window.rootViewController = host
            window.makeKeyAndVisible()
            window.layoutIfNeeded()
        }
    }
}

/// How deep the current thread's stack goes while `work` runs: the stack under the caller is
/// filled with a pattern first, and whatever `work` overwrote of it is what it used.
private enum StackProbe {
    private static let pattern: UInt64 = 0xA5A5_A5A5_A5A5_A5A5
    private static let callerSlack: UInt = 32 * 1024
    private static let painted: UInt = 4 * 1024 * 1024

    @inline(never)
    static func peak(_ work: () -> Void) -> Int {
        let thread = pthread_self()
        let top = UInt(bitPattern: pthread_get_stackaddr_np(thread))
        let lowest = top - UInt(pthread_get_stacksize_np(thread)) + 64 * 1024
        var marker = 0
        let caller = withUnsafeMutablePointer(to: &marker) { UInt(bitPattern: $0) }
        let ceiling = caller - callerSlack
        let floor = max(lowest, ceiling - painted)
        paint(from: floor, to: ceiling)
        work()
        var reached = floor
        while reached < ceiling, UnsafeRawPointer(bitPattern: reached)!.load(as: UInt64.self) == pattern {
            reached += 8
        }
        return Int(caller - reached)
    }

    @inline(never)
    private static func paint(from floor: UInt, to ceiling: UInt) {
        memset(UnsafeMutableRawPointer(bitPattern: floor)!, 0xA5, Int(ceiling - floor))
    }
}
