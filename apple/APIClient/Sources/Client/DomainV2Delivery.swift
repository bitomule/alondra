import AlondraContracts
import Foundation

extension AlondraAPIClient {
    public func domainDeliveryAttempts(after cursor: Int64, epoch: DomainV2.DataEpoch) async throws -> DomainV2.DeliveryAttemptPage {
        guard cursor >= 0 else {
            throw DomainV2.DomainError(code: "invalid_input", message: "Invalid delivery cursor", currentRevision: nil, subject: nil)
        }
        let endpoint = Endpoint<DomainV2.DeliveryAttemptPage>(
            .get, path: ["delivery-attempts"], query: [("after", String(cursor))]
        ) { try DomainV2.decode(DomainV2.DeliveryAttemptPage.self, from: $0) }
        let page = try await send(endpoint)
        guard page.protocolVersion == DomainV2.protocolVersion,
              page.dataEpoch == epoch,
              page.revision <= DomainV2.maximumRevision,
              page.nextAfter >= cursor, page.attempts.count <= 100,
              page.attempts.allSatisfy({ $0.attemptId > cursor && $0.version > 0 }),
              zip(page.attempts, page.attempts.dropFirst()).allSatisfy({ $0.0.attemptId < $0.1.attemptId }),
              (page.attempts.last?.attemptId == page.nextAfter || page.attempts.isEmpty && page.nextAfter == cursor),
              !page.hasMore || !page.attempts.isEmpty
        else {
            throw DomainV2.DomainError(code: "incompatible_protocol", message: "Invalid delivery evidence", currentRevision: nil, subject: nil)
        }
        return page
    }

    public func resolveDomainDeliveryAttempt(
        _ attemptId: Int64, epoch: DomainV2.DataEpoch,
        expectedVersion: UInt64, decision: DomainV2.DeliveryDecision,
        operationID: String
    ) async throws -> DomainV2.DeliveryResolution {
        guard attemptId > 0, !epoch.rawValue.isEmpty, expectedVersion > 0,
              expectedVersion < DomainV2.maximumRevision, !operationID.isEmpty else {
            throw DomainV2.DomainError(code: "invalid_input", message: "Invalid delivery resolution", currentRevision: nil, subject: nil)
        }
        let request = DomainV2.ResolveDeliveryAttempt(dataEpoch: epoch,
            expectedVersion: expectedVersion, decision: decision)
        let endpoint = Endpoint<DomainV2.DeliveryResolution>(
            .post, path: ["delivery-attempts", String(attemptId), "resolve"],
            body: try DomainV2.encode(request), contentType: "application/json", requiresJSONObjectResponse: true
        ) { try DomainV2.decode(DomainV2.DeliveryResolution.self, from: $0) }
        let resolution = try await send(endpoint, operationID: operationID)
        guard resolution.attemptId == attemptId, resolution.version == expectedVersion + 1,
              resolution.state == (decision == .retry ? "retryable" : "discarded") else {
            throw DomainV2.DomainError(code: "incompatible_protocol", message: "Invalid delivery resolution receipt", currentRevision: nil, subject: nil)
        }
        return resolution
    }
}
