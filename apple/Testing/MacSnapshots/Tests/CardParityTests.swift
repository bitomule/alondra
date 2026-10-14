import AlondraContract
import AlondraFeatures
import AlondraMacWorkspace
import XCTest

/// Design §7.4: the same card, served to the iPhone and to the Mac, reads the same on both:
/// labels, order, options, confirmation words and colours come from the server and no client changes them.
final class CardParityTests: XCTestCase {
    private func document(_ name: String) throws -> ViewDocument {
        let environment = ProcessInfo.processInfo.environment
        let root = URL(fileURLWithPath: try XCTUnwrap(environment["TEST_SRCDIR"]))
            .appendingPathComponent(environment["TEST_WORKSPACE"] ?? "_main")
        return try ContractCodec.decode(
            ViewDocument.self, from: Data(contentsOf: root.appendingPathComponent("fixtures/views/cards/\(name).json")))
    }

    private func card(in document: ViewDocument) throws -> Card {
        try XCTUnwrap(document.sections.flatMap(\.items).compactMap { item -> Card? in
            if case let .card(card) = item { card } else { nil }
        }.first)
    }

    /// The buttons the phone's card shows, then what its "More…" holds.
    private func phone(_ document: ViewDocument) -> [AlondraContract.Action] {
        var state = TriageCardFeature.State(id: "parity")
        state.screen.document = document
        return state.buttons + state.menu
    }

    private func mac(_ document: ViewDocument) throws -> [AlondraContract.Action] {
        let buttons = MacCardButtons(try card(in: document))
        return buttons.visible + buttons.more
    }

    private func shown(_ action: AlondraContract.Action) -> [String] {
        [action.label, action.paintedRole.rawValue, action.confirm?.title ?? "", action.confirm?.message ?? "",
         action.confirm?.button ?? "", action.description ?? ""] + action.options.flatMap { [$0.label, $0.description ?? ""] }
    }

    func testTheSameCardOffersTheSameActionsInTheSameOrderOnPhoneAndMac() throws {
        for (phoneName, macName) in [("card-pr", "card-pr-mac"), ("card-question", "card-question-mac"),
                                     ("card-information", "card-information-mac")] {
            let onPhone = phone(try document(phoneName)).map(shown)
            let onMac = try mac(try document(macName)).map(shown)
            XCTAssertFalse(onPhone.isEmpty, phoneName)
            XCTAssertEqual(onPhone, onMac, phoneName)
        }
    }

    func testClosingWithoutMergingIsTheOnlyRedActionAndTheOnlyOneThatAsksFirst() throws {
        let card = try mac(try document("card-pr-mac"))
        let red = card.filter { $0.paintedRole == .risk }
        XCTAssertEqual(red.map(\.label), ["Close without merging"])
        XCTAssertEqual(card.filter { $0.confirm != nil }.map(\.label), ["Close without merging"])
        XCTAssertEqual(card.first { $0.label == "Merge" }?.paintedRole, .advance)
    }

    func testAMenuOffersTwoOrMoreDescribedMethods() throws {
        let merge = try XCTUnwrap(try mac(try document("card-pr-mac")).first { $0.presentation == .menu })
        XCTAssertGreaterThanOrEqual(merge.options.count, 2)
        XCTAssertTrue(merge.options.allSatisfy { $0.description?.isEmpty == false })
    }
}
