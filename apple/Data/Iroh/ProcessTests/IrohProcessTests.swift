import AlondraIroh
import AlondraTestSupport
@testable import AlondraTransport
import CryptoKit
import Foundation
import Testing

/// The Swift client against the real daemon: the hello, the stream framing and the sealed answers
/// are the ones `docs/iroh-protocol.md` describes, not a copy of them in a fake.
@Suite(.serialized, .temporaryFiles)
struct IrohProcessTests {
    private static let macSigning = try! P256.Signing.PrivateKey(rawRepresentation: Data(repeating: 1, count: 32))
    private static let macAgreement = try! P256.KeyAgreement.PrivateKey(rawRepresentation: Data(repeating: 2, count: 32))

    @Test func aPairedDeviceGetsSealedAnswersAndARepeatedActionRunsOnce() async throws {
        let server = try IrohFixtureServer()
        defer { server.stop() }
        let ready = try await server.next()
        let (session, carrier) = try Self.session(deviceID: "phone-1", seed: 33, ready: ready)
        await carrier.start()

        let clock = ContinuousClock()
        let started = clock.now
        let read = try APIRequest(endpoint: Endpoint<Data>.data(.get, path: ["capabilities"]), operationID: "read")
        guard case let .completed(status, _) = try await session.perform(read) else { Issue.record("no answer"); return }
        #expect(status == 200)
        #expect(await carrier.route == .direct)
        var rounds: [Duration] = []
        for _ in 0..<10 {
            let round = clock.now
            _ = try await session.perform(read)
            rounds.append(round.duration(to: clock.now))
        }
        print("iroh loopback: first answer \(started.duration(to: clock.now)), median round trip \(rounds.sorted()[5])")

        let body = try #require(ready.action)
        let action = try APIRequest(endpoint: Endpoint<Data>.data(.post, path: ["actions"], body: Data(body.utf8)),
                                    operationID: "op-limit")
        let first = try await session.perform(action)
        let again = try await session.perform(action)
        guard case .completed(200, _) = first else { Issue.record("action not answered: \(first)"); return }
        #expect(first == again, "the same sealed bytes get the Mac's stored answer")
        try server.send("limit")
        #expect(try await server.next().limit?.contains("max_working_nodes = 4") == true)
        await carrier.shutdown()
    }

    @Test func aDeviceThisMacNeverPairedIsRefusedAtTheHello() async throws {
        let server = try IrohFixtureServer()
        defer { server.stop() }
        let ready = try await server.next()
        let (session, carrier) = try Self.session(deviceID: "phone-9", seed: 70, ready: ready)
        await carrier.start()
        let read = try APIRequest(endpoint: Endpoint<Data>.data(.get, path: ["capabilities"]), operationID: "read")
        await #expect(throws: IrohCarrierError.unavailable) { try await session.perform(read) }
        #expect(await !carrier.isConnected)
        await carrier.shutdown()
    }

    private static func session(deviceID: String, seed: UInt8, ready: IrohFixtureServer.Event) throws -> (DirectSession, IrohCarrier) {
        let signing = try P256.Signing.PrivateKey(rawRepresentation: Data(repeating: seed, count: 32))
        let agreement = try P256.KeyAgreement.PrivateKey(rawRepresentation: Data(repeating: seed + 1, count: 32))
        let endpoint = try #require(ready.endpoint)
        let socket = try #require(ready.address)
        let address = try #require(IrohMacAddress(endpointID: endpoint, relay: nil, directAddresses: [socket]))
        let carrier = IrohCarrier(dialer: IrohEndpointDialer(identity: IrohIdentity(pairingSigning: signing, macID: "mac-1")),
            macID: "mac-1", deviceID: deviceID, signing: signing, address: { address }, connectWait: .seconds(10))
        let session = DirectSession(macID: "mac-1", deviceID: deviceID, signing: signing, agreement: agreement,
            macSigning: macSigning.publicKey, macAgreement: macAgreement.publicKey, iroh: carrier)
        return (session, carrier)
    }
}
