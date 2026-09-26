import AlondraAPIClient
import AlondraContracts
import Foundation

public struct AlondraClient: Sendable {
    public let base: URL
    public let device: String
    public let signer: (any RequestSigner)?
    private let transport: any HTTPTransport
    private static let defaultTransport: any HTTPTransport = URLSessionTransport()

    /// A local launch override lets simulator and Mac UI tests use an isolated daemon.
    public static let defaultURL = ProcessInfo.processInfo.environment["ALONDRA_SERVER_URL"]
        .flatMap { serverURL($0) } ?? URL(string: "http://127.0.0.1:47800")!
    public static let local = AlondraClient(base: defaultURL)

    public init(base: URL, device: String = "mac", signer: (any RequestSigner)? = nil, transport: (any HTTPTransport)? = nil) {
        self.base = base
        self.device = device
        self.signer = signer
        self.transport = transport ?? Self.defaultTransport
    }

    public init?(string: String, device: String = "mac", signer: (any RequestSigner)? = nil, transport: (any HTTPTransport)? = nil) {
        guard let url = Self.serverURL(string) else { return nil }
        self.init(base: url, device: device, signer: signer, transport: transport)
    }

    public static func serverURL(_ string: String) -> URL? {
        let trimmed = string.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let url = URL(string: trimmed), let scheme = url.scheme, ["http", "https"].contains(scheme), url.host() != nil else {
            return nil
        }
        return url
    }

    public var isLoopback: Bool {
        Self.isLoopback(base)
    }

    public static func isLoopback(_ url: URL) -> Bool {
        guard let host = url.host()?.lowercased() else { return false }
        return host == "localhost" || host == "::1" || host == "[::1]" || host.hasPrefix("127.")
    }

    public var identity: String {
        "\(base.absoluteString)#\(signer?.deviceID ?? "")"
    }

    private static let decoder: JSONDecoder = {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return decoder
    }()

    private var api: AlondraAPIClient {
        AlondraAPIClient(base: base, device: device, signer: signer, transport: transport)
    }

    private func request(_ endpoint: Endpoint<Data>) async throws -> Data {
        do { return try await api.send(endpoint) }
        catch let error as APIError { throw AlondraError.from(error) }
    }

    private func decoded<T: Decodable>(_ endpoint: Endpoint<Data>, as type: T.Type = T.self) async throws -> T {
        try Self.decoder.decode(T.self, from: try await request(endpoint))
    }

    public func health() async throws {
        _ = try await request(AlondraEndpoints.health())
    }

    public func overview() async throws -> Overview { try await decoded(AlondraEndpoints.overview()) }

    public func waitForChange(after tick: UInt64) async throws {
        _ = try await request(AlondraEndpoints.waitForChange(after: tick))
    }

    public func answer(_ question: UInt64, text: String, option: String?, attachments: [String] = []) async throws {
        _ = try await request(AlondraEndpoints.answer(question, text: text, option: option, attachments: attachments, device: device))
    }

    public func moreContext(_ question: UInt64, note: String, attachments: [String] = []) async throws {
        _ = try await request(AlondraEndpoints.moreContext(question, note: note, attachments: attachments))
    }

    public func mergePullRequest(_ id: UInt64, method: String) async throws {
        _ = try await request(AlondraEndpoints.mergePullRequest(id, method: method))
    }

    public func requestChanges(onPullRequest id: UInt64, text: String, attachments: [String] = []) async throws {
        _ = try await request(AlondraEndpoints.requestChanges(id, text: text, attachments: attachments))
    }

    public func ask(onPullRequest id: UInt64, text: String, attachments: [String] = []) async throws {
        _ = try await request(AlondraEndpoints.ask(id, text: text, attachments: attachments))
    }

    public func resolveConflicts(onPullRequest id: UInt64) async throws {
        _ = try await request(AlondraEndpoints.resolveConflicts(id))
    }

    public func pullRequestDiff(_ id: UInt64) async throws -> String {
        String(decoding: try await request(AlondraEndpoints.pullRequestDiff(id)), as: UTF8.self)
    }

