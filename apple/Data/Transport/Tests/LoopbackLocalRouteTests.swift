@testable import AlondraTransport
import Foundation
import Testing

@Suite struct LoopbackLocalRouteTests {
    /// The device contract never carries this Mac's own routes, so they cannot go through it.
    @Test func theDeviceContractRefusesTheLocalRoutes() {
        for path in [["notices"], ["devices"], ["devices", "pending"]] {
            #expect(throws: OperationError.invalidEndpoint) {
                try APIRequest(endpoint: Endpoint<Data>.data(.get, path: path), operationID: "read")
            }
        }
    }

    @Test func localRoutesNameExactlyTheDaemonsLoopbackPaths() {
        #expect(LoopbackLocalRoute.notices(lang: "es").target == "/notices?lang=es")
        #expect(LoopbackLocalRoute.pendingDevices.target == "/devices/pending")
        #expect(LoopbackLocalRoute.approvePending("p-1").target == "/devices/pending/p-1/approve")
        #expect(LoopbackLocalRoute.approvePending("p-1").body == Data("{}".utf8))
        #expect(LoopbackLocalRoute.denyPending("p-1").method == "POST")
        #expect(LoopbackLocalRoute.revokeDevice("phone-1").method == "DELETE")
    }

    @Test func anIDCannotReachAnotherRoute() {
        #expect(LoopbackLocalRoute.revokeDevice("../tasks").target == nil)
        #expect(LoopbackLocalRoute.approvePending("a/b").target == nil)
        #expect(LoopbackLocalRoute.notices(lang: "fr").target == nil)
    }
}
