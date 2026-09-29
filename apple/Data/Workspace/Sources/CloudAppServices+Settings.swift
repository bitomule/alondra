import AlondraAPIClient
import Foundation

public extension CloudAppServices {
    /// A signed round trip through this Mac's session: a saved pairing is not proof that
    /// access has not been revoked remotely.
    func checkSettingsConnection() async throws {
        try checkActive()
        _ = try await api.domainCapabilities()
        try checkActive()
    }
}
