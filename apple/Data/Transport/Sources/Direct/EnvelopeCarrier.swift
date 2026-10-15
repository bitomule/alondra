import Foundation

/// What came back for one sealed request.
enum CarrierAnswer: Sendable {
    /// The Mac's sealed answer, already opened with the pinned keys.
    case sealed(Envelope, APIResponse)
    /// A refusal the carrier trusts as the Mac's own, with no sealed answer.
    case plain(APIResponse)
    /// A mutation that may have reached the Mac without an answer.
    case unknown
}

/// How a sealed request reaches one Mac and its sealed answer comes back. `open` accepts only an
/// answer sealed with the pinned keys for this request.
protocol EnvelopeCarrier: Sendable {
    func deliver(_ body: Data, open: @escaping @Sendable (Envelope) throws -> APIResponse, mutation: Bool,
                 timeout: Duration, attempts: Int, lifetime: DirectSessionLifetime) async throws -> CarrierAnswer
}
