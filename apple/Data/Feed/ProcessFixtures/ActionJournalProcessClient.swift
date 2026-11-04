@testable import AlondraFeed
import AlondraContract
import AlondraIroh
@testable import AlondraTransport
import CryptoKit
import Foundation
import Darwin
import Synchronization

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
        let sender = ViewActionSender(transport: api, intents: store,
            retryPolicy: config.measure == true ? .live
                : ActionRetryPolicy(delay: { _ in .milliseconds(50) }, sleep: { try await Task.sleep(for: $0) }))
        try emit(.init(event: "ready", pid: ProcessInfo.processInfo.processIdentifier,
            pendingIDs: store.restored.map(\.request.commandId), pendingPhases: store.restored.map { $0.phase.rawValue },
            undelivered: sender.undelivered.current.sorted()))
        let input = ClientInput()
        let client: ViewClient = config.iroh == nil ? .mac : .ios
        var prepared: ActionRequest?
        var actionTask: Task<ActionResponse, any Error>?
        while true {
            let command = try await input.next()
            do {
                switch command.command {
                case "go_idle":
                    await setup.carrier?.stop()
                    try emit(.init(event: "idle"))
                case "go_live":
                    await setup.carrier?.start()
                    try emit(.init(event: "live"))
                case "timings":
                    try emit(.init(event: "timings", connections: await setup.network?.connectionCount(),
                        timings: await setup.network?.timings()))
                case "prepare":
                    guard let nonce = command.nonce else { throw ProcessClientError.invalidCommand }
                    await setup.network?.mark("tap")
                    let draft = ActionRequest(commandId: nonce, dataEpoch: config.dataEpoch, client: client, lang: "en",
                        actionId: config.actionID, optionId: config.optionID, resolvesCard: true)
                    prepared = try await sender.prepare(draft)
                    await setup.network?.mark("prepared")
                    try emit(.init(event: "prepared", request: prepared))
                case "send":
                    guard let request = command.request ?? prepared else { throw ProcessClientError.invalidCommand }
                    let response = try await sender.send(request)
                    try emit(.init(event: "answered", request: request, response: response))
                case "network":
                    guard let faults = command.network, let network = setup.network else { throw ProcessClientError.invalidCommand }
                    await network.impose(faults)
                    try emit(.init(event: "network_set"))
                case "deliver":
                    guard let request = command.request ?? prepared else { throw ProcessClientError.invalidCommand }
                    await setup.network?.mark("deliver")
                    let response = try await sender.deliver(request)
                    await setup.network?.mark("answered")
                    try emit(.init(event: "answered", request: request, response: response,
                        pendingIDs: await sender.unresolvedCommandIDs(), undelivered: sender.undelivered.current.sorted()))
                case "start_delivery":
                    guard actionTask == nil, let request = command.request ?? prepared else { throw ProcessClientError.invalidCommand }
                    actionTask = Task { try await sender.deliver(request) }
                    try emit(.init(event: "delivery_started"))
                case "resume":
                    await sender.reconcilePending(dataEpoch: config.dataEpoch)
                    await sender.resumeDeliveries(dataEpoch: config.dataEpoch)
                    await sender.drainDeliveries()
                    try emit(.init(event: "resumed", pendingIDs: await sender.unresolvedCommandIDs(),
                        undelivered: sender.undelivered.current.sorted()))
                case "start_action":
                    guard actionTask == nil, let request = command.request ?? prepared else { throw ProcessClientError.invalidCommand }
                    actionTask = Task { try await sender.send(request) }
                    try emit(.init(event: "action_started"))
                case "finish_action":
                    guard let task = actionTask else { throw ProcessClientError.invalidCommand }
                    actionTask = nil
                    let response = try await task.value
                    await setup.network?.mark("answered")
                    try emit(.init(event: "answered", response: response))
                case "detail_burst":
                    let feed = FeedSession(transport: api, client: client, lang: "en")
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
                case "measured_detail_burst":
                    let results = await withTaskGroup(of: String?.self) { group in
                        for ordinal in 0..<8 {
                            let key = ordinal % 2 == 0 ? "card:fixture-detail-b" : "card:fixture-detail-c"
                            group.addTask {
                                do {
                                    guard case let .document(document) = try await api.fetchView(key: key, client: .ios, lang: "en"),
                                          document.key == key, document.dataEpoch == config.dataEpoch else { return nil }
                                    return key
                                } catch { return nil }
                            }
                        }
                        var keys: [String] = []
                        var errors = 0
                        for await key in group {
                            if let key { keys.append(key) } else { errors += 1 }
                        }
                        return (keys.sorted(), errors)
                    }
                    try emit(.init(event: "details_measured", detailKeys: results.0, readAttempts: 8, readErrors: results.1))
                case "connections":
                    guard let network = setup.network else { throw ProcessClientError.invalidCommand }
                    try emit(.init(event: "connections", connections: await network.connectionCount()))
                case "await_connections":
                    guard let count = command.count, let network = setup.network else { throw ProcessClientError.invalidCommand }
                    await network.waitForConnections(count)
                    try emit(.init(event: "connected", connections: await network.connectionCount()))
                case "reconcile":
                    await sender.reconcilePending(dataEpoch: config.dataEpoch)
                    try emit(.init(event: "reconciled", pendingIDs: await sender.unresolvedCommandIDs()))
                case "transport_metrics":
                    let counts = await setup.network?.counts() ?? (0, 0)
                    let results = await setup.network?.operationResultIDs()
                    try emit(.init(event: "transport_metrics", mutativeEnvelopePosts: counts.0, otherEnvelopePosts: counts.1,
                        operationResultGets: results?.count, operationResultIDs: results))
                case "await_answer_attempt":
                    guard let network = setup.network else { throw ProcessClientError.invalidCommand }
                    await network.waitForAnswerAttempt()
                    try emit(.init(event: "answer_attempted"))
                case "hold_retirement":
                    guard let network = setup.network else { throw ProcessClientError.invalidCommand }
                    await network.holdRetirement()
                    try emit(.init(event: "retirement_held"))
                case "release_retirement":
                    guard let network = setup.network else { throw ProcessClientError.invalidCommand }
                    await network.releaseRetirement()
                    await sender.drainRetirement()
                    try emit(.init(event: "retirement_drained"))
                case "drain":
                    await sender.drainRetirement()
                    try emit(.init(event: "retirement_drained"))
                case "resend":
                    guard let request = command.request ?? prepared,
                          let reply = try await api.resendAction(request, body: ContractCodec.encode(request)) else { throw ProcessClientError.invalidCommand }
                    try emit(.init(event: "replayed", response: reply))
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
                    await setup.network?.releaseRetirement()
                    if let actionTask { _ = await actionTask.result }
                    await sender.drainRetirement()
                    await setup.stop()
                    try emit(.init(event: "stopped")); return
                default: throw ProcessClientError.invalidCommand
                }
            } catch {
                var status: Int?
                if let api = error as? APIError {
                    switch api {
                    case .unexpected(let code): status = code
                    case .server(let code, _, _, _): status = code
                    default: break
                    }
                }
                try emit(.init(event: "error", error: String(describing: error), status: status))
            }
        }
    }
    private struct Setup: Sendable {
        let api: AlondraAPIClient
        let material: Data
        let scope: ActionIntentScope
        let stop: @Sendable () async -> Void
        var network: FixtureIrohNetwork? = nil
        var carrier: IrohCarrier? = nil
    }
    private static func setup(config: ProcessClientConfiguration, directory: URL) async throws -> Setup {
        guard let seed = Data(base64Encoded: config.signingSeed) else { throw ProcessClientError.invalidConfiguration }
        let signing = try P256.Signing.PrivateKey(rawRepresentation: seed)
        if let iroh = config.iroh {
            guard let base = URL(string: "alondra-direct://" + iroh.macID),
                  let agreementSeed = Data(base64Encoded: iroh.agreementSeed),
                  let macSigningSeed = Data(base64Encoded: iroh.macSigningSeed),
                  let macAgreementSeed = Data(base64Encoded: iroh.macAgreementSeed),
                  let address = IrohMacAddress(endpointID: iroh.endpoint, relay: iroh.relay.flatMap(URL.init(string:)),
                    directAddresses: iroh.relay == nil ? [iroh.address] : [])
            else { throw ProcessClientError.invalidConfiguration }
            let agreement = try P256.KeyAgreement.PrivateKey(rawRepresentation: agreementSeed)
            let macSigning = try P256.Signing.PrivateKey(rawRepresentation: macSigningSeed).publicKey
            let macAgreement = try P256.KeyAgreement.PrivateKey(rawRepresentation: macAgreementSeed)
            let revision = SHA256.hash(data: signing.publicKey.x963Representation + agreement.publicKey.x963Representation
                + macSigning.x963Representation + macAgreement.publicKey.x963Representation).map { String(format: "%02x", $0) }.joined()
            let scope = EnvelopeJournalScope(relay: base, macID: iroh.macID, deviceID: config.deviceID, credentialRevision: revision)
            let network = FixtureIrohNetwork(phoneSigning: signing.publicKey, macAgreement: macAgreement,
                macID: iroh.macID, deviceID: config.deviceID, measure: config.measure == true)
            let dialer = FixtureIrohDialer(IrohEndpointDialer(identity: IrohIdentity(pairingSigning: signing, macID: iroh.macID)),
                network: network)
            // Reconnects at once, so a test that brings the endpoint back does not wait out the app's backoff.
            let carrier = IrohCarrier(dialer: dialer, macID: iroh.macID, deviceID: config.deviceID, signing: signing,
                address: { address }, connectWait: .seconds(15), dialTimeout: .seconds(2), retryDelays: [.milliseconds(100)])
            let session = DirectSession(macID: iroh.macID, deviceID: config.deviceID, signing: signing, agreement: agreement,
                macSigning: macSigning, macAgreement: macAgreement.publicKey, iroh: carrier,
                journal: try EnvelopeJournal(scope: scope, directory: directory.appending(path: "operations")))
            let routed = RoutedSession(iroh: .init(session: session, carrier: carrier), direct: nil, router: nil)
            if config.startsLive != false { await carrier.start() }
            return Setup(api: .init(serviceURL: base, deviceID: config.deviceID, operations: routed),
                material: agreement.rawRepresentation, scope: .init(service: base.absoluteString + "/" + iroh.macID,
                    device: config.deviceID, credentialRevision: revision),
                stop: { await routed.deactivate() }, network: network, carrier: carrier)
        }
        guard let base = URL(string: config.baseURL) else { throw ProcessClientError.invalidConfiguration }
        let identity = LoopbackIdentityStore(directory: directory.appending(path: "identity"), makeKey: { .software(signing) })
        _ = try await identity.signingKey()
        if await identity.deviceID() == nil { try await identity.save(deviceID: config.deviceID) }
        let material = try await identity.bindingSecret()
        let session = LoopbackSession(base: base, identity: identity, deviceName: "Process fixture client")
        return Setup(api: .init(serviceURL: base, deviceID: config.deviceID, operations: session), material: material,
            scope: .init(service: config.baseURL, device: config.deviceID, credentialRevision: try await identity.credentialRevision()),
            stop: { await session.deactivate() })
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
        for ordinal in 1...512 {
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
            if [64, 128, 256, 512].contains(ordinal) {
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

/// The real iroh dialer, with every connection it makes going through `FixtureIrohNetwork`.
private final class FixtureIrohDialer: IrohDialer {
    let endpointID: Data
    private let dialer: IrohEndpointDialer
    private let network: FixtureIrohNetwork

    init(_ dialer: IrohEndpointDialer, network: FixtureIrohNetwork) {
        endpointID = dialer.endpointID
        self.dialer = dialer
        self.network = network
    }

    func dial(_ mac: IrohMacAddress) async throws -> any IrohLink {
        await network.mark("dial_begin")
        let link = try await dialer.dial(mac)
        await network.mark("dial_end")
        return FixtureIrohLink(link, network: network)
    }

    func networkMoved() async { await dialer.networkMoved() }

    func shutdown() async { await dialer.shutdown() }
}

private final class FixtureIrohLink: IrohLink {
    private let link: any IrohLink
    private let network: FixtureIrohNetwork
    private let stale = Mutex(false)

    init(_ link: any IrohLink, network: FixtureIrohNetwork) {
        self.link = link
        self.network = network
    }

    func exchange(_ request: Data, answerLimit: Int) async throws -> Data {
        guard let envelope = try? JSONDecoder().decode(Envelope.self, from: request) else {
            await network.mark("hello_begin")
            let reply = try await link.exchange(request, answerLimit: answerLimit)
            await network.mark("hello_end")
            if IrohHello.accepted(reply) { await network.connected() }
            return reply
        }
        let disposition = try await network.leaving(envelope)
        if await network.makeStale(envelope) {
            stale.withLock { $0 = true }
            throw IrohLinkError.noAnswer
        }
        if stale.withLock({ $0 }), disposition.resultRead {
            await network.mark("stale_wait_begin", operation: envelope.requestID)
            let held = AsyncStream<Void>.makeStream()
            for await _ in held.stream {}
            held.continuation.finish()
            await network.mark("stale_wait_end", operation: envelope.requestID)
            throw CancellationError()
        }
        let reply = try await link.exchange(request, answerLimit: answerLimit)
        await network.mark("reply", operation: envelope.requestID)
        if disposition.replyLost { throw IrohLinkError.noAnswer }
        return reply
    }

    func path() -> IrohPath { link.path() }
    func pathChanges() -> AsyncStream<IrohPath> { link.pathChanges() }
    var isClosed: Bool { link.isClosed }
    func close() async { await link.close() }
}

/// Opens only what the client seals, to count and fault it by its path; the Mac still verifies
/// and answers every byte.
private actor FixtureIrohNetwork {
    private let phoneSigning: P256.Signing.PublicKey
    private let macAgreement: P256.KeyAgreement.PrivateKey
    private let macID: String
    private let deviceID: String
    private var faults = ProcessNetworkFaults()
    private var mutations = 0
    private var others = 0
    private var operationResults: [String] = []
    private var answerAttempts: [CheckedContinuation<Void, Never>] = []
    private var connections = 0
    private var connectionWaiters: [(Int, CheckedContinuation<Void, Never>)] = []
    private var holdAcknowledgments = false
    private var acknowledgmentReleases: [CheckedContinuation<Void, Never>] = []
    private let measure: Bool
    private var samples: [ProcessTiming] = []

    init(phoneSigning: P256.Signing.PublicKey, macAgreement: P256.KeyAgreement.PrivateKey, macID: String, deviceID: String,
         measure: Bool = false) {
        self.phoneSigning = phoneSigning; self.macAgreement = macAgreement; self.macID = macID; self.deviceID = deviceID
        self.measure = measure
    }

    func mark(_ stage: String, operation: String? = nil) {
        guard measure else { return }
        samples.append(.init(stage: stage, us: Date().timeIntervalSince1970 * 1_000_000, operation: operation))
    }

    func timings() -> [ProcessTiming] { samples }

    func impose(_ faults: ProcessNetworkFaults) { self.faults = faults }

    /// One stream fails without a known outcome; the same connection then silently loses
    /// recovery reads. A fresh connection still reaches the real daemon over the working network.
    func makeStale(_ envelope: Envelope) -> Bool {
        guard faults.staleAnswers > 0, ActionOperationID(rawValue: envelope.requestID) != nil else { return false }
        faults.staleAnswers -= 1
        return true
    }

    func connected() {
        connections += 1
        var waiting: [(Int, CheckedContinuation<Void, Never>)] = []
        for (count, waiter) in connectionWaiters {
            if connections >= count { waiter.resume() } else { waiting.append((count, waiter)) }
        }
        connectionWaiters = waiting
    }

    func connectionCount() -> Int { connections }

    func waitForConnections(_ count: Int) async {
        if connections >= count { return }
        await withCheckedContinuation { connectionWaiters.append((count, $0)) }
    }

    func waitForAnswerAttempt() async {
        if mutations > 0 { return }
        await withCheckedContinuation { answerAttempts.append($0) }
    }

    func holdRetirement() { holdAcknowledgments = true }

    func releaseRetirement() {
        holdAcknowledgments = false
        for waiter in acknowledgmentReleases { waiter.resume() }
        acknowledgmentReleases.removeAll()
    }

    /// Whether the reply to `envelope` is lost once the Mac has it; throws when it never leaves.
    func leaving(_ envelope: Envelope) async throws -> (replyLost: Bool, resultRead: Bool) {
        let bytes = try envelope.open(senderKey: phoneSigning, recipientKey: macAgreement,
            sender: deviceID, recipient: macID, route: macID, nowMilliseconds: nil)
        let path = (try JSONSerialization.jsonObject(with: bytes) as? [String: Any])?["path"] as? String ?? ""
        let answer = ActionOperationID(rawValue: envelope.requestID) != nil
        mark(answer ? "action_send" : "read_send", operation: envelope.requestID)
        if answer {
            mutations += 1
            for waiter in answerAttempts { waiter.resume() }
            answerAttempts.removeAll()
        } else { others += 1 }
        let result = path.hasPrefix("/operation-results/")
        if result, let components = URLComponents(string: path) {
            operationResults.append(String(components.path.dropFirst("/operation-results/".count)))
        }
        if answer, faults.cutAnswers > 0 {
            faults.cutAnswers -= 1
            throw IrohLinkError.notSent
        }
        if path == "/operation-journal/ack", holdAcknowledgments {
            await withCheckedContinuation { acknowledgmentReleases.append($0) }
        }
        if answer, faults.lostReplies > 0 {
            faults.lostReplies -= 1
            return (true, result)
        }
        if result, faults.lostStatus > 0 {
            faults.lostStatus -= 1
            return (true, result)
        }
        return (false, result)
    }

    func counts() -> (Int, Int) { (mutations, others) }
    func operationResultIDs() -> [String] { operationResults }
}
