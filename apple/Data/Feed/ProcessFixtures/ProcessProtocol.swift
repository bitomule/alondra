import AlondraContract
import Foundation

struct ProcessClientConfiguration: Codable, Sendable {
    let baseURL: String
    let dataEpoch: String
    let actionID: String
    let deviceID: String
    let signingSeed: String
    var optionID: String? = nil
    var relay: ProcessRelayConfiguration? = nil
}
struct ProcessRelayConfiguration: Codable, Sendable {
    let baseURL: String
    let bearer: String
    let macID: String
    let agreementSeed: String
    let macSigningSeed: String
    let macAgreementSeed: String
    var expiredBearer: String? = nil
}
struct ProcessClientCommand: Codable, Sendable {
    let command: String
    var nonce: String? = nil
    var request: ActionRequest? = nil
    var count: Int? = nil
}
struct ProcessHistorySample: Codable, Sendable {
    let operations: Int
    let previousFileBytes: Int
    let previousCiphertextPayloadWrittenBytes: Int
    let previousObservedMicroseconds: Double
    let previousObservedCPUMicroseconds: Double
    let sqliteObservedMicroseconds: Double
    let sqliteObservedCPUMicroseconds: Double
    let sqliteFileBytes: Int
    let sqliteEncryptedBytes: Int
    let sqlitePending: Int
    let sqliteRecent: Int
    let sqliteIntervals: Int
}
struct ProcessClientEvent: Codable, Sendable {
    let event: String
    var pid: Int32? = nil
    var request: ActionRequest? = nil
    var response: ActionResponse? = nil
    var error: String? = nil
    var status: Int? = nil
    var pendingIDs: [String]? = nil
    var pendingPhases: [String]? = nil
    var history: [ProcessHistorySample]? = nil
    var mutativeEnvelopePosts: Int? = nil
    var otherEnvelopePosts: Int? = nil
    var detailKeys: [String]? = nil
    var authorizationRefusals: Int? = nil
    var bootstrapGets: Int? = nil
    var operationResultGets: Int? = nil
    var operationResultIDs: [String]? = nil
    var readAttempts: Int? = nil
    var readErrors: Int? = nil
    var coldCompletionMicroseconds: Double? = nil
}
