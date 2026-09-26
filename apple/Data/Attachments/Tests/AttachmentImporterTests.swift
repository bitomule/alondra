import AlondraAttachmentsData
import AlondraAttachmentStorage
import Foundation
import Testing

struct AttachmentImporterTests {
    @Test(arguments: [20 * 1024 * 1024, 20 * 1024 * 1024 + 1])
    func binaryFileLimitKeepsAcceptedBytesAndRefusesOversizedInput(size: Int) throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let url = directory.appendingPathComponent("proof.bin")
        let bytes = Data(repeating: 0x41, count: size)
        try bytes.write(to: url)
        if size == 20 * 1024 * 1024 {
            let draft = try AttachmentImporter.file(url)
            #expect(draft.name == "proof.bin")
            #expect(draft.data == bytes)
        } else {
            #expect(throws: AttachmentImportError.self) { try AttachmentImporter.file(url) }
        }
    }
}
