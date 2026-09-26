import AlondraAuth
import Foundation
import Testing

struct RefreshCredentialTests {
    @Test func legacyKeychainTokenMigratesWithoutChangingItsBytes() throws {
        let legacy = Data("legacy.refresh-token".utf8)
        let record = try RefreshCredential.decodePersisted(legacy)
        #expect(record.token == "legacy.refresh-token")
        #expect(record.status == .active)
        let rewritten = try record.persistedData()
        #expect(rewritten != legacy)
        #expect(try RefreshCredential.decodePersisted(rewritten) == record)
    }

    @Test(arguments: ["{broken", "{\"version\":2,\"token\":\"x\",\"status\":\"active\"}", "{\"version\":1,\"token\":\"\",\"status\":\"active\"}"])
    func corruptOrFutureRecordIsNeverTreatedAsLegacyToken(_ value: String) {
        #expect(throws: (any Error).self) { try RefreshCredential.decodePersisted(Data(value.utf8)) }
    }
}
