import AlondraContract
import AlondraTransport
import Testing
import AlondraAccount
import AlondraFeatures
import Synchronization

@Suite(.serialized)
struct ConnectionClassifierTests {
    @Test func theAccountStateThePersonSeesIsLogged() {
        let lines = Mutex<[String]>([])
        AccountDiagnostics.record(to: { line, _ in lines.withLock { $0.append(line) } })
        defer { AccountDiagnostics.record(to: AccountDiagnostics.systemLog) }
        _ = ConnectionClassifier.problem(for: RelayWireError.server(403), local: false, networkOffline: false)
        _ = ConnectionClassifier.problem(for: AuthError.noSession, local: false, networkOffline: false)
        _ = ConnectionClassifier.problem(for: AuthError.oauth(status: 401, code: "secret-ish"), local: false, networkOffline: false)
        let seen = lines.withLock { $0 }
        #expect(seen.contains("person sees accessNotConfirmed after RelayWireError.server(403)"))
        #expect(seen.contains("person sees sessionExpired after AuthError.noSession"))
        #expect(seen.contains("person sees accessNotConfirmed after AuthError.oauth(status: 401)"))
        #expect(!seen.contains { $0.contains("secret-ish") })
    }

    @Test func aSealedRevocationNamesTheMacEvenWithoutInternet() {
        for offline in [false, true] {
            #expect(ConnectionClassifier.problem(for: DirectSessionError.deviceRevoked, local: false,
                                                 networkOffline: offline, macName: "Studio") == .deviceRevoked(macName: "Studio"))
        }
        #expect(ConnectionProblem.deviceRevoked(macName: "Studio").needsAction)
    }

    @Test func aMacWhoseIdentityChangedIsAHintThatKeepsRetrying() {
        let problem = ConnectionClassifier.problem(for: DirectRouteError.identityChanged, local: false, networkOffline: false)
        #expect(problem == .macChangedIdentity)
        #expect(!problem.needsAction)
    }

    @Test func aMacThatSimplyDoesNotAnswerStaysNotAnswering() {
        #expect(ConnectionClassifier.problem(for: DirectRouteError.unreachable, local: false, networkOffline: false) == .macNotOnNetwork)
    }
}
