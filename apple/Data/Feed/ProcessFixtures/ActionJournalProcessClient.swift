@testable import AlondraFeed
import AlondraContract
@testable import AlondraTransport
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
        let sender = ViewActionSender(transport: api, intents: store,
            retryPolicy: ActionRetryPolicy(delay: { _ in .milliseconds(50) }, sleep: { try await Task.sleep(for: $0) }))
        try emit(.init(event: "ready", pid: ProcessInfo.processInfo.processIdentifier,
            pendingIDs: store.restored.map(\.request.commandId), pendingPhases: store.restored.map { $0.phase.rawValue },
            undelivered: sender.undelivered.current.sorted()))
        let input = ClientInput()
        var prepared: ActionRequest?
        var actionTask: Task<ActionResponse, any Error>?
        var staleTask: Task<RelayJob, any Error>?
        var coldReads: [Task<String, any Error>] = []
        var detailTask: Task<String, any Error>?
        var transferTask: Task<ViewUploadedAttachment, any Error>?
        let measurementStarted = ContinuousClock().now
        let cpuStarted = try processCPU()
        let transferData = Data((0..<(192 * 1024 + 17)).map { UInt8($0 % 251) })
        while true {
            let command = try await input.next()
            do {
                switch command.command {
                case "preload_noise":
                    guard config.deviceID == "noise-phone", let raw = setup.raw, let codec = setup.codec else { throw ProcessClientError.invalidCommand }
                    var ids: [String] = [], bytes = 0
                    for ordinal in 0..<32 {
                        let id = String(format: "noise-%03d", ordinal)
                        let envelope = try codec.sealRequest(requestID: id, method: "GET",
                            path: "/views/card%3Afixture-detail-b?protocol=1&client=ios&lang=en", body: nil)
                        let job = try await raw.submit(envelope)
                        guard job.requestID == id, ["queued", "dispatched"].contains(job.status) else { throw ProcessClientError.invalidCommand }
                        ids.append(id); bytes += try JSONEncoder().encode(envelope).count
                    }
                    try emit(.init(event: "noise_preloaded", envelopeIDs: ids, envelopeBytes: bytes))
                case "await_action_admitted":
                    guard let mapping = setup.mapping, let request = prepared else { throw ProcessClientError.invalidCommand }
                    let id = await mapping.waitForAccepted(containing: request.commandId)
                    try emit(.init(event: "action_admitted", envelopeIDs: [id]))
                case "start_detail", "start_blocked_read":
                    guard detailTask == nil, let key = command.key, let mapping = setup.mapping else { throw ProcessClientError.invalidCommand }
                    let rejectedBefore = await mapping.rejectionCount()
                    detailTask = Task {
                        guard case let .document(document) = try await api.fetchView(key: key, client: .ios, lang: "en"),
                              document.key == key, document.dataEpoch == config.dataEpoch else { throw ProcessClientError.invalidCommand }
                        return document.key
                    }
                    if command.command == "start_blocked_read" {
                        await mapping.waitForRejections(rejectedBefore + 1)
                        try emit(.init(event: "read_admission_rejected", admissionRejections: await mapping.rejectionCount()))
                    } else {
                        let id = await mapping.waitForAccepted(containing: key.replacingOccurrences(of: ":", with: "%3A"))
                        try emit(.init(event: "detail_admitted", envelopeIDs: [id]))
                    }
                case "finish_detail":
                    guard let task = detailTask else { throw ProcessClientError.invalidCommand }
                    let key = try await task.value
                    try emit(.init(event: "detail_finished", detailKeys: [key], readAttempts: 1, readErrors: 0))
                    detailTask = nil
                case "start_transfer":
                    guard transferTask == nil, let mapping = setup.mapping,
                          let id = UUID(uuidString: "70000000-0000-4000-8000-000000000001") else { throw ProcessClientError.invalidCommand }
                    await mapping.holdFirstTransferChunk()
                    transferTask = Task {
                        do { return try await api.uploadViewAttachment(transferData, name: "saturation.bin", transferID: id) }
                        catch { await mapping.transferFailed(error); throw error }
                    }
                    try await mapping.waitForTransferChunk()
                    try emit(.init(event: "transfer_chunk_held"))
                case "release_transfer":
                    guard transferTask != nil, let mapping = setup.mapping else { throw ProcessClientError.invalidCommand }
                    let rejectedBefore = await mapping.rejectionCount()
                    await mapping.releaseTransferChunk()
                    await mapping.waitForRejections(rejectedBefore + 1)
                    try emit(.init(event: "transfer_admission_rejected", admissionRejections: await mapping.rejectionCount()))
                case "finish_transfer":
                    guard let transferTask else { throw ProcessClientError.invalidCommand }
                    let uploaded = try await transferTask.value
                    let downloaded = try await downloadTransfer(api: api, uploaded: uploaded)
                    guard downloaded == transferData else { throw ProcessClientError.invalidCommand }
                    try emit(.init(event: "transfer_finished", transferBytes: downloaded.count, transferDigest: uploaded.contentDigest))
                case "verify_noise":
                    guard let raw = setup.raw, let codec = setup.codec else { throw ProcessClientError.invalidCommand }
                    var ids: [String] = []
                    for ordinal in 0..<32 {
                        let id = String(format: "noise-%03d", ordinal)
                        let receipt = try await raw.waitForRequest(requestID: id, waitSeconds: 25)
                        guard let response = try codec.verified(receipt, requestID: id),
                              case let .completed(status, body) = response, status == 200,
                              case let .document(document) = try ContractCodec.decode(ViewFetch.self, from: body),
                              document.key == "card:fixture-detail-b", document.dataEpoch == config.dataEpoch else { throw ProcessClientError.invalidCommand }
                        try await raw.acknowledgeResult(requestID: id, revision: receipt.revision)
                        ids.append(id)
                    }
                    try emit(.init(event: "noise_verified", readAttempts: 32, readErrors: 0, envelopeIDs: ids))
                case "drain_acknowledgments":
                    guard let session = setup.session else { throw ProcessClientError.invalidCommand }
                    await sender.drainRetirement()
                    await session.drainAcknowledgments()
                    try emit(.init(event: "acknowledgments_drained"))
                case "saturation_metrics":
                    guard let mapping = setup.mapping else { throw ProcessClientError.invalidCommand }
                    let metrics = await mapping.saturationMetrics()
                    var usage = rusage()
                    guard getrusage(RUSAGE_SELF, &usage) == 0 else { throw ProcessClientError.measurementUnavailable }
                    try emit(.init(event: "saturation_metrics", envelopeIDs: metrics.ids,
                        admissionRejections: metrics.rejections, immutableResubmissions: metrics.immutable,
                        observedMicroseconds: microseconds(measurementStarted.duration(to: ContinuousClock().now)),
                        observedCPUMicroseconds: try processCPU() - cpuStarted, peakResidentBytes: Int(usage.ru_maxrss)))
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
                case "network":
                    guard let faults = command.network, let mapping = setup.mapping else { throw ProcessClientError.invalidCommand }
                    await mapping.impose(faults)
                    try emit(.init(event: "network_set"))
                case "deliver":
                    guard let request = command.request ?? prepared else { throw ProcessClientError.invalidCommand }
                    let response = try await sender.deliver(request)
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
                    try emit(.init(event: "answered", response: response))
                case "start_cold_reads":
                    guard coldReads.isEmpty, let mapping = setup.mapping, let session = setup.session else { throw ProcessClientError.invalidCommand }
                    let (arrivals, continuation) = AsyncStream<Int>.makeStream(bufferingPolicy: .bufferingNewest(1))
                    await session.observeBootstrapWaiters { continuation.yield($0) }
                    await mapping.holdBootstrap()
                    for ordinal in 0..<4 {
                        let key = ordinal % 2 == 0 ? "card:fixture-detail-b" : "card:fixture-detail-c"
                        coldReads.append(Task {
                            guard case let .document(document) = try await api.fetchView(key: key, client: .ios, lang: "en"),
                                  document.key == key, document.dataEpoch == config.dataEpoch else { throw ProcessClientError.invalidCommand }
                            return key
                        })
                    }
                    await mapping.waitForBootstrap()
                    for await count in arrivals where count == 4 { break }
                    await session.observeBootstrapWaiters(nil)
                    continuation.finish()
                    try emit(.init(event: "cold_bootstrap_held"))
                case "finish_cold_reads":
                    guard !coldReads.isEmpty, let mapping = setup.mapping else { throw ProcessClientError.invalidCommand }
                    let releasedAt = await mapping.releaseBootstrap()
                    var keys: [String] = []
                    var errors = 0
                    for task in coldReads {
                        do { keys.append(try await task.value) }
                        catch { errors += 1 }
                    }
                    let completionMicroseconds = microseconds(releasedAt.duration(to: ContinuousClock().now))
                    let attempts = coldReads.count
                    coldReads.removeAll()
                    try emit(.init(event: "cold_reads_finished", detailKeys: keys.sorted(), readAttempts: attempts, readErrors: errors,
                        coldCompletionMicroseconds: completionMicroseconds))
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
                    let reads = await setup.mapping?.readCounts()
                    try emit(.init(event: "transport_metrics", mutativeEnvelopePosts: counts.0, otherEnvelopePosts: counts.1,
                        bootstrapGets: reads?.bootstraps, operationResultGets: reads?.results.count, operationResultIDs: reads?.results))
                case "await_answer_attempt":
                    while await (setup.mapping?.counts().0 ?? 1) == 0 { try await Task.sleep(for: .milliseconds(20)) }
                    try emit(.init(event: "answer_attempted"))
                case "hold_retirement":
                    guard let mapping = setup.mapping else { throw ProcessClientError.invalidCommand }
                    await mapping.holdRetirement()
                    try emit(.init(event: "retirement_held"))
                case "release_retirement":
                    guard let mapping = setup.mapping else { throw ProcessClientError.invalidCommand }
                    await mapping.releaseRetirement()
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
                    await setup.mapping?.releaseBootstrap()
                    await setup.mapping?.releaseTransferChunk()
                    for task in coldReads { task.cancel(); _ = await task.result }
                    await setup.mapping?.releaseUnauthorized()
                    await setup.mapping?.releaseRetirement()
                    if let staleTask { _ = await staleTask.result }
                    if let actionTask { _ = await actionTask.result }
                    if let detailTask { _ = await detailTask.result }
                    if let transferTask { _ = await transferTask.result }
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
        var session: RelaySession? = nil
        var codec: EnvelopeCodec? = nil
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
            let mapping = FixtureRelayHTTP(destination: destination, phoneSigning: signing.publicKey,
                macAgreement: try P256.KeyAgreement.PrivateKey(rawRepresentation: macAgreementSeed), macID: relay.macID, deviceID: config.deviceID)
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
                    device: config.deviceID, credentialRevision: revision), stop: { await session.deactivate() }, mapping: mapping, credentials: credentials, raw: raw, session: session,
                codec: EnvelopeCodec(macID: relay.macID, deviceID: config.deviceID, signing: signing, agreement: agreement,
                    macSigning: macSigning, macAgreement: macAgreement, now: { UInt64(Date().timeIntervalSince1970 * 1000) }))
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
    private struct DownloadChunk: Decodable, Sendable {
        let contentDigest: String
        let version: String
        let data: String
        let offset: Int
        let total: Int
        let nextOffset: Int
        let eof: Bool
        enum CodingKeys: String, CodingKey {
            case version, data, offset, total, eof
            case contentDigest = "content_digest", nextOffset = "next_offset"
        }
    }
    private static func downloadTransfer(api: AlondraAPIClient, uploaded: ViewUploadedAttachment) async throws -> Data {
        guard uploaded.contentReference.hasPrefix("files/") else { throw ProcessClientError.invalidCommand }
        var downloaded = Data(), version: String?
        for _ in 0..<4 {
            let query = [("path", uploaded.contentReference), ("offset", String(downloaded.count))]
                + (version.map { [("version", $0)] } ?? [])
            let chunk = try await api.send(Endpoint<DownloadChunk>(.get, path: ["files", "chunk"], query: query) {
                try JSONDecoder().decode(DownloadChunk.self, from: $0)
            })
            let encoded = chunk.data.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
            guard chunk.contentDigest == uploaded.contentDigest, !chunk.version.isEmpty,
                  version == nil || version == chunk.version, chunk.total == Int(uploaded.bytes),
                  chunk.offset == downloaded.count, chunk.nextOffset <= chunk.total,
                  chunk.nextOffset > chunk.offset,
                  let bytes = Data(base64Encoded: encoded + String(repeating: "=", count: (4 - encoded.count % 4) % 4)),
                  bytes.count == chunk.nextOffset - chunk.offset else { throw ProcessClientError.invalidCommand }
            downloaded.append(bytes); version = chunk.version
            if chunk.eof {
                guard downloaded.count == Int(uploaded.bytes), chunk.nextOffset == chunk.total else { throw ProcessClientError.invalidCommand }
                return downloaded
            }
        }
        throw ProcessClientError.invalidCommand
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
    private var bootstraps = 0
    private var operationResults: [String] = []
    private var accepted: [(id: String, path: String)] = []
    private var acceptanceWaiters: [(String, CheckedContinuation<String, Never>)] = []
    private var rejectedAdmissions = 0
    private var rejectionWaiters: [(Int, CheckedContinuation<Void, Never>)] = []
    private var envelopeBodies: [String: Data] = [:]
    private var immutableBodies = true
    private var holdTransferChunk = false
    private var transferChunkHeld = false
    private var transferChunkArrival: CheckedContinuation<Void, any Error>?
    private var transferStartupError: (any Error)?
    private var transferChunkRelease: CheckedContinuation<Void, Never>?
    func holdFirstTransferChunk() { holdTransferChunk = true }
    func waitForTransferChunk() async throws {
        if transferChunkHeld { return }
        if let transferStartupError { throw transferStartupError }
        try await withCheckedThrowingContinuation { transferChunkArrival = $0 }
    }
    func transferFailed(_ error: any Error) {
        transferStartupError = error
        transferChunkArrival?.resume(throwing: error); transferChunkArrival = nil
    }
    func releaseTransferChunk() { transferChunkRelease?.resume(); transferChunkRelease = nil }
    func rejectionCount() -> Int { rejectedAdmissions }
    func waitForRejections(_ count: Int) async {
        if rejectedAdmissions >= count { return }
        await withCheckedContinuation { rejectionWaiters.append((count, $0)) }
    }
    func waitForAccepted(containing value: String) async -> String {
        if let item = accepted.first(where: { $0.id == value || $0.path.contains(value) }) { return item.id }
        return await withCheckedContinuation { acceptanceWaiters.append((value, $0)) }
    }
    func saturationMetrics() -> (ids: [String], rejections: Int, immutable: Bool) {
        (envelopeBodies.keys.sorted(), rejectedAdmissions, immutableBodies)
    }
    private let phoneSigning: P256.Signing.PublicKey
    private let macAgreement: P256.KeyAgreement.PrivateKey
    private let macID: String
    private let deviceID: String
    private var holdBootstrapRequest = false
    private var bootstrapHeld = false
    private var bootstrapReleases: [CheckedContinuation<Void, Never>] = []
    private var bootstrapArrival: CheckedContinuation<Void, Never>?
    func holdBootstrap() { holdBootstrapRequest = true }
    func waitForBootstrap() async {
        if bootstrapHeld { return }
        await withCheckedContinuation { bootstrapArrival = $0 }
    }
    @discardableResult func releaseBootstrap() -> ContinuousClock.Instant {
        let releasedAt = ContinuousClock().now
        holdBootstrapRequest = false
        for waiter in bootstrapReleases { waiter.resume() }
        bootstrapReleases.removeAll()
        return releasedAt
    }
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
    private var holdJournalAcknowledgments = false
    private var journalAcknowledgmentReleases: [CheckedContinuation<Void, Never>] = []
    func holdRetirement() { holdJournalAcknowledgments = true }
    func releaseRetirement() {
        holdJournalAcknowledgments = false
        for waiter in journalAcknowledgmentReleases { waiter.resume() }
        journalAcknowledgmentReleases.removeAll()
    }
    private var faults = ProcessNetworkFaults()
    func impose(_ faults: ProcessNetworkFaults) { self.faults = faults }
    init(destination: URL, phoneSigning: P256.Signing.PublicKey, macAgreement: P256.KeyAgreement.PrivateKey, macID: String, deviceID: String) {
        self.destination = destination; self.phoneSigning = phoneSigning; self.macAgreement = macAgreement
        self.macID = macID; self.deviceID = deviceID
    }
    func send(_ request: URLRequest) async throws -> HTTPReply {
        guard let url = request.url, url.host == "fixture-relay.invalid",
              var components = URLComponents(url: url, resolvingAgainstBaseURL: false),
              let target = URLComponents(url: destination, resolvingAgainstBaseURL: false) else { throw ProcessClientError.invalidConfiguration }
        var answer = false, read = false
        var submitted: (id: String, path: String)?
        if request.httpMethod == "POST", url.path == "/v1/requests", let body = request.httpBody,
           let envelope = try? JSONDecoder().decode(Envelope.self, from: body) {
            if let previous = envelopeBodies[envelope.requestID], previous != body { immutableBodies = false }
            envelopeBodies[envelope.requestID] = body
            answer = ActionOperationID(rawValue: envelope.requestID) != nil
            read = envelope.requestID.hasPrefix("read-")
            if answer { mutations += 1 } else { others += 1 }
            let bytes = try envelope.open(senderKey: phoneSigning, recipientKey: macAgreement,
                sender: deviceID, recipient: macID, route: macID, nowMilliseconds: nil)
            if let payload = try JSONSerialization.jsonObject(with: bytes) as? [String: Any], let path = payload["path"] as? String {
                submitted = (envelope.requestID, path)
            }
            if let payload = try JSONSerialization.jsonObject(with: bytes) as? [String: Any],
               payload["method"] as? String == "GET", let path = payload["path"] as? String,
               path.hasPrefix("/operation-results/"), let components = URLComponents(string: path) {
                operationResults.append(String(components.path.dropFirst("/operation-results/".count)))
            }
        }
        let status = request.httpMethod == "GET" && url.path.hasPrefix("/v1/requests/op_")
        read = read || request.httpMethod == "GET" && url.path.hasPrefix("/v1/requests/read-")
        if answer, faults.cutAnswers > 0 {
            faults.cutAnswers -= 1
            throw URLError(.networkConnectionLost)
        }
        components.scheme = target.scheme; components.host = target.host; components.port = target.port
        guard let mapped = components.url else { throw ProcessClientError.invalidConfiguration }
        var actual = request; actual.url = mapped
        if request.httpMethod == "GET", url.path == "/v1/bootstrap" { bootstraps += 1 }
        if submitted?.path == "/uploads/chunk", holdTransferChunk {
            holdTransferChunk = false
            await withCheckedContinuation {
                transferChunkRelease = $0
                transferChunkHeld = true
                transferChunkArrival?.resume(returning: ()); transferChunkArrival = nil
            }
        }
        if submitted?.path == "/operation-journal/ack", holdJournalAcknowledgments {
            await withCheckedContinuation { journalAcknowledgmentReleases.append($0) }
        }
        let reply = try await network.send(actual)
        if let submitted {
            if reply.status == 200 || reply.status == 202 {
                accepted.append(submitted)
                var remaining: [(String, CheckedContinuation<String, Never>)] = []
                for (value, waiter) in acceptanceWaiters {
                    if submitted.id == value || submitted.path.contains(value) { waiter.resume(returning: submitted.id) }
                    else { remaining.append((value, waiter)) }
                }
                acceptanceWaiters = remaining
            } else if reply.status == 429,
                      let body = try JSONSerialization.jsonObject(with: reply.body) as? [String: Any], body["error"] as? String == "queue_full" {
                rejectedAdmissions += 1
                var remaining: [(Int, CheckedContinuation<Void, Never>)] = []
                for (count, waiter) in rejectionWaiters {
                    if rejectedAdmissions >= count { waiter.resume() } else { remaining.append((count, waiter)) }
                }
                rejectionWaiters = remaining
            }
        }
        if answer, faults.lostReplies > 0 {
            faults.lostReplies -= 1
            throw URLError(.networkConnectionLost)
        }
        if status, faults.lostStatus > 0 {
            faults.lostStatus -= 1
            throw URLError(.timedOut)
        }
        if read, faults.lostLookups > 0 {
            faults.lostLookups -= 1
            throw URLError(.networkConnectionLost)
        }
        if request.httpMethod == "GET", url.path == "/v1/bootstrap" {
            if holdBootstrapRequest {
                await withCheckedContinuation {
                    bootstrapReleases.append($0); bootstrapHeld = true
                    bootstrapArrival?.resume(); bootstrapArrival = nil
                }
            }
        }
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
    func readCounts() -> (bootstraps: Int, results: [String]) { (bootstraps, operationResults) }
}
