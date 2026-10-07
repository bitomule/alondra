import AlondraContract
import AlondraTransport
import Testing
import AlondraAccount

struct ConnectionClassifierTests {
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
