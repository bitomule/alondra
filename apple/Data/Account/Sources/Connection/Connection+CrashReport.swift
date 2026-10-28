import AlondraContract
import AlondraFeatures
import AlondraTransport
import Foundation

public extension Connection {
    /// A crash report on this connection's Mac: its daemon builds, shows and sends it; this app adds
    /// its own section. None of these requests is journaled on this device.
    nonisolated func crashReportClient(client: String, measurements: URL?, onThisMac: Bool) -> CrashReportClient {
        let api = api
        let monitor = monitor
        let lang = MachineSettingsFeature.State.deviceLanguage
        @Sendable func post(_ path: [String], _ body: [String: Any]) async throws -> Data {
            let bytes = try JSONSerialization.data(withJSONObject: body)
            return try await api.send(Endpoint<Data>.data(.post, path: path, body: bytes, contentType: "application/json",
                                                         requiresJSONObjectResponse: true),
                                      operationID: "report-" + UUID().uuidString.lowercased())
        }
        return CrashReportClient(
            build: { description, failure, section in
                var body: [String: Any] = ["failure": failure, "lang": lang, "app_section": section]
                if let description { body["description"] = description }
                guard let report = CrashReport(daemonReply: try await post(["reports"], body)) else { throw CrashReportUnreadable() }
                return report
            },
            file: { id, name in
                var text = ""
                var offset: Int? = 0
                while let from = offset {
                    let data = try await api.send(Endpoint<Data>.data(.get, path: ["reports", id, "files", name], query: [("offset", String(from))]))
                    guard let page = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any],
                          let part = page["text"] as? String, page["offset"] as? Int == from else { throw CrashReportUnreadable() }
                    text += part
                    offset = page["next"] as? Int
                    guard offset.map({ $0 > from }) ?? true else { throw CrashReportUnreadable() }
                }
                return text
            },
            send: { id, failure in
                guard let sent = CrashReportSent(daemonReply: try await post(["reports", id, "send"], ["failure": failure, "lang": lang]))
                else { throw CrashReportUnreadable() }
                return sent
            },
            appSection: {
                let connection = await monitor.state
                let since = Date().addingTimeInterval(-72 * 60 * 60)
                return await Task.detached(priority: .utility) {
                    let lines = measurements.flatMap { try? String(contentsOf: $0, encoding: .utf8) }
                        .map { $0.split(separator: "\n").map(String.init) } ?? []
                    let info = Bundle.main.infoDictionary
                    let version = "\(info?["CFBundleShortVersionString"] as? String ?? "0") (\(info?["CFBundleVersion"] as? String ?? "0"))"
                    let system = ProcessInfo.processInfo.operatingSystemVersion
                    let app = AppReportSection.App(client: client, version: version,
                                                   system: "\(system.majorVersion).\(system.minorVersion).\(system.patchVersion)",
                                                   connection: connection)
                    return AppReportSection.text(app: app, entries: AppLogs.recent(since: since), measurements: lines)
                }.value
            },
            problem: { self.connectionProblem(for: $0) },
            onThisMac: onThisMac
        )
    }
}
