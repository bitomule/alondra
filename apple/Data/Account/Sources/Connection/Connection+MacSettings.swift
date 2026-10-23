import AlondraFeatures
import AlondraTransport
import Foundation

extension Connection {
    private struct Change: Encodable {
        let projectsRoot: String
        let lang: String

        private enum CodingKeys: String, CodingKey {
            case projectsRoot = "projects_root"
            case lang
        }
    }

    public func macSettings() async throws -> MacSettings {
        try checkActive()
        return try await api.send(.json(.get, path: ["settings"]))
    }

    public func setProjectsRoot(_ path: String) async throws -> MacSettings {
        try checkActive()
        let body = try JSONEncoder().encode(Change(projectsRoot: path, lang: MachineSettingsFeature.State.deviceLanguage))
        return try await api.send(.json(.put, path: ["settings"], body: body), operationID: UUID().uuidString.lowercased())
    }

    public func folders(at path: String?) async throws -> FolderListing {
        try checkActive()
        return try await api.send(.json(.get, path: ["settings", "folders"], query: path.map { [("path", $0)] } ?? []))
    }

    public nonisolated func macSettingsClient(pickLocalFolder: (@Sendable () async -> String?)? = nil) -> MacSettingsClient {
        MacSettingsClient(load: { try await self.macSettings() },
                          save: { try await self.setProjectsRoot($0) },
                          folders: { try await self.folders(at: $0) },
                          pickLocalFolder: pickLocalFolder,
                          problem: { self.connectionProblem(for: $0) },
                          refresh: .seconds(10))
    }
}
