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
            let session = try RelaySession(scope: scope, bearer: { relay.bearer }, transport: mapping,
                journal: EnvelopeJournal(scope: scope, directory: directory.appending(path: "operations")),
                signing: signing, agreement: agreement, macSigning: macSigning, macAgreement: macAgreement)
            return Setup(api: .init(serviceURL: base, deviceID: config.deviceID, operations: session),
                material: agreement.rawRepresentation, scope: .init(service: base.absoluteString + "/" + relay.macID,
                    device: config.deviceID, credentialRevision: revision), stop: { await session.deactivate() }, mapping: mapping)
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
private actor FixtureRelayHTTP: HTTPTransport {
    private let destination: URL
    private let network = AlondraAPIClient.makeHTTPTransport()
    private var mutations = 0
    private var others = 0
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
        return try await network.send(actual)
    }
    func counts() -> (Int, Int) { (mutations, others) }
}
