import AlondraNotifications
import Foundation
import XCTest

final class PushPreferencesLedgerTests: XCTestCase {
    func testRecreationRetainsSnapshotRevisionAndRevertingPreferencesAdvancesIt() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let file = directory.appendingPathComponent("preferences.json")
        let first = PushPreferencesLedger(file: file)
        let enabled = try await first.reserve(identity: "client-1", binding: "binding", quietStart: "23:00", quietEnd: "08:00")
        let reopened = PushPreferencesLedger(file: file)
        let retry = try await reopened.reserve(identity: "client-1", binding: "binding", quietStart: "23:00", quietEnd: "08:00")
        XCTAssertEqual(enabled, retry)
        let disabled = try await reopened.reserve(identity: "client-1", binding: "binding", quietStart: nil, quietEnd: nil)
        let reenabled = try await first.reserve(identity: "client-1", binding: "binding", quietStart: "23:00", quietEnd: "08:00")
        XCTAssertGreaterThan(disabled, enabled)
        XCTAssertGreaterThan(reenabled, disabled)
        let restored = try await reopened.reserve(identity: "client-1", binding: "binding", quietStart: "23:00", quietEnd: "08:00", newerThan: 99)
        XCTAssertEqual(restored, 100)
        let changedBinding = try await first.reserve(identity: "client-1", binding: "new-binding", quietStart: "23:00", quietEnd: "08:00")
        XCTAssertEqual(changedBinding, 101)
        let anotherDevice = try await first.reserve(identity: "client-2", binding: "other", quietStart: nil, quietEnd: nil)
        XCTAssertEqual(anotherDevice, 1)
    }
}
