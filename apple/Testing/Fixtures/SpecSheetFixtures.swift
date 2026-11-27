import AlondraContract
import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers

/// The specs of a plan as the daemon words them for a client, with made-up orders and refunds: the
/// plan, a change to approved specs, a delivery with what the validator found, and the states a
/// screen of specs can be in. Each is built in English and in Spanish, as the daemon would.
enum SpecSheetFixtures {
    struct Words {
        let spanish: Bool
        func pick(_ english: String, _ spanish: String) -> String { self.spanish ? spanish : english }
        var given: String { pick("Given", "Partiendo de") }
        var when: String { pick("When", "Cuando") }
        var then: String { pick("Then", "Entonces") }
        var test: String { pick("test", "test") }
        var judgment: String { pick("judgment on captures", "juicio sobre capturas") }
    }

    static let imageName = String(repeating: "1a", count: 32) + ".png"
    static let imageURL = "/files?path=files/images/\(imageName)"
    static let diagram = "flowchart LR\n  Ask[Customer asks for a refund] --> Check{Anything left?}\n  Check -->|yes| Pay[Refund the amount]\n  Check -->|no| Refuse[Refuse it]"

    static var picture: SheetPicture { SheetPicture(url: imageURL, width: 1170, height: 2532, bytes: 184_320) }

    /// A phone screen drawn in code, so no capture of a real app is needed.
    static var mockupPNG: Data {
        let width = 390, height = 844
        let space = CGColorSpace(name: CGColorSpace.sRGB)!
        let context = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0, space: space,
                                bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
        func fill(_ red: CGFloat, _ green: CGFloat, _ blue: CGFloat, _ rect: CGRect, radius: CGFloat = 0) {
            context.setFillColor(CGColor(red: red, green: green, blue: blue, alpha: 1))
            context.addPath(CGPath(roundedRect: rect, cornerWidth: radius, cornerHeight: radius, transform: nil))
            context.fillPath()
        }
        fill(0.96, 0.94, 0.9, CGRect(x: 0, y: 0, width: width, height: height))
        fill(0.85, 0.4, 0.12, CGRect(x: 24, y: height - 150, width: width - 48, height: 80), radius: 18)
        for row in 0..<4 {
            fill(1, 1, 1, CGRect(x: 24, y: height - 280 - row * 110, width: width - 48, height: 92), radius: 16)
            fill(0.8, 0.78, 0.74, CGRect(x: 44, y: height - 230 - row * 110, width: 160, height: 12), radius: 6)
            fill(0.9, 0.88, 0.85, CGRect(x: 44, y: height - 256 - row * 110, width: 240, height: 10), radius: 5)
        }
        fill(0.25, 0.49, 0.31, CGRect(x: 24, y: 40, width: width - 48, height: 56), radius: 28)
        let data = NSMutableData()
        let destination = CGImageDestinationCreateWithData(data, UTType.png.identifier as CFString, 1, nil)!
        CGImageDestinationAddImage(destination, context.makeImage()!, nil)
        CGImageDestinationFinalize(destination)
        return data as Data
    }

    private static func mark(_ change: SheetChange, _ words: Words) -> SheetMark {
        switch change {
        case .new: SheetMark(change: .new, label: words.pick("new", "nueva"))
        case .changed: SheetMark(change: .changed, label: words.pick("changed", "cambiada"))
        case .removed: SheetMark(change: .removed, label: words.pick("removed", "eliminada"))
        case .unchanged: SheetMark(change: .unchanged, label: words.pick("unchanged", "sin cambios"))
        }
    }

    private static func lanes(_ words: Words, given: [SheetStep], when: [SheetStep], then: [SheetStep]) -> [SheetLane] {
        [SheetLane(kind: .given, label: words.given, steps: given), SheetLane(kind: .when, label: words.when, steps: when),
         SheetLane(kind: .then, label: words.then, steps: then)]
    }

