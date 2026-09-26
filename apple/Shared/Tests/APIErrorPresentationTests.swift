import AlondraAPIClient
import AlondraShared
import Testing

@Suite struct APIErrorPresentationTests {
    @Test func emptyForbiddenAndConflictKeepTheirActualStatus() {
        #expect(AlondraError.from(.forbidden(kind: nil, detail: nil, reasons: [])) == .unexpected(403))
        #expect(AlondraError.from(.conflict(kind: nil, detail: nil, reasons: [])) == .unexpected(409))
    }

}
