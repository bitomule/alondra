@testable import AlondraFeed
import AlondraContract
import AlondraTestSupport
import CryptoKit
import Foundation
import Testing

@Suite(.temporaryFiles) struct BoundedActionIntentTests {
    private let material = Data(repeating: 17, count: 32)
    private let scope = ActionIntentScope(service: "mac", device: "phone", credentialRevision: "approval")
    private func draft(_ ordinal: Int, epoch: String = "epoch") -> ActionRequest {
        let nonce = String(format: "00000000-0000-4000-8000-%012x", ordinal)
        return ActionRequest(commandId: nonce, dataEpoch: epoch, client: .ios, lang: "en",
            actionId: "answer:\(ordinal)", input: ActionInput(text: "Private intent"), resolvesCard: true)
    }
    private func store(_ directory: URL) async throws -> ViewActionIntentStore {
        try await ViewActionIntentStore(directory: directory, keyMaterial: material, scope: scope)
    }
    private func response() throws -> ActionResponse {
        try ContractCodec.decode(ActionResponse.self, from: Data(#"{"protocol":1,"outcome":"applied","stamp":"Saved","revision":2}"#.utf8))
    }
    private func claimed(_ reservation: ViewActionIntentStore.Reservation) throws -> ViewActionIntentStore.Record {
        guard case .created(let record) = reservation else { Issue.record("a prepared ticket must be claimed exactly once"); throw ViewActionIntentError.invalidRecord }
        return record
    }

    @Test func independentStoresClaimOneDurableTicketOnlyOnce() async throws {
        let directory = try TestFiles.directory()
        let first = try await store(directory)
        let second = try await store(directory)
        let prepared = try await first.prepareNew(draft(1), generation: 7)
        let expected = "op_" + SHA256.hash(data: Data("epoch".utf8)).prefix(16).map { String(format: "%02x", $0) }.joined()
            + "_7_1_00000000-0000-4000-8000-000000000001"
        #expect(prepared.request.commandId == expected)
        #expect(prepared.phase == .prepared)
        #expect(prepared.restoredRequest.resolvesItsCard)
        let claimed = try claimed(await first.reserve(prepared.restoredRequest, body: prepared.body))
        #expect(claimed.phase == .dispatched)
        let duplicate = try await second.reserve(prepared.restoredRequest, body: prepared.body)
        guard case .pending(let pending) = duplicate else { Issue.record("another store must recover instead of dispatching"); return }
        #expect(pending.request.commandId == expected)
        let reopened = try await store(directory)
        #expect(reopened.restored.map(\.request.commandId) == [expected])
        #expect(reopened.restored.first?.phase == .dispatched)
    }

    @Test func preparationAdoptsTheOriginalPendingTicketBeforePresentation() async throws {
        let directory = try TestFiles.directory()
        let first = try await store(directory)
        let original = try await first.prepareNew(draft(1), generation: 7)
        let replacement = ActionRequest(commandId: draft(2).commandId, dataEpoch: "epoch", client: .ios, lang: "es",
            actionId: original.request.actionId, input: original.request.input, resolvesCard: false)
        let reopened = try await store(directory)
        let adopted = try await reopened.prepareNew(replacement, generation: 7)
        #expect(adopted.request.commandId == original.request.commandId)
        #expect(adopted.body == original.body)
        #expect(adopted.restoredRequest.resolvesItsCard)
        #expect(adopted.phase == .prepared)
        #expect(try await reopened.statistics().pending == 1)
    }

    @Test func anUnknownHoleDoesNotRetainEveryLaterCompletedUUID() async throws {
        let directory = try TestFiles.directory()
        let store = try await store(directory)
        let hole = try await store.prepareNew(draft(1), generation: 7)
        _ = try claimed(await store.reserve(hole.restoredRequest, body: hole.body))
        var firstCompleted: ViewActionIntentStore.Record?
        var warmBytes = 0
        for ordinal in 2...302 {
            let prepared = try await store.prepareNew(draft(ordinal), generation: 7)
            let dispatched = try claimed(await store.reserve(prepared.restoredRequest, body: prepared.body))
            if ordinal == 2 { firstCompleted = dispatched }
            try await store.complete(dispatched, response: response())
            let ack = try await store.acknowledgmentRanges()
            try await store.markAcknowledged(.init(dataEpoch: ack.dataEpoch, generation: ack.generation,
                ranges: ack.ranges.map { .init(first: $0.first, last: $0.last) }))
            if ordinal == 102 { warmBytes = try await store.statistics().databaseBytes }
        }
        let stats = try await store.statistics()
        print("action-intents plateau: admitted=302 pending=\(stats.pending) recent=\(stats.recent) legacy=\(stats.legacy) intervals=\(stats.retiredIntervals) encrypted_bytes=\(stats.encryptedBytes) database_bytes=\(stats.databaseBytes) warm_database_bytes=\(warmBytes)")
        #expect(stats.pending == 1)
        #expect(stats.recent == 64)
        #expect(stats.legacy == 0)
        #expect(stats.retiredIntervals == 1)
        #expect(stats.encryptedBytes < 128 * 1024)
        #expect(stats.databaseBytes <= warmBytes + 4 * 4096, "history must reuse pages rather than grow by one UUID per completion")
        let ack = try await store.acknowledgmentRanges()
        #expect(ack.generation == 7)
        #expect(ack.dataEpoch == "epoch")
        #expect(ack.ranges == [.init(first: 2, last: 302)])
        let old = try #require(firstCompleted)
        guard case .completed(nil) = try await store.reserve(old.restoredRequest, body: old.body)
        else { Issue.record("compacted tickets must remain retired without their UUID row"); return }
        let reopened = try await self.store(directory)
        #expect(reopened.restored.map(\.request.commandId) == [hole.request.commandId])
        try await reopened.complete(hole, response: response())
        #expect(try await reopened.acknowledgmentRanges().ranges == [.init(first: 1, last: 302)])
    }

    @Test func aVerifiedGenerationFenceRetiresOldPreparedAndDispatchedTickets() async throws {
        let directory = try TestFiles.directory()
        let store = try await store(directory)
        let prepared = try await store.prepareNew(draft(1), generation: 7)
        let dispatched = try await store.prepareNew(draft(2), generation: 7)
        _ = try claimed(await store.reserve(dispatched.restoredRequest, body: dispatched.body))
        let current = try await store.prepareNew(draft(3, epoch: "new-epoch"), generation: 8)
        #expect(current.request.commandId.contains("_8_1_"))
        #expect(try await store.statistics().pending == 1)
        for old in [prepared, dispatched] {
            guard case .completed(nil) = try await store.reserve(old.restoredRequest, body: old.body)
            else { Issue.record("old generations never authorize dispatch"); continue }
        }
        await #expect(throws: ViewActionIntentError.invalidRecord) { try await store.prepareNew(draft(4), generation: 7) }
        await #expect(throws: ViewActionIntentError.invalidRecord) { try await store.prepareNew(draft(4), generation: 8) }
        let reopened = try await self.store(directory)
        #expect(reopened.restored.map(\.request.commandId) == [current.request.commandId])
    }

    @Test func unissuedOrRelabelledTicketsAndRawUUIDsCannotAuthorizePOST() async throws {
        let directory = try TestFiles.directory()
        let store = try await store(directory)
        await #expect(throws: ViewActionIntentError.invalidRecord) { try await store.reserve(draft(1), body: ContractCodec.encode(draft(1))) }
        let prepared = try await store.prepareNew(draft(1), generation: 7)
        for id in [prepared.request.commandId.replacingOccurrences(of: "_7_1_", with: "_7_2_"),
                   prepared.request.commandId.replacingOccurrences(of: "_7_1_", with: "_07_1_")] {
            let forged = ActionRequest(commandId: id, dataEpoch: "epoch", client: .ios, lang: "en", actionId: "answer:1", input: draft(1).input)
            await #expect(throws: ViewActionIntentError.invalidRecord) { try await store.reserve(forged, body: ContractCodec.encode(forged)) }
        }
        let relabelled = ActionRequest(commandId: prepared.request.commandId, dataEpoch: "other", client: .ios, lang: "en", actionId: "answer:1", input: draft(1).input)
        await #expect(throws: ViewActionIntentError.invalidRecord) { try await store.reserve(relabelled, body: ContractCodec.encode(relabelled)) }
        #expect(try await store.statistics().pending == 1)
    }

    @Test func legacyMigrationPreservesOriginalBytesAndProofsWithoutDispatch() async throws {
        let directory = try TestFiles.directory()
        let files = try LegacyIntentFiles(directory: directory, keyMaterial: material, scope: scope, completedLimit: ViewActionIntentStore.maxCompleted)
        let pending = ViewActionIntentStore.Record(request: draft(1), body: try ContractCodec.encode(draft(1)))
        let completed = ViewActionIntentStore.Completed(commandID: draft(2).commandId, bodyDigest: Data(SHA256.hash(data: try ContractCodec.encode(draft(2)))), dataEpoch: "epoch", response: try response())
        let encoder = JSONEncoder(); encoder.outputFormatting = [.sortedKeys]
        let binding = try encoder.encode(scope)
        let fixtureKey = HKDF<SHA256>.deriveKey(inputKeyMaterial: SymmetricKey(data: material), salt: binding,
            info: Data("alondra-action-intents-v1".utf8), outputByteCount: 32)
        var snapshot = try #require(JSONSerialization.jsonObject(with: encoder.encode(ViewActionIntentStore.Snapshot(pending: [pending], completed: [completed]))) as? [String: Any])
        var records = try #require(snapshot["pending"] as? [[String: Any]])
        records[0].removeValue(forKey: "phase")
        snapshot["pending"] = records
        let oldFormat = try JSONSerialization.data(withJSONObject: snapshot, options: [.sortedKeys])
        let ciphertext = try #require(AES.GCM.seal(oldFormat, using: fixtureKey, authenticating: binding).combined)
        try ciphertext.write(to: files.file)
        let markerName = SHA256.hash(data: binding).map { String(format: "%02x", $0) }.joined() + ".initialized"
        try Data(SHA256.hash(data: binding)).write(to: directory.appending(path: markerName))
        let migrated = try await store(directory)
        #expect(migrated.restored.map(\.request.commandId) == [draft(1).commandId])
        #expect(migrated.restored.first?.body == pending.body)
        #expect(migrated.restored.first?.phase == .dispatched)
        guard case .pending = try await migrated.reserve(draft(1), body: pending.body) else { Issue.record("migration cannot replay a raw UUID"); return }
        guard case .completed(let result) = try await migrated.reserve(draft(2), body: ContractCodec.encode(draft(2))) else { Issue.record("migration must preserve completions"); return }
        #expect(result?.stamp == "Saved")
        #expect(!FileManager.default.fileExists(atPath: files.file.path))
        let reopened = try await store(directory)
        #expect(try await reopened.statistics().legacy == 1)
        #expect(reopened.restored.first?.body == pending.body)
        await #expect(throws: ViewActionIntentError.invalidRecord) { try await reopened.reserve(draft(3), body: ContractCodec.encode(draft(3))) }
    }

    @Test func aFullPendingWindowIsBoundedButCompletionMakesRoomAgain() async throws {
        let directory = try TestFiles.directory()
        let store = try await store(directory)
        var records: [ViewActionIntentStore.Record] = []
        for ordinal in 1...128 { records.append(try await store.prepareNew(draft(ordinal), generation: 7)) }
        await #expect(throws: ViewActionIntentError.capacityExceeded) { try await store.prepareNew(draft(129), generation: 7) }
        let oldest = try #require(records.first)
        _ = try claimed(await store.reserve(oldest.restoredRequest, body: oldest.body))
        try await store.complete(oldest, response: response())
        try await store.markAcknowledged(.init(dataEpoch: "epoch", generation: 7, ranges: [.init(first: 1, last: 1)]))
        let new = try await store.prepareNew(draft(129), generation: 7)
        #expect(new.request.commandId.contains("_7_129_"))
        #expect(try await store.statistics().pending == 128)
    }
    @Test func unacknowledgedCompletionsKeepTheRemoteWindowReservedAcrossRelaunch() async throws {
        let directory = try TestFiles.directory()
        let first = try await store(directory)
        for ordinal in 1...128 {
            let prepared = try await first.prepareNew(draft(ordinal), generation: 7)
            let dispatched = try claimed(await first.reserve(prepared.restoredRequest, body: prepared.body))
            try await first.complete(dispatched, response: response())
        }
        #expect(try await first.statistics().pending == 0)
        let reopened = try await store(directory)
        await #expect(throws: ViewActionIntentError.capacityExceeded) { try await reopened.prepareNew(draft(129), generation: 7) }
        #expect(try await reopened.statistics().pending == 0, "refused preparation must not create a stranded ticket")
        await #expect(throws: ViewActionIntentError.invalidRecord) {
            try await reopened.markAcknowledged(.init(dataEpoch: "epoch", generation: 7, ranges: [.init(first: 1, last: 129)]))
        }
        await #expect(throws: ViewActionIntentError.capacityExceeded) { try await first.prepareNew(draft(129), generation: 7) }
        try await reopened.markAcknowledged(.init(dataEpoch: "epoch", generation: 7, ranges: [.init(first: 1, last: 128)]))
        let afterAck = try await store(directory)
        let candidate = try await afterAck.prepareNew(draft(129), generation: 7)
        #expect(candidate.request.commandId.contains("_7_129_"), "failed reservations must not consume a sequence")
        let dispatched = try claimed(await first.reserve(candidate.restoredRequest, body: candidate.body))
        #expect(dispatched.request.commandId == candidate.request.commandId)
        guard case .pending = try await reopened.reserve(candidate.restoredRequest, body: candidate.body)
        else { Issue.record("another process must not redispatch the admitted candidate"); return }
    }

}
