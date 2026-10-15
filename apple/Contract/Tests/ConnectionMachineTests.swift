import AlondraContract
import XCTest

final class ConnectionMachineTests: XCTestCase {
    /// 14 Oct, 08:58, iPhone on 5G: a reload of Waiting for You gave up on a slow relay while the
    /// Mac had just answered this device. The Mac answering is the newer fact.
    func testASlowRequestThatGivesUpAfterTheMacAnsweredLeavesItConnected() {
        var machine = ConnectionMachine()
        let slowReload = machine.begin()
        _ = machine.begin()
        machine.handle(.answered(.relay))
        for _ in 0 ..< ConnectionMachine.passingFailuresShown {
            machine.handle(.failed(.macNotAnswering, ticket: slowReload))
        }
        machine.handle(.failed(.unexpected, ticket: slowReload))

        XCTAssertEqual(machine.state, .connected(.relay))
    }

    /// iroh moves a live connection from the relay to the direct path without a new answer: the
    /// label follows it, but a path change never makes a Mac that is not answering look connected.
    func testTheRouteFollowsTheLiveConnectionOnlyWhileConnected() {
        var machine = ConnectionMachine()
        machine.handle(.routeChanged(.direct))
        XCTAssertEqual(machine.state, .connecting)

        machine.handle(.answered(.irohRelay))
        machine.handle(.routeChanged(.direct))
        XCTAssertEqual(machine.state, .connected(.direct))

        machine.handle(.networkLost)
        machine.handle(.routeChanged(.irohRelay))
        XCTAssertEqual(machine.state, .unreachable(.noInternet))
    }

    func testAMacThatStopsAnsweringIsUnreachableOnlyOnceItKeepsFailing() {
        var machine = ConnectionMachine()
        machine.handle(.answered(.localNetwork))
        for attempt in 1 ... ConnectionMachine.passingFailuresShown {
            let ticket = machine.begin()
            machine.handle(.failed(.macNotAnswering, ticket: ticket))
            let expected: ConnectionState = attempt < ConnectionMachine.passingFailuresShown
                ? .connected(.localNetwork) : .unreachable(.macNotAnswering)
            XCTAssertEqual(machine.state, expected, "after \(attempt) failures")
        }
    }

    func testAProblemOnlyThePersonCanFixShowsAtOnce() {
        for problem in [ConnectionProblem.sessionExpired, .deviceRevoked(macName: "Studio"), .localNetworkDenied, .noInternet] {
            var machine = ConnectionMachine()
            machine.handle(.answered(.relay))
            machine.handle(.failed(problem, ticket: machine.begin()))
            XCTAssertEqual(machine.state, .unreachable(problem))
        }
    }

    func testThisDevicesStorageIsNeverAConnectionProblem() {
        var machine = ConnectionMachine()
        machine.handle(.answered(.thisMac))
        machine.handle(.failed(.deviceStorage, ticket: machine.begin()))
        XCTAssertEqual(machine.state, .connected(.thisMac))
    }

    func testTheNetworkComingBackOrARetryConnectsAgainAndForgetsOlderFailures() {
        for fresh in [ConnectionMachine.Event.networkBack, .retrying] {
            var machine = ConnectionMachine()
            let before = machine.begin()
            machine.handle(.networkLost)
            XCTAssertEqual(machine.state, .unreachable(.noInternet))
            machine.handle(fresh)
            XCTAssertEqual(machine.state, .connecting)
            machine.handle(.failed(.sessionExpired, ticket: before))
            XCTAssertEqual(machine.state, .connecting, "\(fresh): a request from before is not news")
        }
    }

    func testTheNetworkComingBackDoesNotHideAProblemOnlyThePersonCanFix() {
        var machine = ConnectionMachine()
        machine.handle(.failed(.sessionExpired, ticket: machine.begin()))
        machine.handle(.networkBack)
        XCTAssertEqual(machine.state, .unreachable(.sessionExpired))
    }

    func testRetryingAMacThatWasNotAnsweringSaysSoAgainAfterOneFailure() {
        var machine = ConnectionMachine()
        for _ in 0 ..< ConnectionMachine.passingFailuresShown {
            machine.handle(.failed(.macNotAnswering, ticket: machine.begin()))
        }
        machine.handle(.retrying)
        XCTAssertEqual(machine.state, .connecting)
        machine.handle(.failed(.macNotAnswering, ticket: machine.begin()))
        XCTAssertEqual(machine.state, .unreachable(.macNotAnswering))
    }

    func testRetryingWhileConnectedStaysConnected() {
        var machine = ConnectionMachine()
        machine.handle(.answered(.tailscale))
        machine.handle(.retrying)
        machine.handle(.networkBack)
        XCTAssertEqual(machine.state, .connected(.tailscale))
    }

    /// Any order of outcomes: an answer always wins, and a failure from a request that began
    /// before the latest answer never moves the state.
    func testAnyHistoryKeepsTheRules() {
        var random = SeededRandom(seed: 0xA10D7A)
        let problems: [ConnectionProblem] = [.noInternet, .serverUnreachable, .macNotAnswering, .sessionExpired,
                                             .accessNotConfirmed, .localNetworkDenied, .deviceStorage, .unexpected]
        let routes: [ConnectionRoute] = [.thisMac, .localNetwork, .tailscale, .relay]
        for _ in 0 ..< 2_000 {
            var machine = ConnectionMachine()
            var open: [UInt64] = []
            var answeredAfter: Set<UInt64> = []
            for _ in 0 ..< Int.random(in: 1 ... 40, using: &random) {
                switch Int.random(in: 0 ..< 6, using: &random) {
                case 0, 1:
                    open.append(machine.begin())
                case 2:
                    let route = routes.randomElement(using: &random)!
                    machine.handle(.answered(route))
                    answeredAfter.formUnion(open)
                    XCTAssertEqual(machine.state, .connected(route))
                case 3 where !open.isEmpty:
                    let ticket = open.remove(at: Int.random(in: 0 ..< open.count, using: &random))
                    let before = machine.state
                    machine.handle(.failed(problems.randomElement(using: &random)!, ticket: ticket))
                    if answeredAfter.contains(ticket) { XCTAssertEqual(machine.state, before) }
                case 4:
                    machine.handle(.networkLost)
                    answeredAfter.formUnion(open)
                    XCTAssertEqual(machine.state, .unreachable(.noInternet))
                default:
                    let before = machine.state
                    machine.handle(Bool.random(using: &random) ? .networkBack : .retrying)
                    if before.problem?.needsAction != true { answeredAfter.formUnion(open) }
                    if before.isConnected { XCTAssertEqual(machine.state, before) }
                }
            }
        }
    }
}

private struct SeededRandom: RandomNumberGenerator {
    var state: UInt64
    init(seed: UInt64) { state = seed }
    mutating func next() -> UInt64 {
        state &+= 0x9E37_79B9_7F4A_7C15
        var z = state
        z = (z ^ (z >> 30)) &* 0xBF58_476D_1CE4_E5B9
        z = (z ^ (z >> 27)) &* 0x94D0_49BB_1331_11EB
        return z ^ (z >> 31)
    }
}
