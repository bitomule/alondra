import AlondraContract
import AlondraTransport
import ComposableArchitecture
import Foundation

/// Settings › Send a crash report: «What happened?» first, then the Mac builds the report and
/// the screen shows it, every file whole, exactly as it would leave the Mac; then one decision,
/// send it or keep it on the Mac. Nothing is sent until the user says yes.
@Reducer public struct CrashReportFeature: Sendable {
    public enum Failure: Equatable, Sendable {
        /// The Mac did not answer; what to run on it is said in the screen.
        case notAnswering(onThisMac: Bool)
        case problem(ConnectionProblem)
        /// The Mac answered with why it could not: its own words.
        case refused(String)
        case unreadable
    }

    /// Why the daemon turned a report down, as its report routes name it.
    public enum ReportRefusal: String, Sendable {
        case invalid, noCli = "no_cli", cliFailed = "cli_failed", timedOut = "timed_out", gone
    }

    public enum Stage: Equatable, Sendable {
        case describing
        case building
        case ready(CrashReport)
        case sending(CrashReport)
        case sent(CrashReportSent)
        /// The user kept it: it stays on the Mac under this id.
        case kept(String)
        /// `report` is the report that was being sent, when it failed while sending.
        case failed(Failure, report: String?)
    }

    public struct Viewing: Equatable, Sendable, Identifiable {
        public var name: String
        public var text: String?
        public var error: String?
        public var id: String { name }

        public init(name: String, text: String? = nil, error: String? = nil) {
            self.name = name; self.text = text; self.error = error
        }
    }

    @ObservableState public struct State: Equatable {
        /// Settings always sends «other»: the button of a failed task is the one that names a kind.
        public var failure = "other"
        public var whatHappened = ""
        public var stage: Stage = .describing
        public var viewing: Viewing?

        public init(whatHappened: String = "", stage: Stage = .describing, viewing: Viewing? = nil) {
            self.whatHappened = whatHappened; self.stage = stage; self.viewing = viewing
        }

        public var report: CrashReport? {
            switch stage {
            case .ready(let report), .sending(let report): report
            default: nil
            }
        }

        public var writtenDescription: String? {
            let text = whatHappened.trimmingCharacters(in: .whitespacesAndNewlines)
            return text.isEmpty ? nil : text
        }
    }

    public enum Action {
        case describe(String)
        case build
        case built(CrashReport)
        case open(String)
        case fileLoaded(String, text: String?, error: String?)
        case closeFile
        case send
        case sent(CrashReportSent)
        case keep
        case failed(Failure, report: String?)
        case startOver
    }

    @Dependency(\.crashReports) var client
    private enum ID { case work, file }

    public init() {}

    public var body: some ReducerOf<Self> {
        Reduce { state, action in
            switch action {
            case .describe(let text):
                guard state.stage == .describing else { return .none }
                state.whatHappened = text
                return .none
            case .build:
                guard state.stage == .describing || state.isFailedBeforeBuilding else { return .none }
                state.stage = .building
                let (description, failure) = (state.writtenDescription, state.failure)
                return .run { [client] send in
                    do {
                        let section = await client.appSection()
                        let report = try await client.build(description, failure, section)
                        await send(.built(report))
                    } catch {
                        await send(.failed(Self.failure(error, client), report: nil))
                    }
                }.cancellable(id: ID.work, cancelInFlight: true)
            case .built(let report):
                guard state.stage == .building else { return .none }
                state.stage = .ready(report)
                return .none
            case .open(let name):
                guard let report = state.report, report.files.contains(where: { $0.name == name && $0.openable }) else { return .none }
                state.viewing = Viewing(name: name)
                return .run { [client] send in
                    do {
                        await send(.fileLoaded(name, text: try await client.file(report.id, name), error: nil))
                    } catch {
                        await send(.fileLoaded(name, text: nil, error: Self.message(Self.failure(error, client))))
                    }
                }.cancellable(id: ID.file, cancelInFlight: true)
            case let .fileLoaded(name, text, error):
                guard state.viewing?.name == name else { return .none }
                state.viewing?.text = text
                state.viewing?.error = error
                return .none
            case .closeFile:
                state.viewing = nil
                return .cancel(id: ID.file)
            case .send:
                guard case .ready(let report) = state.stage, report.sendable else { return .none }
                state.stage = .sending(report)
                let failure = state.failure
                return .run { [client] send in
                    do {
                        let outcome = try await client.send(report.id, failure)
                        if outcome.state == .refused {
                            await send(.failed(.refused(outcome.message), report: report.id))
                        } else {
                            await send(.sent(outcome))
                        }
                    } catch {
                        await send(.failed(Self.failure(error, client), report: report.id))
                    }
                }.cancellable(id: ID.work, cancelInFlight: true)
            case .sent(let outcome):
                guard case .sending = state.stage else { return .none }
                state.stage = .sent(outcome)
                return .none
            case .keep:
                guard case .ready(let report) = state.stage else { return .none }
                state.stage = .kept(report.id)
                return .none
            case let .failed(failure, report):
                state.stage = .failed(failure, report: report)
                return .none
            case .startOver:
                state = State()
                return .merge(.cancel(id: ID.work), .cancel(id: ID.file))
            }
        }
    }

    static func failure(_ error: any Error, _ client: CrashReportClient) -> Failure {
        if error is CrashReportUnreadable { return .unreadable }
        if let why = refusal(error) { return .refused(why) }
        switch client.problem(error) {
        case .macNotAnswering, .alondraNotRunning, .macNotOnNetwork, .macChangedIdentity, .localNetworkDenied, .unexpected:
            return .notAnswering(onThisMac: client.onThisMac)
        case let problem:
            return .problem(problem)
        }
    }

    /// The Mac's words when it answered and turned the report down: a refusal the daemon names, or
    /// any 4xx answer from an older daemon that names none. A 5xx is the Mac not answering yet.
    static func refusal(_ error: any Error) -> String? {
        let status: Int, kind: String?, detail: String?
        switch error as? APIError {
        case let .server(code, named, words, _): (status, kind, detail) = (code, named, words)
        case let .conflict(named, words, _): (status, kind, detail) = (409, named, words)
        case let .forbidden(named, words, _): (status, kind, detail) = (403, named, words)
        default: return nil
        }
        guard kind.flatMap(ReportRefusal.init(rawValue:)) != nil || (400..<500).contains(status) else { return nil }
        return detail ?? String(localized: "Alondra on your Mac could not do it.")
    }

    public static func message(_ failure: Failure) -> String {
        switch failure {
        case .notAnswering(true):
            String(localized: "Alondra’s service is not answering: open Terminal and run alondra report.")
        case .notAnswering(false):
            String(localized: "Your Mac is not answering: run alondra report on it.")
        case .problem(let problem): String(localized: problem.title)
        case .refused(let why): why
        case .unreadable: String(localized: "Alondra on your Mac answered with something that is not a report.")
        }
    }
}

extension CrashReportFeature.State {
    var isFailedBeforeBuilding: Bool {
        if case .failed(_, nil) = stage { true } else { false }
    }
}
