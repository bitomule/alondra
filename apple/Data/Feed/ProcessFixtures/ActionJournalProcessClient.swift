@testable import AlondraFeed
import AlondraContract
import AlondraTransport
import CryptoKit
import Foundation
import Darwin

private enum ProcessClientError: Error { case invalidCommand, invalidConfiguration, endOfInput, measurementUnavailable }

private final class ClientInput: Sendable {
    private let queue = DispatchQueue(label: "alondra.fixture-client-input")
    func next() async throws -> ProcessClientCommand {
        let bytes: Data = try await withCheckedThrowingContinuation { continuation in
            queue.async {
                do {
                    var line = Data()
                    while let byte = try FileHandle.standardInput.read(upToCount: 1), !byte.isEmpty {
                        if byte[0] == 10 { continuation.resume(returning: line); return }
                        line.append(byte)
                        guard line.count <= 1024 * 1024 else { throw ProcessClientError.invalidCommand }
                    }
                    throw ProcessClientError.endOfInput
                } catch { continuation.resume(throwing: error) }
            }
        }
        return try JSONDecoder().decode(ProcessClientCommand.self, from: bytes)
    }
}

@main struct ActionJournalProcessClient {
    static func emit(_ event: ProcessClientEvent) throws {
        var bytes = Data("ALONDRA_CLIENT ".utf8)
        bytes.append(try JSONEncoder().encode(event)); bytes.append(10)
        try FileHandle.standardOutput.write(contentsOf: bytes)
    }
    static func main() async throws {
        guard CommandLine.arguments.count == 3 else { throw ProcessClientError.invalidConfiguration }
        let directory = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)
        let config = try JSONDecoder().decode(ProcessClientConfiguration.self,
            from: Data(contentsOf: URL(fileURLWithPath: CommandLine.arguments[2])))
        let setup = try await setup(config: config, directory: directory)
        let store = try await ViewActionIntentStore(directory: directory.appending(path: "intents"),
            keyMaterial: setup.material, scope: setup.scope)
        let api = setup.api
        let sender = ViewActionSender(transport: api, intents: store)
        try emit(.init(event: "ready", pid: ProcessInfo.processInfo.processIdentifier,
            pendingIDs: store.restored.map(\.request.commandId), pendingPhases: store.restored.map { $0.phase.rawValue }))
        let input = ClientInput()
        var prepared: ActionRequest?
        var actionTask: Task<ActionResponse, any Error>?
        var staleTask: Task<RelayJob, any Error>?
        while true {
            let command = try await input.next()
            do {
                switch command.command {
                case "prepare":
                    guard let nonce = command.nonce else { throw ProcessClientError.invalidCommand }
                    let draft = ActionRequest(commandId: nonce, dataEpoch: config.dataEpoch, client: config.relay == nil ? .mac : .ios, lang: "en",
                        actionId: config.actionID, optionId: config.optionID, resolvesCard: true)
                    prepared = try await sender.prepare(draft)
                    try emit(.init(event: "prepared", request: prepared))
                case "send":
                    guard let request = command.request ?? prepared else { throw ProcessClientError.invalidCommand }
                    let response = try await sender.send(request)
                    try emit(.init(event: "answered", request: request, response: response))
                case "start_action":
                    guard actionTask == nil, let request = command.request ?? prepared else { throw ProcessClientError.invalidCommand }
                    actionTask = Task { try await sender.send(request) }
                    try emit(.init(event: "action_started"))
                case "finish_action":
                    guard let task = actionTask else { throw ProcessClientError.invalidCommand }
                    let response = try await task.value
                    actionTask = nil
                    try emit(.init(event: "answered", response: response))
                case "detail_burst":
                    let feed = FeedSession(transport: api, client: config.relay == nil ? .mac : .ios, lang: "en")
                    let keys = try await withThrowingTaskGroup(of: String.self) { group in
                        for ordinal in 0..<8 {
                            let key = ordinal % 2 == 0 ? "card:fixture-detail-b" : "card:fixture-detail-c"
                            group.addTask {
                                guard case let .document(document) = try await feed.load(key), document.key == key,
                                      document.dataEpoch == config.dataEpoch else { throw ProcessClientError.invalidCommand }
                                return document.key
                            }
                        }
                        var results: [String] = []
                        for try await key in group { results.append(key) }
                        return results.sorted()
                    }
                    await feed.stop()
                    try emit(.init(event: "details_loaded", detailKeys: keys))
                case "hold_stale_authorization":
                    guard staleTask == nil, let relay = config.relay, let expired = relay.expiredBearer,
                          let credentials = setup.credentials, let mapping = setup.mapping, let raw = setup.raw else {
                        throw ProcessClientError.invalidConfiguration
                    }
                    await mapping.holdUnauthorized()
                    await credentials.replace(expired)
                    staleTask = Task { try await raw.status(requestID: "stale-status") }
                    await mapping.waitForUnauthorized()
                    await credentials.replace(relay.bearer)
                    try emit(.init(event: "stale_authorization_held"))
                case "release_stale_authorization":
                    guard let staleTask, let mapping = setup.mapping else { throw ProcessClientError.invalidCommand }
                    await mapping.releaseUnauthorized()
                    do { _ = try await staleTask.value; throw ProcessClientError.invalidCommand }
                    catch RelayWireError.unauthorized {}
                    try emit(.init(event: "stale_authorization_finished", authorizationRefusals: await setup.credentials?.refusals))
                case "reconcile":
                    await sender.reconcilePending(dataEpoch: config.dataEpoch)
                    try emit(.init(event: "reconciled", pendingIDs: await sender.unresolvedCommandIDs()))
                case "transport_metrics":
                    let counts = await setup.mapping?.counts() ?? (0, 0)
                    try emit(.init(event: "transport_metrics", mutativeEnvelopePosts: counts.0, otherEnvelopePosts: counts.1))
                case "drain":
                    await sender.drainRetirement()
                    try emit(.init(event: "retirement_drained"))
                case "replay_http":
                    guard let request = command.request else { throw ProcessClientError.invalidCommand }
                    let reply = try await api.submitAction(request, body: ContractCodec.encode(request))
                    try emit(.init(event: "replayed", response: reply))
                case "fill":
                    guard let count = command.count, (1...256).contains(count) else { throw ProcessClientError.invalidCommand }
                    for ordinal in 1...count {
                        let nonce = String(format: "10000000-0000-4000-8000-%012x", ordinal)
                        let draft = ActionRequest(commandId: nonce, dataEpoch: config.dataEpoch, client: .mac, lang: "en", actionId: "read:unavailable-process-fixture@1")
                        let ticket = try await sender.prepare(draft)
                        guard try await sender.send(ticket).outcome == .noLongerAvailable else { throw ProcessClientError.invalidCommand }
                    }
                    try emit(.init(event: "filled"))
                case "history":
                    try emit(.init(event: "history", history: await compareHistory(directory: directory)))
                case "quit":
                    await setup.mapping?.releaseUnauthorized()
                    if let staleTask { _ = await staleTask.result }
                    if let actionTask { _ = await actionTask.result }
                    await sender.drainRetirement()
                    await setup.stop()
                    try emit(.init(event: "stopped")); return
                default: throw ProcessClientError.invalidCommand
                }
            } catch {
                var kind = String(describing: error)
                var status: Int?
                if let api = error as? APIError {
                    switch api {
                    case .unexpected(let code): status = code
                    case .server(let code, _, _, _): status = code
                    default: break
                    }
                }
                try emit(.init(event: "error", error: kind, status: status))
            }
        }
    }
    private struct Setup: Sendable {
        let api: AlondraAPIClient
        let material: Data
        let scope: ActionIntentScope
        let stop: @Sendable () async -> Void
        let mapping: FixtureRelayHTTP?
        var credentials: FixtureCredentials? = nil
        var raw: (any Transport)? = nil
    }
    private static func setup(config: ProcessClientConfiguration, directory: URL) async throws -> Setup {
        guard let seed = Data(base64Encoded: config.signingSeed) else { throw ProcessClientError.invalidConfiguration }
        let signing = try P256.Signing.PrivateKey(rawRepresentation: seed)
        if let relay = config.relay {
            guard let base = URL(string: "https://fixture-relay.invalid"), let destination = URL(string: relay.baseURL),
                  let agreementSeed = Data(base64Encoded: relay.agreementSeed),
                  let macSigningSeed = Data(base64Encoded: relay.macSigningSeed),
                  let macAgreementSeed = Data(base64Encoded: relay.macAgreementSeed) else { throw ProcessClientError.invalidConfiguration }
            let agreement = try P256.KeyAgreement.PrivateKey(rawRepresentation: agreementSeed)
            let macSigning = try P256.Signing.PrivateKey(rawRepresentation: macSigningSeed).publicKey
            let macAgreement = try P256.KeyAgreement.PrivateKey(rawRepresentation: macAgreementSeed).publicKey
            let revision = SHA256.hash(data: signing.publicKey.x963Representation + agreement.publicKey.x963Representation
                + macSigning.x963Representation + macAgreement.x963Representation).map { String(format: "%02x", $0) }.joined()
            let scope = RelayOutboxScope(relay: base, macID: relay.macID, deviceID: config.deviceID, credentialRevision: revision)
            let mapping = FixtureRelayHTTP(destination: destination)
            let credentials = FixtureCredentials(token: relay.bearer, rejected: relay.expiredBearer ?? relay.bearer)
            let raw = try AlondraAPIClient.makeTransport(base: base, deviceID: config.deviceID, signing: signing,
                bearer: { await credentials.value() }, bearerRefused: { await credentials.refuse() },
                bearerRefusedForToken: { await credentials.refuse(ifMatches: $0) }, transport: mapping)
            let session = try RelaySession(scope: scope, bearer: { await credentials.value() },
                bearerRefusedForToken: { await credentials.refuse(ifMatches: $0) }, transport: mapping,
                journal: EnvelopeJournal(scope: scope, directory: directory.appending(path: "operations")),
                signing: signing, agreement: agreement, macSigning: macSigning, macAgreement: macAgreement)
            return Setup(api: .init(serviceURL: base, deviceID: config.deviceID, operations: session),
                material: agreement.rawRepresentation, scope: .init(service: base.absoluteString + "/" + relay.macID,
                    device: config.deviceID, credentialRevision: revision), stop: { await session.deactivate() }, mapping: mapping, credentials: credentials, raw: raw)
        }
        guard let base = URL(string: config.baseURL) else { throw ProcessClientError.invalidConfiguration }
        let identity = LoopbackIdentityStore(directory: directory.appending(path: "identity"), makeKey: { .software(signing) })
        _ = try await identity.signingKey()
        if await identity.deviceID() == nil { try await identity.save(deviceID: config.deviceID) }
        let material = try await identity.bindingSecret()
        let session = LoopbackSession(base: base, identity: identity, deviceName: "Process fixture client")
        return Setup(api: .init(serviceURL: base, deviceID: config.deviceID, operations: session), material: material,
            scope: .init(service: config.baseURL, device: config.deviceID, credentialRevision: try await identity.credentialRevision()),
            stop: { await session.deactivate() }, mapping: nil)
    }
    private static func compareHistory(directory: URL) async throws -> [ProcessHistorySample] {
        let modernDirectory = directory.appending(path: "history-modern")
        let previousDirectory = directory.appending(path: "history-previous")
        let material = Data(repeating: 23, count: 32)
        let modern = try await ViewActionIntentStore(directory: modernDirectory, keyMaterial: material,
            scope: .init(service: "history", device: "fixture", credentialRevision: "fixed"))
        let previous = try await PreviousActionIntentStore(directory: previousDirectory, keyMaterial: material,
            scope: .init(service: "history", device: "fixture", credentialRevision: "fixed"))
        let response = try ContractCodec.decode(ActionResponse.self,
            from: Data(#"{"protocol":1,"outcome":"applied","stamp":"Saved","revision":2}"#.utf8))
        var samples: [ProcessHistorySample] = []
        var previousWritten = 0
        var previousElapsed = 0.0, previousCPU = 0.0, modernElapsed = 0.0, modernCPU = 0.0
        let clock = ContinuousClock()
        for ordinal in 1...1024 {
            let request = ActionRequest(commandId: String(format: "20000000-0000-4000-8000-%012x", ordinal),
                dataEpoch: "history-epoch", client: .mac, lang: "en", actionId: String(format: "answer:%04d", ordinal))
            let body = try ContractCodec.encode(request)
            let oldStarted = clock.now, oldCPU = try processCPU()
            guard case .created(let old) = try await previous.reserve(request, body: body) else { throw ProcessClientError.invalidCommand }
            previousElapsed += microseconds(oldStarted.duration(to: clock.now))
            previousCPU += try processCPU() - oldCPU
            previousWritten += try ciphertextBytes(previousDirectory)
            let completeStarted = clock.now, completeCPU = try processCPU()
            try await previous.complete(old, response: response)
            previousElapsed += microseconds(completeStarted.duration(to: clock.now))
            previousCPU += try processCPU() - completeCPU
            previousWritten += try ciphertextBytes(previousDirectory)
            let modernStarted = clock.now, modernCPUStarted = try processCPU()
            let ticket = try await modern.prepareNew(request, generation: 1)
            guard case .created(let claimed) = try await modern.reserve(ticket.restoredRequest, body: ticket.body) else { throw ProcessClientError.invalidCommand }
            try await modern.complete(claimed, response: response)
            let acknowledged = try await modern.acknowledgmentRanges()
            try await modern.markAcknowledged(.init(dataEpoch: acknowledged.dataEpoch, generation: acknowledged.generation,
                ranges: acknowledged.ranges.map { .init(first: $0.first, last: $0.last) }))
            modernElapsed += microseconds(modernStarted.duration(to: clock.now))
            modernCPU += try processCPU() - modernCPUStarted
            if [64, 128, 256, 1024].contains(ordinal) {
                let stats = try await modern.statistics()
                samples.append(.init(operations: ordinal, previousFileBytes: try ciphertextBytes(previousDirectory),
                    previousCiphertextPayloadWrittenBytes: previousWritten,
                    previousObservedMicroseconds: previousElapsed, previousObservedCPUMicroseconds: previousCPU,
                    sqliteObservedMicroseconds: modernElapsed, sqliteObservedCPUMicroseconds: modernCPU,
                    sqliteFileBytes: stats.databaseBytes,
                    sqliteEncryptedBytes: stats.encryptedBytes, sqlitePending: stats.pending,
                    sqliteRecent: stats.recent, sqliteIntervals: stats.retiredIntervals))
            }
        }
        return samples
    }
    private static func microseconds(_ duration: Duration) -> Double {
        let value = duration.components
        return Double(value.seconds) * 1_000_000 + Double(value.attoseconds) / 1_000_000_000_000
    }
    private static func processCPU() throws -> Double {
        var usage = rusage()
        guard getrusage(RUSAGE_SELF, &usage) == 0 else { throw ProcessClientError.measurementUnavailable }
        return Double(usage.ru_utime.tv_sec + usage.ru_stime.tv_sec) * 1_000_000
            + Double(usage.ru_utime.tv_usec + usage.ru_stime.tv_usec)
    }
    private static func ciphertextBytes(_ directory: URL) throws -> Int {
        guard let file = try FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil)
            .first(where: { $0.pathExtension == "intents" }) else { throw ProcessClientError.invalidConfiguration }
        return try Data(contentsOf: file).count
    }
}

