import AlondraAttachmentsData
import Foundation
import XCTest

final class AttachmentDownloadStorageTests: XCTestCase {
    func testExportLeaseSurvivesLogoutButReleaseRemovesPrivateFile() async throws {
        let storage = AttachmentDownloadStorage()
        let data = Data("document".utf8)
        let file = try await storage.store(data, name: "../../outside.txt")
        XCTAssertEqual(file.name, "outside.txt")
        XCTAssertEqual(try Data(contentsOf: file.url), data)
        let attributes = try FileManager.default.attributesOfItem(atPath: file.url.path)
        XCTAssertEqual((attributes[.posixPermissions] as? NSNumber)?.intValue, 0o600)
        await storage.deactivate()
        // Logout must not invalidate a file that the system exporter still owns.
        XCTAssertTrue(FileManager.default.fileExists(atPath: file.url.path))
        do {
            _ = try await storage.store(data, name: "late.txt")
            XCTFail("An inactive session must reject late downloads")
        } catch is CancellationError { }
        await storage.release(file)
        XCTAssertFalse(FileManager.default.fileExists(atPath: file.url.path))
        XCTAssertFalse(FileManager.default.fileExists(atPath: file.url.deletingLastPathComponent().path))
    }
}
