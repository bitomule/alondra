@testable import AlondraTransport
import AlondraTestSupport
import CryptoKit
import Foundation
import Testing

private enum AdmissionFault: Sendable, Equatable { case full, serverBusy, otherRefusal, lostReply, expiredWait, none }
private actor AdmissionPeer: HTTPTransport {
    let signing = try! P256.Signing.PrivateKey(rawRepresentation: Data(repeating: 1, count: 32))
    let agreement = try! P256.KeyAgreement.PrivateKey(rawRepresentation: Data(repeating: 2, count: 32))
    let macSigning = try! P256.Signing.PrivateKey(rawRepresentation: Data(repeating: 3, count: 32))
    let macAgreement = try! P256.KeyAgreement.PrivateKey(rawRepresentation: Data(repeating: 4, count: 32))
    let clock = MutationTestClock()
    let fault: AdmissionFault
    let ackCapability: Bool
    var posts: [Data] = []
    var effects = 0
    var waits = 0
    var acknowledgments: [String] = []
    var replies: [String: Envelope] = [:]
    var rejectCount = 2
    init(_ fault: AdmissionFault = .full, ackCapability: Bool = false) { self.fault = fault; self.ackCapability = ackCapability }
    func session(journal: EnvelopeJournal? = nil) throws -> RelaySession {
        try RelaySession(scope: Self.scope, bearer: { "token" }, transport: self, journal: journal,
            signing: signing, agreement: agreement, macSigning: macSigning.publicKey, macAgreement: macAgreement.publicKey,
            now: { 1_000 }, sleep: { _ in await self.pause() }, waitForReadDeadline: clock.clock.waitUntil, requestClock: clock.clock)
    }
    static let scope = RelayOutboxScope(relay: URL(string: "https://relay.fixture.invalid")!, macID: "mac", deviceID: "phone", credentialRevision: "fixture")
    func pause() { waits += 1; clock.advance(by: fault == .expiredWait ? .seconds(121) : .seconds(1)) }
    func send(_ request: URLRequest) async throws -> HTTPReply {
        let path = try #require(request.url).path
        func json(_ value: [String: Any], status: Int = 200) throws -> HTTPReply {
            HTTPReply(status: status, body: try JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]))
        }
        if path == "/v1/bootstrap" { return try json(["protocol":1,"epoch":Base64URL.encode(Data(repeating:7,count:16)),"result_ack":ackCapability,"request_wait":true]) }
        if path.hasSuffix("/ack") {
            acknowledgments.append(String(path.split(separator:"/").dropLast().last!))
            return HTTPReply(status:204,body:Data())
        }
        if request.httpMethod == "POST" {
            let bytes = try #require(request.httpBody); posts.append(bytes)
            if fault == .otherRefusal { return try json(["error":"rate_limited"],status:429) }
            if fault == .full || fault == .serverBusy || fault == .expiredWait, rejectCount > 0 {
                rejectCount -= 1; return try json(["error":fault == .serverBusy ? "server_busy" : "queue_full"],status:fault == .serverBusy ? 503 : 429)
            }
            let envelope = try JSONDecoder().decode(Envelope.self,from:bytes)
            let opened = try envelope.open(senderKey:signing.publicKey,recipientKey:macAgreement,sender:"phone",recipient:"mac",route:"mac",nowMilliseconds:1_000)
            let payload = try #require(try JSONSerialization.jsonObject(with:opened) as? [String:Any])
            if payload["method"] as? String == "POST" { effects += 1 }
            let reply = try Envelope.seal(route:"mac",sender:"mac",recipient:"phone",requestID:envelope.requestID,kind:"response",expiresAtMilliseconds:601_000,
                plaintext:try JSONSerialization.data(withJSONObject:["status":200,"body":["identity":envelope.requestID]],options:[.sortedKeys]),senderKey:macSigning,recipientKey:agreement.publicKey)
            replies[envelope.requestID] = reply
            if fault == .lostReply { throw URLError(.networkConnectionLost) }
            return try json(["request_id":envelope.requestID,"status":"completed","revision":2,"response":try JSONSerialization.jsonObject(with:JSONEncoder().encode(reply))])
        }
        let id = try #require(request.url).lastPathComponent
        guard let response = replies[id] else { return try json(["error":"not_found"],status:404) }
        return try json(["request_id":id,"status":"completed","revision":2,"response":try JSONSerialization.jsonObject(with:JSONEncoder().encode(response))])
    }
}

