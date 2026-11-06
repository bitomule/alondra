@testable import AlondraTransport
import CryptoKit
import Foundation
import Testing

private struct DirectoryRelay: HTTPTransport {
    var directoryStatus = 200
    var devices = #"{"macs":[{"mac_id":"mac-studio","status":"active","clients":[{"device_id":"phone-old","status":"revoked"},{"device_id":"phone-current","status":"active"}]}]}"#
    var epochChanged = false

    func send(_ request: URLRequest) async throws -> HTTPReply {
        let path = try #require(request.url?.path)
        if path == "/v1/bootstrap" {
            return HTTPReply(status: 200, body: Data(#"{"protocol":1,"epoch":"BwcHBwcHBwcHBwcHBwcHBw"}"#.utf8))
        }
        #expect(request.value(forHTTPHeaderField: "Authorization") == "Bearer account-token")
        switch path {
        case "/v1/account":
            #expect(request.value(forHTTPHeaderField: "x-device-signature") == nil)
            return HTTPReply(status: directoryStatus, body: Data(#"{"account":{"display_name":"Alice Example"}}"#.utf8))
        case "/v1/account/devices":
            #expect(request.value(forHTTPHeaderField: "x-device-signature") == nil,
                    "a removed device can still ask its account what happened")
            return HTTPReply(status: directoryStatus, body: Data(devices.utf8))
        case "/v1/push/register", "/v1/iroh-node", "/v1/macs":
            #expect(request.value(forHTTPHeaderField: "x-device-signature") != nil)
            return HTTPReply(status: 401, body: Data((epochChanged
                ? #"{"error":"relay_epoch_changed"}"# : #"{"error":"unauthorized"}"#).utf8))
        default:
            Issue.record("Unexpected request: \(path)")
            return HTTPReply(status: 500, body: Data())
        }
    }
}

struct RelayAccountDirectoryTests {
    private let base = URL(string: "https://relay.example")!

    @Test func accountNameComesFromTheAccountNotTheMacAddress() async throws {
        let directory = try RelayAccountDirectory(base: base, bearer: { "account-token" }, transport: DirectoryRelay())
        #expect(try await directory.accountName() == "Alice Example")
    }

    @Test func removalMatchesBothMacAndDevice() async throws {
        let directory = try RelayAccountDirectory(base: base, bearer: { "account-token" }, transport: DirectoryRelay())
        #expect(try await directory.isRemoved(macID: "mac-studio", deviceID: "phone-old"))
        #expect(try await !directory.isRemoved(macID: "mac-studio", deviceID: "phone-current"))
        #expect(try await !directory.isRemoved(macID: "mac-other", deviceID: "phone-old"))
        #expect(try await !directory.isRemoved(macID: "mac-studio", deviceID: "phone-unknown"))
    }

    @Test(arguments: ["/v1/push/register", "/v1/iroh-node", "/v1/macs"])
    func aSignedRejectionIsExplainedByTheAuthenticatedAccount(path: String) async throws {
        let account = try RelayAccount(relay: base, macID: "mac-studio", deviceID: "phone-old",
            signing: P256.Signing.PrivateKey(), bearer: { "account-token" }, transport: DirectoryRelay())
        await #expect(throws: AccountDeviceError.removedFromWeb) {
            switch path {
            case "/v1/push/register":
                try await account.registerServicePush(token: String(repeating: "ab", count: 32), environment: "sandbox",
                    binding: Base64URL.encode(Data(repeating: 1, count: 32)))
            case "/v1/iroh-node":
                try await account.bindIrohNode(IrohIdentity(seed: Data(repeating: 2, count: 32)))
            default:
                _ = try await account.macIrohEndpoint()
            }
        }
    }

    @Test(arguments: [401, 403, 503])
    func anUnavailableAccountNeverConfirmsRemoval(status: Int) async throws {
        let account = try RelayAccount(relay: base, macID: "mac-studio", deviceID: "phone-old",
            signing: P256.Signing.PrivateKey(), bearer: { "account-token" },
            transport: DirectoryRelay(directoryStatus: status))
        #expect(await !account.removedFromAccount())
        await #expect(throws: RelayWireError.unauthorized) { _ = try await account.macIrohEndpoint() }
    }

    @Test func aDifferentAccountsDirectoryAndAnEpochChangeNeverConfirmRemoval() async throws {
        let account = try RelayAccount(relay: base, macID: "mac-studio", deviceID: "phone-old",
            signing: P256.Signing.PrivateKey(), bearer: { "account-token" },
            transport: DirectoryRelay(devices: #"{"macs":[]}"#))
        await #expect(throws: RelayWireError.unauthorized) { _ = try await account.macIrohEndpoint() }
        let restarted = try RelayAccount(relay: base, macID: "mac-studio", deviceID: "phone-old",
            signing: P256.Signing.PrivateKey(), bearer: { "account-token" }, transport: DirectoryRelay(epochChanged: true))
        await #expect(throws: RelayWireError.unauthorized) { _ = try await restarted.macIrohEndpoint() }
    }
}