    public func dismissPullRequest(_ id: UInt64) async throws { _ = try await request(AlondraEndpoints.dismissPullRequest(id)) }
    public func cancelTask(_ id: String) async throws { _ = try await request(AlondraEndpoints.cancelTask(id)) }
    public func reopenTask(_ id: String) async throws { _ = try await request(AlondraEndpoints.reopenTask(id)) }
    public func withdrawRequest(_ id: String) async throws { _ = try await request(AlondraEndpoints.withdrawRequest(id)) }
    public func archiveCompletedTasks(ids: [String]? = nil) async throws {
        _ = try await request(AlondraEndpoints.archiveCompletedTasks(ids: ids))
    }

    public func nodeThreads(_ session: String) async throws -> NodeThreads {
        let answer: Data
        do { answer = try await request(AlondraEndpoints.nodeActivity(session)) }
        catch AlondraError.unexpected(404) { answer = try await request(AlondraEndpoints.nodeDetail(session)) }
        return try Self.decoder.decode(NodeThreads.self, from: answer)
    }

    public static func nodeThreads(_ session: String, get: (String, [(String, String)]) async throws -> Data) async throws -> NodeThreads {
        let answer: Data
        do { answer = try await get("v1/nodes/\(session)/activity", []) }
        catch AlondraError.unexpected(404) { answer = try await get("v1/nodes/\(session)", [("limit", "1")]) }
        return try decoder.decode(NodeThreads.self, from: answer)
    }

    public func task(_ id: String) async throws -> TaskInFull { try await decoded(AlondraEndpoints.task(id)) }
    public func activity(client: String) async throws { _ = try await request(AlondraEndpoints.activity(client: client)) }
    public func newRequest(_ body: NewRequestBody) async throws { _ = try await request(AlondraEndpoints.newRequest(body)) }
    public func schedules() async throws -> [ScheduledTask] {
        let list: ScheduledTaskList = try await decoded(AlondraEndpoints.schedules())
        return list.schedules
    }
    public func saveSchedule(_ draft: ScheduledTaskDraft, id: String? = nil) async throws -> ScheduledTask {
        try await decoded(AlondraEndpoints.saveSchedule(JSONEncoder().encode(draft), id: id))
    }
    public func deleteSchedule(_ id: String) async throws { _ = try await request(AlondraEndpoints.deleteSchedule(id)) }
    public func runSchedule(_ id: String) async throws { _ = try await request(AlondraEndpoints.runSchedule(id)) }
    public func options(project: String?) async throws -> OptionsCatalog { try await decoded(AlondraEndpoints.options(project: project)) }
    public func issues(refresh: Bool) async throws -> IssueList { try await decoded(AlondraEndpoints.issues(refresh: refresh)) }
    public func projects() async throws -> ProjectList { try await decoded(AlondraEndpoints.projects()) }
    public func search(_ query: String) async throws -> SearchResult { try await decoded(AlondraEndpoints.search(query)) }
    public func fileText(_ path: String) async throws -> String {
        String(decoding: try await request(AlondraEndpoints.fileText(path)), as: UTF8.self)
    }
    public func markRead(_ report: UInt64) async throws { _ = try await request(AlondraEndpoints.markRead(report)) }
    public func review(_ report: UInt64, _ verdict: ReviewVerdict) async throws {
        _ = try await request(AlondraEndpoints.review(report, verdict: verdict.rawValue))
    }
    public func closeNode(_ key: String, force: Bool = false) async throws {
        do { _ = try await request(AlondraEndpoints.closeNode(key, force: force)) }
        catch AlondraError.server(let message) { throw CloseRefused(message: message) }
    }
    public func send(to session: String, text: String, attachments: [String] = []) async throws {
        _ = try await request(AlondraEndpoints.send(to: session, text: text, attachments: attachments))
    }
    public func upload(_ data: Data, name: String, progress: @escaping @Sendable (Double) -> Void = { _ in }) async throws -> String {
        do { return try await api.upload(data, name: name, progress: progress) }
        catch let error as APIError { throw AlondraError.from(error) }
    }
    public func registerPush(_ registration: PushRegistration) async throws {
        _ = try await request(AlondraEndpoints.registerPush(registration))
    }
    public static func pair(at base: URL, code: String, deviceName: String, publicKey: Data) async throws -> String {
        struct Paired: Decodable { let deviceId: String }
        let client = AlondraClient(base: base)
        let data = try await client.request(AlondraEndpoints.pair(code: code, deviceName: deviceName, publicKey: publicKey))
        return try decoder.decode(Paired.self, from: data).deviceId
    }
    public func createPairing() async throws -> PairingInvite { try await decoded(AlondraEndpoints.createPairing()) }
    public func pairedDevices() async throws -> [PairedDevice] {
        struct List: Decodable { let devices: [PairedDevice] }
        let list: List = try await decoded(AlondraEndpoints.pairedDevices())
        return list.devices
    }
    public func revokeDevice(_ id: String) async throws { _ = try await request(AlondraEndpoints.revokeDevice(id)) }

}