/// Maps only the fixture origin. Production code still signs/authenticates and seals every byte.
private actor FixtureCredentials {
    private var token: String
    private let rejected: String
    private(set) var refusals = 0
    init(token: String, rejected: String) { self.token = token; self.rejected = rejected }
    func value() -> String { token }
    func replace(_ value: String) { token = value }
    func refuse() { refusals += 1; token = rejected }
    func refuse(ifMatches value: String) { if token == value { refuse() } }
}

private actor FixtureRelayHTTP: HTTPTransport {
    private let destination: URL
    private let network = AlondraAPIClient.makeHTTPTransport()
    private var mutations = 0
    private var others = 0
    private var holdRefusal = false
    private var refusalHeld = false
    private var refusal: CheckedContinuation<Void, Never>?
    private var arrival: CheckedContinuation<Void, Never>?
    func holdUnauthorized() { holdRefusal = true }
    func waitForUnauthorized() async {
        if refusalHeld { return }
        await withCheckedContinuation { arrival = $0 }
    }
    func releaseUnauthorized() { refusal?.resume(); refusal = nil }
    init(destination: URL) { self.destination = destination }
    func send(_ request: URLRequest) async throws -> HTTPReply {
        guard let url = request.url, url.host == "fixture-relay.invalid",
              var components = URLComponents(url: url, resolvingAgainstBaseURL: false),
              let target = URLComponents(url: destination, resolvingAgainstBaseURL: false) else { throw ProcessClientError.invalidConfiguration }
        if request.httpMethod == "POST", url.path == "/v1/requests", let body = request.httpBody,
           let envelope = try? JSONDecoder().decode(Envelope.self, from: body) {
            if ActionOperationID(rawValue: envelope.requestID) != nil { mutations += 1 } else { others += 1 }
        }
        components.scheme = target.scheme; components.host = target.host; components.port = target.port
        guard let mapped = components.url else { throw ProcessClientError.invalidConfiguration }
        var actual = request; actual.url = mapped
        let reply = try await network.send(actual)
        if holdRefusal, reply.status == 401 {
            holdRefusal = false
            await withCheckedContinuation {
                refusal = $0
                refusalHeld = true
                arrival?.resume(); arrival = nil
            }
        }
        return reply
    }
    func counts() -> (Int, Int) { (mutations, others) }
}