    private static func diagramPart(_ words: Words, change: SheetChange? = .new) -> SheetPart {
        let title = words.pick("Diagram “Refund flow”", "Diagrama «Refund flow»")
        return SheetPart(id: "refund-flow.mmd", kind: .diagram, name: "refund-flow.mmd", title: title,
                         alt: words.pick("Diagram “Refund flow” of the spec “Partial refunds”", "Diagrama «Refund flow» de la spec «Partial refunds»"),
                         mark: change.map { mark($0, words) }, code: diagram,
                         anchor: SheetAnchor(id: "billing.partial#refund-flow.mmd", quote: "Diagram refund-flow.mmd"),
                         edit: .view(key: "task_specs:t-1:part:diagram:s1:d1"))
    }

    private static func mockupPart(_ words: Words, scenario: String? = nil, change: SheetChange? = .new) -> SheetPart {
        let of = scenario ?? "Partial refunds"
        return SheetPart(id: "refund-sheet.png", kind: .mockup, name: "refund-sheet.png", title: words.pick("Mockup “Refund sheet”", "Mockup «Refund sheet»"),
                         alt: words.pick("Mockup “Refund sheet” of the scenario “\(of)”", "Mockup «Refund sheet» del escenario «\(of)»"),
                         mark: change.map { mark($0, words) }, picture: picture,
                         anchor: SheetAnchor(id: "billing.partial#refund-sheet.png", quote: "Mockup refund-sheet.png"),
                         edit: .view(key: "task_specs:t-1:part:mockup:c1:m1"))
    }

    private static func partOfAnOrder(_ words: Words, change: SheetChange, result: SheetResult? = nil, collapsed: Bool = false) -> SheetScenario {
        SheetScenario(
            id: "billing.partial/part", title: "Part of an order", kind: words.test, mark: mark(change, words), collapsed: collapsed,
            lanes: lanes(words, given: [SheetStep(text: "a paid order of 30 EUR")], when: [SheetStep(text: "10 EUR are refunded")],
                         then: [SheetStep(text: "the customer receives 10 EUR"), SheetStep(text: "and 20 EUR are left to refund")]),
            result: result, anchor: SheetAnchor(id: "billing.partial/part", quote: "billing.partial/part · Part of an order"),
            edit: .view(key: "task_specs:t-1:scenario:c1"))
    }

    private static func twoAtOnce(_ words: Words, change: SheetChange, result: SheetResult? = nil) -> SheetScenario {
        SheetScenario(
            id: "billing.partial/together", title: "Two refunds at the same time", kind: words.judgment, mark: mark(change, words),
            lanes: lanes(words, given: [SheetStep(text: "a paid order of 30 EUR"), SheetStep(text: "two agents open its refund sheet")],
                         when: [SheetStep(text: "both ask to refund these amounts", table: [["agent", "amount"], ["ana", "20 EUR"], ["luis", "20 EUR"]])],
                         then: [SheetStep(text: "only the first succeeds"), SheetStep(text: "the second sees a refusal with this text", doc: "Only 10 EUR are left to refund.")]),
            parts: [mockupPart(words, scenario: "Two refunds at the same time")], result: result,
            anchor: SheetAnchor(id: "billing.partial/together", quote: "billing.partial/together · Two refunds at the same time"),
            edit: .view(key: "task_specs:t-1:scenario:c2"),
            add: [SheetReference(title: words.pick("Add a diagram", "Añadir un diagrama"), link: .view(key: "task_specs:t-1:part:diagram:c2:new")),
                  SheetReference(title: words.pick("Add a mockup", "Añadir un mockup"), link: .view(key: "task_specs:t-1:part:mockup:c2:new"))])
    }

    private static func outline(_ words: Words) -> SheetScenario {
        SheetScenario(
            id: "billing.partial/amounts", title: "Amounts that cannot be refunded", kind: words.test, mark: mark(.new, words),
            lanes: lanes(words, given: [SheetStep(text: "a paid order of 30 EUR")], when: [SheetStep(text: "<amount> are refunded")], then: [SheetStep(text: "the refund is refused")]),
            examples: SheetTable(label: words.pick("Examples", "Ejemplos"), rows: [["amount", "reason"], ["0 EUR", "nothing to refund"], ["31 EUR", "more than the order"]]),
            anchor: SheetAnchor(id: "billing.partial/amounts", quote: "billing.partial/amounts"), edit: .view(key: "task_specs:t-1:scenario:c3"))
    }

