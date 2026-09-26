import Foundation

public extension CloudAppServices {
    func checkSettingsConnection() async throws {
        try checkActive()
        _ = try await api.overview()
        try checkActive()
    }
}
