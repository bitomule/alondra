import SwiftUI

/// What a card keeps when the stack gives it less room. Each step gives up the next thing that also
/// lives in the detail; the last steps cap the text size instead.
struct CardFit: Equatable, Identifiable {
    let id: Int
    var why = true
    var meta = true
    var chips = true
    var row = true
    var state = true
    var reply = true
    var largestType = DynamicTypeSize.accessibility5

    static let ladder: [CardFit] = [
        CardFit(id: 0),
        CardFit(id: 1, meta: false),
        CardFit(id: 2, why: false, meta: false),
        CardFit(id: 3, why: false, meta: false, chips: false),
        CardFit(id: 4, why: false, meta: false, chips: false, row: false),
        CardFit(id: 5, why: false, meta: false, chips: false, row: false, state: false),
        compact(id: 6, largestType: .accessibility5),
        compact(id: 7, largestType: .accessibility2),
        compact(id: 8, largestType: .xxxLarge),
    ]

    private static func compact(id: Int, largestType: DynamicTypeSize) -> CardFit {
        CardFit(id: id, why: false, meta: false, chips: false, row: false, state: false, reply: false, largestType: largestType)
    }
}