    private static func quiet(_ words: Words, count: Int) -> SheetUnchanged {
        let scenarios = (1...count).map { index in
            SheetScenario(id: "billing.partial/old-\(index)", title: "Existing refund rule \(index)", kind: words.test, mark: mark(.unchanged, words), collapsed: true,
                          lanes: lanes(words, given: [SheetStep(text: "an order")], when: [SheetStep(text: "it is refunded")], then: [SheetStep(text: "the rule \(index) holds")]))
        }
        return SheetUnchanged(label: words.pick("\(count) unchanged scenarios", "\(count) escenarios sin cambios"), scenarios: scenarios)
    }

    private static func decisions(_ words: Words) -> SheetReferences {
        SheetReferences(label: words.pick("Relies on:", "Se apoya en:"), items: [
            SheetReference(title: "A refund never exceeds what is left", link: .view(key: "task_specs:t-1:decision:d1"))])
    }

    /// The plan: a new spec with a diagram, a mockup and two scenarios, as the user reads it before approving.
    static func plan(spanish: Bool = false) -> SpecSheet {
        let words = Words(spanish: spanish)
        return SpecSheet(
            id: "s1", spec: "billing.partial", title: "Partial refunds", mark: mark(.new, words),
            explanation: "A customer gets back any amount up to what is left of the order.", decisions: decisions(words),
            parts: [diagramPart(words), mockupPart(words)],
            scenarios: [partOfAnOrder(words, change: .new), twoAtOnce(words, change: .new), outline(words)],
            anchor: SheetAnchor(id: "billing.partial", quote: "billing.partial · Partial refunds"), edit: .view(key: "task_specs:t-1:spec:s1"),
            add: [SheetReference(title: words.pick("Add a scenario", "Añadir un escenario"), link: .view(key: "task_specs:t-1:scenario:new-s1"))])
    }

    /// A change to approved specs: one scenario changed, one added and the rest as they were.
    static func amendment(spanish: Bool = false) -> SpecSheet {
        let words = Words(spanish: spanish)
        return SpecSheet(
            id: "s1", spec: "billing.partial", title: "Partial refunds", mark: mark(.changed, words),
            explanation: "A customer gets back any amount up to what is left of the order.",
            parts: [diagramPart(words, change: .changed)],
            background: SheetBackground(label: words.pick("Background", "Contexto"), lanes: [SheetLane(kind: .given, label: words.given, steps: [SheetStep(text: "the store is open")])]),
            scenarios: [partOfAnOrder(words, change: .changed), outline(words)], unchanged: quiet(words, count: 12),
            anchor: SheetAnchor(id: "billing.partial", quote: "billing.partial · Partial refunds"))
    }

    private static func line(_ icon: Icon, _ tone: Tone, _ text: String, detail: String? = nil) -> SheetLine { SheetLine(icon: icon, tone: tone, text: text, detail: detail) }

