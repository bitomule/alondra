import AlondraContracts
import Foundation

extension AlondraAPIClient {
    public func domainSchedules() async throws -> DomainV2.ScheduleList {
        let endpoint = Endpoint<DomainV2.ScheduleList>(.get, path: ["v2", "domain", "schedules"]) {
            try DomainV2.decode(DomainV2.ScheduleList.self, from: $0)
        }
        let result = try await send(endpoint)
        try Self.validateScheduleRead(result.protocolVersion, revision: result.revision)
        return result
    }

    public func domainSchedule(_ id: DomainV2.ScheduleId) async throws -> DomainV2.ScheduleDetail {
        let endpoint = Endpoint<DomainV2.ScheduleDetail>(.get,
            path: ["v2", "domain", "schedules", id.rawValue]) {
                try DomainV2.decode(DomainV2.ScheduleDetail.self, from: $0)
            }
        let result = try await send(endpoint)
        try Self.validateScheduleRead(result.protocolVersion, revision: result.revision)
        guard result.schedule.id == id else {
            throw Self.invalidScheduleResponse("Schedule detail ID changed")
        }
        return result
    }

    public func createDomainSchedule(_ draft: DomainV2.ScheduleDraft,
                                     operationID: String) async throws -> DomainV2.ScheduleWriteReceipt {
        guard draft.revision == 0 else {
            throw Self.invalidScheduleInput("New Schedule revision must be zero")
        }
        let endpoint = Endpoint<DomainV2.ScheduleWriteReceipt>(.post,
            path: ["v2", "domain", "schedules"], body: try DomainV2.encode(draft),
            contentType: "application/json", requiresJSONObjectResponse: true) {
                try DomainV2.decode(DomainV2.ScheduleWriteReceipt.self, from: $0)
            }
        let result = try await send(endpoint, operationID: operationID)
        guard result.revision == 1 else {
            throw Self.invalidScheduleResponse("Invalid Schedule create receipt")
        }
        return result
    }

    public func updateDomainSchedule(_ id: DomainV2.ScheduleId, draft: DomainV2.ScheduleDraft,
                                     operationID: String) async throws -> DomainV2.ScheduleWriteReceipt {
        guard draft.revision >= 1, UInt64(draft.revision) < DomainV2.maximumRevision else {
            throw Self.invalidScheduleInput("Invalid Schedule update revision")
        }
        let endpoint = Endpoint<DomainV2.ScheduleWriteReceipt>(.put,
            path: ["v2", "domain", "schedules", id.rawValue], body: try DomainV2.encode(draft),
            contentType: "application/json", requiresJSONObjectResponse: true) {
                try DomainV2.decode(DomainV2.ScheduleWriteReceipt.self, from: $0)
            }
        let result = try await send(endpoint, operationID: operationID)
        guard result.id == id, result.revision == draft.revision + 1 else {
            throw Self.invalidScheduleResponse("Invalid Schedule update receipt")
        }
        return result
    }

    public func deleteDomainSchedule(_ id: DomainV2.ScheduleId,
                                     operationID: String) async throws {
        let endpoint = Endpoint<DomainV2.ScheduleDeleteReceipt>(.delete,
            path: ["v2", "domain", "schedules", id.rawValue],
            requiresJSONObjectResponse: true) {
                try DomainV2.decode(DomainV2.ScheduleDeleteReceipt.self, from: $0)
            }
        let result = try await send(endpoint, operationID: operationID)
        guard result.ok else { throw Self.invalidScheduleResponse("Invalid Schedule delete receipt") }
    }

    /// P3 currently returns `launch_unavailable` without mutation. A later success
    /// body has no agreed DTO yet, so callers must not infer Claude execution.
    public func requestDomainScheduleRun(_ id: DomainV2.ScheduleId,
                                         operationID: String) async throws -> Data {
        let endpoint = Endpoint<Data>.data(.post,
            path: ["v2", "domain", "schedules", id.rawValue, "run"],
            requiresJSONObjectResponse: true)
        return try await send(endpoint, operationID: operationID)
    }

    private static func validateScheduleRead(_ version: UInt32, revision: DomainV2.Revision) throws {
        guard version == DomainV2.protocolVersion, revision <= DomainV2.maximumRevision else {
            throw invalidScheduleResponse("Unsupported Schedule snapshot")
        }
    }

    private static func invalidScheduleInput(_ message: String) -> DomainV2.DomainError {
        DomainV2.DomainError(code: "invalid_input", message: message, currentRevision: nil, subject: nil)
    }

    private static func invalidScheduleResponse(_ message: String) -> DomainV2.DomainError {
        DomainV2.DomainError(code: "incompatible_protocol", message: message, currentRevision: nil, subject: nil)
    }
}
