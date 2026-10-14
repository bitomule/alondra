import AlondraContract
import AlondraFeatures
import XCTest

/// Every connection indicator comes out of one value, so none can contradict another.
final class ConnectionIndicatorsTests: XCTestCase {
    private static let states: [ConnectionState] = [
        .connecting,
        .connected(.thisMac), .connected(.localNetwork), .connected(.tailscale), .connected(.relay),
        .unreachable(.noInternet), .unreachable(.serverUnreachable), .unreachable(.macNotAnswering),
        .unreachable(.alondraNotRunning), .unreachable(.sessionExpired), .unreachable(.accessNotConfirmed),
        .unreachable(.deviceRevoked(macName: "Studio")), .unreachable(.macNotOnNetwork),
        .unreachable(.macChangedIdentity), .unreachable(.localNetworkDenied), .unreachable(.unexpected),
    ]

    func testNoStateShowsTwoThingsAtOnce() {
        for state in Self.states {
            for hasSavedData in [true, false] {
                let shown = ConnectionIndicators(state, hasSavedData: hasSavedData)
                let context = "\(state), saved data: \(hasSavedData)"
                switch shown.light {
                case .live:
                    XCTAssertEqual(shown.line, .summary, context)
                    XCTAssertEqual(shown.cover, .none, context)
                    XCTAssertTrue(shown.figuresLive, context)
                case .connecting:
                    XCTAssertTrue(shown.line == .connecting || shown.cover == .connecting, context)
                    XCTAssertFalse(shown.figuresLive, context)
                case .down:
                    XCTAssertTrue(shown.line == .savedData(state.problem!) || shown.cover == .lost(state.problem!), context)
                    XCTAssertFalse(shown.figuresLive, context)
                }
                XCTAssertFalse(shown.line != .summary && shown.cover != .none,
                               "\(context): a header over saved data and a cover in its place")
                if hasSavedData { XCTAssertEqual(shown.cover, .none, "\(context): saved data stays visible") }
            }
        }
    }

    /// 14 Oct, 08:58: a failed reload of Waiting for You while the Mac answered. The header reads
    /// only the connection state, which says the Mac is answering.
    func testSavedDataIsNeverSaidWhileTheMacAnswers() {
        for route in [ConnectionRoute.thisMac, .localNetwork, .tailscale, .relay] {
            let shown = ConnectionIndicators(.connected(route), hasSavedData: true)
            XCTAssertEqual(shown.light, .live)
            XCTAssertEqual(shown.line, .summary)
        }
    }

    func testTheWindowSubtitleSaysTheSameAsTheHeader() {
        XCTAssertEqual(ConnectionIndicators(.connected(.thisMac), hasSavedData: true).subtitle(summary: "2 nodes working"),
                       "2 nodes working")
        XCTAssertEqual(ConnectionIndicators(.connecting, hasSavedData: true).subtitle(summary: "2 nodes working"),
                       "Connecting…")
        XCTAssertEqual(ConnectionIndicators(.unreachable(.alondraNotRunning), hasSavedData: true).subtitle(summary: "2 nodes working"),
                       "saved data · Alondra isn’t running")
    }

    func testTheMacNotAnsweringWithSavedDataSaysWhyOverIt() {
        let shown = ConnectionIndicators(.unreachable(.macNotAnswering), hasSavedData: true)
        XCTAssertEqual(shown, ConnectionIndicators(.unreachable(.macNotAnswering), hasSavedData: true))
        XCTAssertEqual(shown.light, .down)
        XCTAssertEqual(shown.line, .savedData(.macNotAnswering))
        XCTAssertEqual(shown.cover, .none)
    }
}