    /// A delivery: what the validator found for each scenario, with the captures its judgment rests on.
    static func delivery(spanish: Bool = false, evidenceAvailable: Bool = true) -> SpecSheet {
        let words = Words(spanish: spanish)
        let tested = SheetResult(
            state: line(.check, .success, words.pick("Validated", "Validado")),
            checks: [line(.check, .success, words.pick("It passes: valid", "Pasa: válido")), line(.check, .success, words.pick("Its test checks it: valid", "Su test lo comprueba: válido"))],
            tests: [line(.check, .success, "crates/billing/tests/refunds.rs:42", detail: words.pick("suite billing: 18 passed", "suite billing: 18 pasan"))])
        let capture = SheetPart(
            id: "evidence-1", kind: .mockup, name: "refund-sheet.png", title: words.pick("Capture “refund-sheet.png”", "Captura «refund-sheet.png»"),
            alt: words.pick("Capture “refund-sheet.png” the validator judged", "Captura «refund-sheet.png» que juzgó el validador"),
            picture: evidenceAvailable ? picture : nil,
            unavailable: evidenceAvailable ? nil : words.pick("Evidence not available: this capture was not committed", "Evidencia no disponible: esta captura no se subió al repositorio"))
        let judged = SheetResult(
            state: line(.check, .warning, words.pick("Judged: needs another look", "Juzgado: pide otra mirada")),
            verdicts: [SheetVerdict(
                line: line(.warning, .warning, words.pick("Judgment: the sheet shows what is left, but not after a second refund", "Juicio: la hoja enseña lo que queda, pero no tras un segundo reembolso")),
                reasoning: words.pick("The first capture shows 20 EUR left. No capture shows the sheet after the second refund.", "La primera captura enseña 20 EUR restantes. Ninguna captura enseña la hoja tras el segundo reembolso."),
                nearestLabel: words.pick("Closest to an error the test still accepts", "Lo más cercano a un error que el test aún acepta"),
                nearestWrong: words.pick("The sheet keeps showing 20 EUR after another 10 EUR were refunded.", "La hoja sigue enseñando 20 EUR después de reembolsar otros 10 EUR."),
                evidence: [capture])])
        return SpecSheet(
            id: "s1", spec: "billing.partial", title: "Partial refunds", mark: mark(.changed, words),
            explanation: "A customer gets back any amount up to what is left of the order.",
            scenarios: [partOfAnOrder(words, change: .new, result: tested), twoAtOnce(words, change: .new, result: judged)],
            unchanged: quiet(words, count: 12))
    }

    /// A delivery that is being validated right now.
    static func validating(spanish: Bool = false) -> SpecSheet {
        let words = Words(spanish: spanish)
        let running = SheetResult(state: line(.history, .info, words.pick("Validating now", "Validando ahora")))
        return SpecSheet(id: "s1", spec: "billing.partial", title: "Partial refunds", explanation: "A customer gets back any amount up to what is left of the order.",
                         scenarios: [partOfAnOrder(words, change: .unchanged, result: running)])
    }

    static func decision(spanish: Bool = false) -> DecisionSheet {
        let words = Words(spanish: spanish)
        return DecisionSheet(
            id: "d1", decision: "refund-limit", title: "A refund never exceeds what is left", status: words.pick("accepted", "aceptada"), mark: mark(.new, words),
            sections: [
                SheetSection(label: words.pick("Context", "Contexto"), text: "Two agents can open the refund sheet of one order at the same time."),
                SheetSection(label: words.pick("Decision", "Decisión"), text: "The amount left is read when the refund is made, never when the sheet opens."),
                SheetSection(label: words.pick("Consequences", "Consecuencias"), text: "A second refund can be refused after the sheet said it was possible."),
                SheetSection(label: words.pick("How it is checked", "Cómo se comprueba"), text: "A scenario refunds twice at once and expects one refusal.")],
            supersedes: SheetReferences(label: words.pick("Replaces", "Sustituye a"), items: [SheetReference(title: "Refunds are whole", link: nil)]),
            anchor: SheetAnchor(id: "refund-limit", quote: "refund-limit · A refund never exceeds what is left"), edit: .view(key: "task_specs:t-1:decision:d1"))
    }

    /// A spec with forty scenarios: three the change touches and thirty-seven folded into a line.
    static func forty(spanish: Bool = false) -> SpecSheet {
        let words = Words(spanish: spanish)
        return SpecSheet(id: "s1", spec: "billing.partial", title: "Partial refunds", mark: mark(.changed, words),
                         explanation: "A customer gets back any amount up to what is left of the order.",
                         scenarios: [partOfAnOrder(words, change: .changed), twoAtOnce(words, change: .changed), outline(words)], unchanged: quiet(words, count: 37))
    }

    static func notice(_ text: String, tone: Tone) throws -> Notice {
        try ContractCodec.decode(Notice.self, from: Data(#"{"text":"\#(text)","tone":"\#(tone.rawValue)"}"#.utf8))
    }

    static let steps = "Given a paid order of 30 EUR\nWhen these amounts are refunded\n  | agent | amount |\n  | ana   | 20 EUR |\nThen the second refund is refused\n  \"\"\"\n  Only 10 EUR are left to refund.\n  \"\"\"\nAnd the order keeps 10 EUR\n\nExamples:\n  | amount | reason |\n  | 0 EUR  | nothing to refund |"
}
