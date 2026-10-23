import AlondraDesign
import Foundation
import Testing

@Suite struct StoredImageTests {
    static let digest = String(repeating: "a", count: 64)

    @Test func anImageAlondraCopiedIsReadAsAnAttachmentOfTheDaemon() throws {
        let blocks = MarkdownParser.blocks(from: "![Main screen](/files?path=files/images/\(Self.digest).png)")
        guard case .image(let url, let alt) = try #require(blocks.first).kind else {
            Issue.record("not an image block: \(blocks)")
            return
        }
        #expect(alt == "Main screen")
        let attachment = try #require(StoredImage.attachment(url))
        #expect(attachment.fetch == "files/images/\(Self.digest).png")
        #expect(attachment.digest == Self.digest)
        #expect(attachment.kind == .image)
    }

    @Test(arguments: [
        "/files?path=/etc/hosts", "/files?path=files/images/../x.png", "/files?path=files/uploads/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.png",
        "/files?path=files/images/abc.png", "/files?path=files/images/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.exe", "docs/shot.png", "https://a.dev/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.png",
    ])
    func anythingElseIsNot(_ written: String) throws {
        #expect(StoredImage.attachment(try #require(URL(string: written))) == nil)
    }
}
