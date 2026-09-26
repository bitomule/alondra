import Foundation

public struct RelayReceipt: Sendable {
    public let requestID: String
    public let status: String
    public let revision: UInt64
    public let response: EnvelopeV1?

    public init(requestID: String, status: String, revision: UInt64, response: EnvelopeV1?) {
        self.requestID = requestID
        self.status = status
        self.revision = revision
        self.response = response
    }
}

public protocol RelayRequestTransport: Sendable {
    func submitRequest(_ envelope: EnvelopeV1) async throws -> RelayReceipt
    func requestStatus(requestID: String) async throws -> RelayReceipt
}
