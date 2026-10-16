@testable import AlondraTransport
import CryptoKit
import Foundation
import Testing

/// The relay's account routes as a Mac paired with an account uses them: its iroh relay and the
/// owner's Macs with the iroh endpoint each one bound.
private actor AccountRelay: HTTPTransport {
    private(set) var paths: [String] = []
    private let macs: String

    init(macs: String) { self.macs = macs }

    func send(_ request: URLRequest) async throws -> HTTPReply {
        let path = try #require(request.url?.path)
        paths.append(path)
        switch path {
        case "/v1/bootstrap":
            return HTTPReply(status: 200, body: Data(#"{"protocol":1,"epoch":"BwcHBwcHBwcHBwcHBwcHBw","iroh_relay":"https://iroh.example.com"}"#.utf8))
        case "/v1/macs":
            return HTTPReply(status: 200, body: Data(macs.utf8))
        default:
            return HTTPReply(status: 404, body: Data())
        }
    }
}

@Suite struct RelayAccountTests {
    private let bound = IrohIdentity(seed: Data(repeating: 3, count: 32)).endpointIDHex
    private let known = IrohIdentity(seed: Data(repeating: 5, count: 32)).endpointIDHex

    private func account(_ relay: AccountRelay) throws -> RelayAccount {
        try RelayAccount(relay: URL(string: "https://relay.example")!, macID: "mac-1", deviceID: "phone-1",
                         signing: P256.Signing.PrivateKey(), bearer: { "token" }, transport: relay)
    }

    /// A pairing made before its Mac had an iroh endpoint learns it from the relay.
    @Test func aPairingWithoutTheMacsEndpointLearnsItFromTheOwnersMacs() async throws {
        let relay = AccountRelay(macs: #"{"macs":[{"mac_id":"mac-2","iroh_endpoint":"\#(known)"},{"mac_id":"mac-1","iroh_endpoint":"\#(bound)"}]}"#)
        let address = try #require(await account(relay).macAddress(known: nil))
        #expect(address.endpointID == bound)
        #expect(address.relay == URL(string: "https://iroh.example.com"))
        #expect(await relay.paths.contains("/v1/macs"))
    }

    @Test func aKnownEndpointIsDialedWithoutAskingTheRelayForIt() async throws {
        let relay = AccountRelay(macs: #"{"macs":[{"mac_id":"mac-1","iroh_endpoint":"\#(bound)"}]}"#)
        let address = try #require(await account(relay).macAddress(known: known))
        #expect(address.endpointID == known)
        #expect(await !relay.paths.contains("/v1/macs"))
    }

    @Test func aMacThatBoundNoEndpointHasNoAddress() async throws {
        let relay = AccountRelay(macs: #"{"macs":[{"mac_id":"mac-1"}]}"#)
        #expect(try await account(relay).macAddress(known: nil) == nil)
    }
}