/// Why `POST /v1/nodes/{key}/close` refused, in the user's language and without the server's
/// status code or raw prose. `detail`, when present, names the worktree or files at stake and
/// is not itself translated.
func closeReasonMessage(kind: String, detail: String?) -> String {
    switch kind {
    case "busy":
        return String(localized: "it is working; wait until it stops", table: "AlondraShared", bundle: #bundle)
    case "permission":
        return String(localized: "it has a permission pending on a tool call that is running; answer it and close again", table: "AlondraShared", bundle: #bundle)
    case "orchestrator":
        return String(localized: "it is the orchestrator, which Alondra keeps alive", table: "AlondraShared", bundle: #bundle)
    case "worktree":
        return String(localized: "its worktree has work that is not pushed (\(detail ?? "")); commit and push it, or discard it, and close again", table: "AlondraShared", bundle: #bundle)
    default:
        return detail ?? kind
    }
}

public typealias PairingInvite = AlondraContracts.PairingInvite
public typealias PairedDevice = AlondraContracts.PairedDevice
public typealias PushRegistration = AlondraContracts.PushRegistration

extension PairingInvite {
    public func payload(fallbackURL: String) -> String {
        let object = ["url": url ?? fallbackURL, "code": code]
        let data = (try? JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])) ?? Data()
        return String(decoding: data, as: UTF8.self)
    }
}

public enum AlondraError: LocalizedError, Equatable {
    /// alondrad's own JSON error, meant for the user.
    case server(String)
    case unauthorized
    /// A gateway answered instead of alondrad (the tunnel is up, alondrad is not): worth retrying.
    case unreachable
    /// Any other failure whose body is not alondrad's JSON; the body only goes to the log.
    case unexpected(Int)

    public static let longestMessage = 300

    /// Keep the app's existing localized error contract while the transport reports typed status.
    public static func from(_ error: APIError) -> AlondraError {
        switch error {
        case .unauthorized: return .unauthorized
        case .unavailable: return .unreachable
        case .unexpected(let status): return .unexpected(status)
        case .forbidden(let kind, let detail, let reasons):
            return message(status: 403, kind: kind, detail: detail, reasons: reasons)
        case .conflict(let kind, let detail, let reasons):
            return message(status: 409, kind: kind, detail: detail, reasons: reasons)
        case .server(let status, let kind, let detail, let reasons):
            return message(status: status, kind: kind, detail: detail, reasons: reasons)
        }
    }

    private static func message(status: Int, kind: String?, detail: String?, reasons: [APIError.RefusalReason]) -> AlondraError {
        if !reasons.isEmpty {
            let lines = reasons.map { closeReasonMessage(kind: $0.kind, detail: $0.detail) }
            return .server(lines.count == 1 ? lines[0] : lines.map { "- \($0)" }.joined(separator: "\n"))
        }
        if status == 404 {
            return .server(String(localized: "Alondra no longer has it", table: "AlondraShared", bundle: #bundle))
        }
        if let detail, !detail.isEmpty { return .server(refusalMessage(kind: kind, detail: detail)) }
        return .unexpected(status)
    }

    /// Only alondrad's JSON errors reach the user as text; a proxy's HTML page never does.
    /// A 5xx carrying alondrad's own JSON is alondrad answering, not a gateway standing in for it:
    /// sending the same thing again would get the same answer.
    public static func from(status: Int, body: Data) -> AlondraError {
        if status == 401 { return .unauthorized }
        let json = try? JSONSerialization.jsonObject(with: body) as? [String: Any]
        if let reasons = json?["reasons"] as? [[String: Any]], !reasons.isEmpty {
            let lines = reasons.map { closeReasonMessage(kind: $0["kind"] as? String ?? "", detail: $0["detail"] as? String) }
            return .server(lines.count == 1 ? lines[0] : lines.map { "- \($0)" }.joined(separator: "\n"))
        }
        if status == 404, json?["error"] != nil {
            return .server(String(localized: "Alondra no longer has it", table: "AlondraShared", bundle: #bundle))
        }
        if let message = (json?["error"] as? String)?.trimmingCharacters(in: .whitespacesAndNewlines), !message.isEmpty {
            let detail = message.count > longestMessage ? String(message.prefix(longestMessage)) + "…" : message
            return .server(refusalMessage(kind: json?["kind"] as? String, detail: detail))
        }
        if [502, 503, 504].contains(status) { return .unreachable }
        return .unexpected(status)
    }

    /// alondrad's `kind` in the user's language; its English `error` only when the kind is unknown.
    static func refusalMessage(kind: String?, detail: String) -> String {
        switch kind {
        case "pr_not_open": String(localized: "This PR is no longer open", table: "AlondraShared", bundle: #bundle)
        case "merge_refused": String(localized: "GitHub didn’t merge it: \(detail)", table: "AlondraShared", bundle: #bundle)
        case "question_closed": String(localized: "This question was already answered", table: "AlondraShared", bundle: #bundle)
        case "task_refused": String(localized: "The task’s state does not allow that now", table: "AlondraShared", bundle: #bundle)
        default: detail
        }
    }

    public var errorDescription: String? {
        switch self {
        case .server(let message): message
        case .unauthorized: String(localized: "This device is not paired", table: "AlondraShared", bundle: #bundle)
        case .unreachable: String(localized: "Alondra isn’t responding right now", table: "AlondraShared", bundle: #bundle)
        case .unexpected(let status): String(localized: "The server responded \(status)", table: "AlondraShared", bundle: #bundle)
        }
    }
}

public struct CloseRefused: LocalizedError, Equatable {
    public let message: String

    public init(message: String) {
        self.message = message
    }

    public var errorDescription: String? { message }
}

extension Error {
    public var isCancellation: Bool {
        if self is CancellationError { return true }
        return (self as? URLError)?.code == .cancelled
    }

    /// A failure that says nothing about the request itself (alondrad down, restarting, the
    /// network gone): sending the same thing again later is the right answer.
    public var isTransient: Bool {
        if let error = self as? AlondraError { return error == .unreachable }
        guard let code = (self as? URLError)?.code else { return false }
        return [.timedOut, .cannotConnectToHost, .cannotFindHost, .networkConnectionLost, .notConnectedToInternet, .dnsLookupFailed].contains(code)
    }

    public var friendlyDescription: String {
        guard let urlError = self as? URLError else { return localizedDescription }
        switch urlError.code {
        case .cannotConnectToHost, .cannotFindHost: return String(localized: "Can’t find the Alondra server", table: "AlondraShared", bundle: #bundle)
        case .notConnectedToInternet, .networkConnectionLost: return String(localized: "No connection", table: "AlondraShared", bundle: #bundle)
        case .timedOut: return String(localized: "The server isn’t responding", table: "AlondraShared", bundle: #bundle)
        default: return urlError.localizedDescription
        }
    }
}
