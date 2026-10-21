import AlondraAccount
import Foundation
import Testing

@Suite struct LoopbackReturnListenerTests {
    private func listener() async throws -> LoopbackReturnListener {
        for _ in 0..<20 {
            let port = UInt16.random(in: 49152...65000)
            if let opened = try? await LoopbackReturnListener.open(port: port, page: { "<p>back</p>" }) { return opened }
        }
        throw LoopbackReturnListener.Failure.unavailable
    }

    private func get(_ url: String) async throws -> (Int, String) {
        let session = URLSession(configuration: .ephemeral)
        defer { session.invalidateAndCancel() }
        let (data, response) = try await session.data(from: URL(string: url)!)
        return ((response as? HTTPURLResponse)?.statusCode ?? 0, String(decoding: data, as: UTF8.self))
    }

    @Test func theBrowsersReturnToThisDevicesLocalhostIsHandedBackOnce() async throws {
        let listener = try await listener()
        defer { listener.close() }
        let port = listener.port
        #expect(try await get("http://127.0.0.1:\(port)/favicon.ico").0 == 404)
        #expect(try await get("http://127.0.0.1:\(port)/callback?state=s1").0 == 404, "a return carries its code or error")
        let answer = try await get("http://localhost:\(port)/callback?code=one-time&state=s1")
        #expect(answer == (200, "<p>back</p>"))
        let returned = await listener.next()
        #expect(returned?.absoluteString == "http://localhost:\(port)/callback?code=one-time&state=s1")
    }

    @Test func aPortSomethingElseHoldsIsReportedAsTaken() async throws {
        let first = try await listener()
        defer { first.close() }
        await #expect(throws: LoopbackReturnListener.Failure.portTaken) {
            try await LoopbackReturnListener.open(port: first.port, page: { "" })
        }
    }

    @Test func closingEndsTheWaitForAReturn() async throws {
        let listener = try await listener()
        listener.close()
        #expect(await listener.next() == nil)
    }

    @Test func onlyAGetToCallbackWithStateAndCodeIsAReturn() {
        let head = { (line: String) in LoopbackReturnListener.address(requestHead: Data((line + "\r\nHost: x").utf8), port: 5000) }
        #expect(head("GET /callback?code=c&state=s HTTP/1.1")?.absoluteString == "http://localhost:5000/callback?code=c&state=s")
        #expect(head("GET /callback?error=access_denied&state=s HTTP/1.1") != nil)
        #expect(head("POST /callback?code=c&state=s HTTP/1.1") == nil)
        #expect(head("GET /other?code=c&state=s HTTP/1.1") == nil)
        #expect(head("GET /callback?code=c HTTP/1.1") == nil)
        #expect(head("GET http://evil.example.com/callback?code=c&state=s HTTP/1.1") == nil)
    }
}
