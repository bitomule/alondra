import AlondraContract

/// What a screen reader says of the parts of a sheet. The daemon words every label in the viewer's
/// language, so these only put the words in the order a person listens to them.
enum SheetSpeech {
    static func scenario(_ scenario: SheetScenario) -> String {
        var words = [scenario.title, scenario.kind]
        if let mark = scenario.mark, mark.change != .unchanged { words.append(mark.label) }
        if let result = scenario.result { words.append(result.state.text) }
        return words.joined(separator: ", ")
    }

    static func spec(_ sheet: SpecSheet) -> String {
        var words = [sheet.title]
        if let mark = sheet.mark, mark.change != .unchanged { words.append(mark.label) }
        return words.joined(separator: ", ")
    }

    static func decision(_ sheet: DecisionSheet) -> String {
        var words = [sheet.title, sheet.status]
        if let mark = sheet.mark, mark.change != .unchanged { words.append(mark.label) }
        return words.joined(separator: ", ")
    }
}
