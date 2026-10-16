@testable import AlondraTransport
import Foundation

extension Transport {
    /// A signed GET whose path names it, `/v1/pairings/{id}`: the fake relays echo the id back.
    func signedRead(_ id: String) async throws -> String {
        let reply = try await pairingRequest(method: "GET", target: "/v1/pairings/\(id)", body: Data())
        switch reply.status {
        case 200: break
        case 401: throw RelayWireError.unauthorized
        default: throw RelayWireError.server(reply.status)
        }
        return try JSONDecoder().decode(SignedReadEcho.self, from: reply.body).request_id
    }
}

private struct SignedReadEcho: Decodable { let request_id: String }
