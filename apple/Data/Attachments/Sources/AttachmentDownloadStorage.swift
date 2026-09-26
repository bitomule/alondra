import AlondraDomain
import Foundation

/// Files belong to a service session. Presentation leases survive logout until the
/// system preview/export dismisses; releasing a lease removes its private directory.
public actor AttachmentDownloadStorage {
    private let root = FileManager.default.temporaryDirectory.appendingPathComponent("AlondraDownloads-" + UUID().uuidString, isDirectory: true)
    private var leases: Set<URL> = []
    private var active = true
    public init() {}
    public func store(_ data: Data, name: String) throws -> DownloadedAttachment {
        guard active else { throw CancellationError() }
        guard data.count <= 20 * 1024 * 1024 else { throw AttachmentImportError.invalidSize }
        let folder = root.appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        let component = URL(fileURLWithPath: name).lastPathComponent
        let clean = component.unicodeScalars.filter { !CharacterSet.controlCharacters.contains($0) }.map(String.init).joined()
        let filename = clean.isEmpty || clean == "." || clean == ".." ? "attachment" : String(clean.prefix(180))
        let file = folder.appendingPathComponent(filename)
        do {
            try data.write(to: file, options: .atomic)
            try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: file.path)
            leases.insert(file)
            return DownloadedAttachment(url: file, name: filename)
        } catch { try? FileManager.default.removeItem(at: folder); throw error }
    }
    public func release(_ file: DownloadedAttachment) {
        guard leases.remove(file.url) != nil else { return }
        try? FileManager.default.removeItem(at: file.url.deletingLastPathComponent())
        if leases.isEmpty { try? FileManager.default.removeItem(at: root) }
    }
    public func deactivate() {
        active = false
        // Active system presentations hold leases; their dismissal calls release.
        if leases.isEmpty { try? FileManager.default.removeItem(at: root) }
    }
}
