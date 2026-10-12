import Foundation
import AlondraContract

/// Product API over the durable service operation transport. HTTP authentication,
/// envelopes and journaling belong to the capabilities composed beneath this value.
public struct AlondraAPIClient: Sendable {
    public let base: URL
    public let device: String
    let operations: any OperationAPI

    public init(serviceURL: URL, deviceID: String, operations: any OperationAPI) {
        self.base = serviceURL
        self.device = deviceID
        self.operations = operations
    }

    public func actionJournal(dataEpoch: String) async throws -> ActionJournalState? {
        do {
            let state = try await send(Endpoint<ActionJournalState>(.get, path: ["operation-journal"],
                query: [("data_epoch", dataEpoch)]) { try JSONDecoder().decode(ActionJournalState.self, from: $0) })
            guard state.protocolVersion == 1, state.dataEpoch == dataEpoch, (1...ActionOperationID.maximumCounter).contains(state.generation),
                  state.maxPending == 128, state.maxRanges == 129,
                  state.maxSequence == ActionOperationID.maximumCounter else { throw OperationError.outcomeUnknown }
            try await operations.retireActions(ActionJournalAcknowledgment(dataEpoch: dataEpoch,
                generation: state.generation, ranges: []))
            return state
        } catch APIError.unexpected(status: 404) { return nil }
        catch APIError.server(status: 404, kind: _, detail: _, reasons: _) { return nil }
    }

    public func acknowledgeActions(_ acknowledgment: ActionJournalAcknowledgment) async throws {
        let body = try JSONEncoder().encode(acknowledgment)
        let state = try await send(Endpoint<ActionJournalState>(.post, path: ["operation-journal", "ack"], body: body) {
            try JSONDecoder().decode(ActionJournalState.self, from: $0)
        }, operationID: "control-" + UUID().uuidString)
        guard state.acknowledged == true, state.generation == acknowledgment.generation,
              state.dataEpoch == acknowledgment.dataEpoch,
              state.protocolVersion == 1, state.maxPending == 128, state.maxRanges == 129,
              state.maxSequence == ActionOperationID.maximumCounter,
              let ranges = state.ranges, ranges.count <= 129,
              ranges.allSatisfy({ $0.first > 0 && $0.first <= $0.last && $0.last <= ActionOperationID.maximumCounter }),
              acknowledgment.ranges.allSatisfy({ expected in ranges.contains { $0.first <= expected.first && $0.last >= expected.last } })
        else { throw OperationError.outcomeUnknown }
        try await operations.retireActions(acknowledgment)
    }

    public func recover<Response>(_ endpoint: Endpoint<Response>, operationID: String) async throws -> Response? {
        guard endpoint.method != .get else { throw OperationError.invalidEndpoint }
        let request = try APIRequest(endpoint: endpoint, operationID: operationID)
        try Task.checkCancellation()
        switch try await operations.recover(request) {
        case .unknown: return nil
        case let .completed(status, body):
            guard (200..<300).contains(status) else { throw APIError.from(status: status, body: body) }
            if endpoint.requiresJSONObjectResponse,
               (try? JSONSerialization.jsonObject(with: body)) as? [String: Any] == nil {
                throw OperationError.outcomeUnknown
            }
            return try endpoint.decode(body)
        }
    }

    public func send<Response>(_ endpoint: Endpoint<Response>, operationID: String? = nil) async throws -> Response {
        guard endpoint.method == .get || operationID?.isEmpty == false else {
            throw OperationError.operationIdentityRequired
        }
        let request = try APIRequest(endpoint: endpoint, operationID: operationID ?? UUID().uuidString)
        try Task.checkCancellation()
        switch try await operations.perform(request) {
        case .unknown: throw OperationError.outcomeUnknown
        case let .completed(status, body):
            guard (200..<300).contains(status) else {
                throw APIError.from(status: status, body: body)
            }
            if endpoint.requiresJSONObjectResponse, (try? JSONSerialization.jsonObject(with: body)) as? [String: Any] == nil {
                throw OperationError.outcomeUnknown
            }
            return try endpoint.decode(body)
        }
    }
}