@Suite(.temporaryFiles)
private struct RelayAdmissionTests {
    @Test(arguments: [false,true], [AdmissionFault.full,.serverBusy]) func explicitFullWaitsForSameEnvelopeAndCompletesWithoutAnotherEffect(mutation: Bool, refusal:AdmissionFault) async throws {
        let peer=AdmissionPeer(refusal), session=try await peer.session()
        let result = mutation ? try await session.execute(requestID:"op-action",method:"POST",path:"/actions",body:Data("{}".utf8),statusPolls:1)
            : try await session.execute(requestID:"unused-read",method:"GET",path:"/views/card:B?protocol=1&client=ios&lang=en")
        guard case let .completed(status,body)=result else { Issue.record("explicit refusal must wait and complete"); return }
        #expect(status==200)
        let posts=await peer.posts
        #expect(posts.count==3)
        #expect(Set(posts).count==1)
        let envelope=try JSONDecoder().decode(Envelope.self,from:#require(posts.first))
        let object=try #require(try JSONSerialization.jsonObject(with:body) as? [String:String])
        #expect(object["identity"]==envelope.requestID)
        #expect(await peer.effects==(mutation ? 1 : 0))
        #expect(await peer.waits==2)
        await session.deactivate()
    }

    @Test(arguments: [AdmissionFault.otherRefusal,.lostReply,.expiredWait])
    func otherRefusalsAndAmbiguityNeverAuthorizeFreshPost(_ fault: AdmissionFault) async throws {
        let peer=AdmissionPeer(fault), session=try await peer.session()
        let result=try await session.execute(requestID:"op-preserved",method:"POST",path:"/actions",body:Data("{}".utf8),statusPolls:1)
        #expect(await peer.posts.count==1)
        if fault == .lostReply {
            guard case .completed=result else { Issue.record("GET must recover the one accepted effect");return }
            #expect(await peer.effects==1)
        } else {
            guard case .unknown=result else { Issue.record("unaccepted/expired work must remain unknown");return }
            #expect(await peer.effects==0)
        }
        #expect(try await session.sentEnvelope(for:"op-preserved") != nil)
        await session.deactivate()
    }

    @Test(arguments:[false,true]) func actionConsumptionRequiresDurableReceipt(durable:Bool) async throws {
        let peer=AdmissionPeer(.none,ackCapability:true)
        let directory=try TestFiles.directory()
        let journal=durable ? try EnvelopeJournal(scope:AdmissionPeer.scope,directory:directory) : nil
        let session=try await peer.session(journal:journal)
        let result=try await session.execute(requestID:"op-durable",method:"POST",path:"/actions",body:Data("{}".utf8),statusPolls:1)
        guard case .completed=result else { Issue.record("expected completed action");return }
        await session.drainAcknowledgments()
        #expect(await peer.acknowledgments==(durable ? ["op-durable"] : []))
        if let journal {
            let restored=try EnvelopeJournal(scope:AdmissionPeer.scope,directory:directory)
            let entry=try #require(try await restored.entry(requestID:"op-durable"))
            #expect(entry.lastResponse != nil)
            #expect(try await journal.entry(requestID:"op-durable")?.lastResponse==entry.lastResponse)
        }
        await session.deactivate()
    }

    @Test(arguments:[false,true]) func verifiedReadsConsumeOnlyWhenRelayAdvertisesSupport(supported:Bool) async throws {
        let peer=AdmissionPeer(.none,ackCapability:supported), session=try await peer.session()
        let result=try await session.execute(requestID:"unused-read",method:"GET",path:"/views/card:B?protocol=1&client=ios&lang=en")
        guard case .completed=result else { Issue.record("expected verified read");return }
        await session.drainAcknowledgments()
        let id=try JSONDecoder().decode(Envelope.self,from:#require(await peer.posts.first)).requestID
        #expect(await peer.acknowledgments==(supported ? [id] : []))
        #expect(await peer.effects==0)
        await session.deactivate()
    }
}

private actor AckCancellationPeer: HTTPTransport {
    enum Held: Sendable { case bearer, reply }
    let base = AdmissionPeer(.none,ackCapability:true)
    let held: Held
    let arrivals: AsyncStream<Void>
    let entered: AsyncStream<Void>.Continuation
    var continuation: CheckedContinuation<Void,Never>?
    var tokens=0
    var sends=0
    var refusals=0
    init(_ held: Held) {
        self.held=held
        (arrivals,entered)=AsyncStream.makeStream(bufferingPolicy:.bufferingNewest(1))
    }
    func token() async -> String {
        tokens += 1
        if tokens>1, held == .bearer { await withCheckedContinuation { continuation=$0; entered.yield(()) } }
        return "token"
    }
    func release() { continuation?.resume();continuation=nil }
    func refused() { refusals += 1 }
    func session() async throws -> RelaySession {
        try await RelaySession(scope:AdmissionPeer.scope,bearer:{await self.token()},bearerRefused:{await self.refused()},transport:self,
            signing:base.signing,agreement:base.agreement,macSigning:base.macSigning.publicKey,macAgreement:base.macAgreement.publicKey,now:{1_000})
    }
    func rawTransport() throws -> RelayTransport {
        let key = base.signing
        return try RelayTransport(base:AdmissionPeer.scope.relay,deviceID:"phone",
            signer:{try key.signature(for:$0).rawRepresentation},bearer:{await self.token()},bearerRefused:{await self.refused()},
            transport:self,now:{1_000})
    }
    func send(_ request:URLRequest) async throws -> HTTPReply {
        if request.url?.path.hasSuffix("/ack")==true {
            sends += 1
            if held == .reply { await withCheckedContinuation { continuation=$0; entered.yield(()) } }
            return HTTPReply(status:401,body:Data())
        }
        return try await base.send(request)
    }
}

@Suite(.timeLimit(.minutes(1)))
private struct RelayAcknowledgmentCancellationTests {
    @Test(arguments:[AckCancellationPeer.Held.bearer,.reply])
    func deactivationPreventsLateAcknowledgmentAuthenticationEffects(_ held:AckCancellationPeer.Held) async throws {
        let peer=AckCancellationPeer(held), session=try await peer.session()
        var arrivals=peer.arrivals.makeAsyncIterator()
        let result=try await session.execute(requestID:"unused-read",method:"GET",path:"/views/card:B?protocol=1&client=ios&lang=en")
        guard case .completed=result else { Issue.record("expected verified read");return }
        _=await arrivals.next()
        await session.deactivate()
        await peer.release()
        await session.drainAcknowledgments()
        #expect(await peer.sends==(held == .bearer ? 0 : 1))
        #expect(await peer.refusals==0)
    }
    @Test func aLateCancelledAckCannotRejectTheCurrentBearerWithoutMutationScope() async throws {
        let peer=AckCancellationPeer(.reply), transport=try await peer.rawTransport()
        _=try await transport.bootstrap()
        var arrivals=peer.arrivals.makeAsyncIterator()
        let request=Task { try await transport.acknowledgeResult(requestID:"read-held",revision:1) }
        _=await arrivals.next()
        request.cancel()
        await peer.release()
        _=await request.result
        #expect(await peer.sends==1)
        #expect(await peer.refusals==0)
    }

}
