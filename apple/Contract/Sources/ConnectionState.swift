/// Whether the paired Mac is answering this device right now: the one value every connection
/// indicator in the apps derives from. Whether a screen has saved data to show is the screen's
/// business; this only says whether that data is live.
public enum ConnectionState: Equatable, Sendable {
    /// Nothing has answered yet since the app started, the network came back or the person retried.
    case connecting
    /// The Mac answered, and nothing that began after that answer has failed for good.
    case connected(ConnectionRoute)
    case unreachable(ConnectionProblem)

    public var problem: ConnectionProblem? {
        if case .unreachable(let problem) = self { problem } else { nil }
    }

    public var isConnected: Bool {
        if case .connected = self { true } else { false }
    }
}

/// The rules that move `ConnectionState`, fed with every request's outcome in the order they
/// happen. Each request takes a ticket when it starts; a failure only counts when its request
/// began after the latest fresh start (an answer, the network coming back, a retry), so a slow
/// request that gives up after the Mac already answered something newer never says it is gone.
public struct ConnectionMachine: Equatable, Sendable {
    /// Failures in a row of the kind that clears by itself before the Mac counts as unreachable.
    public static let passingFailuresShown = 3

    public enum Event: Equatable, Sendable {
        case answered(ConnectionRoute)
        /// The live connection moved to another path; it says nothing about whether the Mac answers.
        case routeChanged(ConnectionRoute)
        case failed(ConnectionProblem, ticket: UInt64)
        case networkLost
        case networkBack
        case retrying
    }

    public private(set) var state: ConnectionState = .connecting
    private var issued: UInt64 = 0
    private var freshFrom: UInt64 = 0
    private var passingFailures = 0

    public init() {}

    /// The ticket for a request starting now.
    public mutating func begin() -> UInt64 {
        issued += 1
        return issued
    }

    public mutating func handle(_ event: Event) {
        switch event {
        case .answered(let route):
            startFresh()
            state = .connected(route)
        case .routeChanged(let route):
            guard state.isConnected else { return }
            state = .connected(route)
        case .failed(let problem, let ticket):
            guard ticket > freshFrom, problem != .deviceStorage else { return }
            guard !Self.isPassing(problem) else {
                passingFailures += 1
                if passingFailures >= Self.passingFailuresShown { state = .unreachable(problem) }
                return
            }
            state = .unreachable(problem)
        case .networkLost:
            startFresh()
            state = .unreachable(.noInternet)
        case .networkBack:
            if state.problem?.needsAction == true { return }
            tryAgain()
        case .retrying:
            tryAgain()
        }
    }

    /// Already unreachable, one more failure says so again: the person sees it trying, not
    /// «Connecting…» for three slow attempts at a Mac that was already not answering.
    private mutating func tryAgain() {
        let wasUnreachable = state.problem != nil
        startFresh()
        guard !state.isConnected else { return }
        state = .connecting
        if wasUnreachable { passingFailures = Self.passingFailuresShown - 1 }
    }

    private mutating func startFresh() {
        freshFrom = issued
        passingFailures = 0
    }

    /// Problems that usually clear on their own: the Mac or the server being slow or busy.
    public static func isPassing(_ problem: ConnectionProblem) -> Bool {
        switch problem {
        case .macNotAnswering, .serverUnreachable, .accessNotConfirmed, .unexpected: true
        default: false
        }
    }
}
