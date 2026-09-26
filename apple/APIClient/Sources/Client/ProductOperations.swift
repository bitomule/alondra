import Foundation
import AlondraContracts

extension AlondraAPIClient {
    public func overview() async throws -> Data { try await send(AlondraEndpoints.overview()) }
    public func task(_ id: String) async throws -> Data { try await send(AlondraEndpoints.task(id)) }
    public func search(_ query: String) async throws -> Data { try await send(AlondraEndpoints.search(query)) }
    public func markRead(_ report: UInt64, operationID: String) async throws {
        _ = try await send(AlondraEndpoints.markRead(report), operationID: operationID)
    }
    public func dismissPullRequest(_ id: UInt64, operationID: String) async throws {
        _ = try await send(AlondraEndpoints.dismissPullRequest(id), operationID: operationID)
    }
    public func answer(_ question: UInt64, text: String, option: String?, attachments: [String], operationID: String) async throws {
        _ = try await send(AlondraEndpoints.answer(question, text: text, option: option,
            attachments: attachments, device: device), operationID: operationID)
    }
    public func createRequest(_ body: NewRequestBody, operationID: String) async throws -> Data {
        try await send(AlondraEndpoints.newRequest(body), operationID: operationID)
    }
}
