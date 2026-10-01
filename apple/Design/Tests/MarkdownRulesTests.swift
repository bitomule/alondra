import AlondraDesign
import Testing

@Suite struct MarkdownRulesTests {
    struct TaskCase: CustomTestStringConvertible, Sendable {
        let source: String
        let marker: MarkdownBlock.ListMarker
        let text: String

        var testDescription: String { source }
    }

    static let taskCases: [TaskCase] = [
        TaskCase(source: "- [ ] open", marker: .task(done: false), text: "open"),
        TaskCase(source: "- [x] done", marker: .task(done: true), text: "done"),
        TaskCase(source: "- [X] done", marker: .task(done: true), text: "done"),
        TaskCase(source: "* [ ] star", marker: .task(done: false), text: "star"),
        TaskCase(source: "1. [x] numbered", marker: .task(done: true), text: "numbered"),
        TaskCase(source: "- [ ] **bold** first", marker: .task(done: false), text: "bold first"),
        TaskCase(source: "- [ ]no space", marker: .bullet, text: "[ ]no space"),
        TaskCase(source: "- [] empty", marker: .bullet, text: "[] empty"),
        TaskCase(source: "- [y] other", marker: .bullet, text: "[y] other"),
        TaskCase(source: "- text [ ] later", marker: .bullet, text: "text [ ] later"),
        TaskCase(source: "3. [ ]", marker: .number("3"), text: "[ ]"),
    ]

    @Test(arguments: taskCases)
    func aListItemStartingWithABoxIsATask(_ testCase: TaskCase) throws {
        let block = try #require(MarkdownParser.blocks(from: testCase.source).first)
        #expect(block.kind == .listItem(marker: testCase.marker))
        #expect(block.plainText == testCase.text)
    }

    @Test func aBoxOutsideAListStaysText() throws {
        let block = try #require(MarkdownParser.blocks(from: "[ ] not a list").first)
        #expect(block.kind == .paragraph)
        #expect(block.plainText == "[ ] not a list")
    }

    struct KeyCase: CustomTestStringConvertible, Sendable {
        let name: String
        let before: String
        let after: String
        let block: String

        var testDescription: String { name }
    }

    static let keyCases: [KeyCase] = [
        KeyCase(name: "a block added above", before: "Intro\n\nTarget", after: "New\n\nIntro\n\nTarget", block: "Target"),
        KeyCase(name: "a block removed above", before: "Intro\n\nOther\n\nTarget", after: "Intro\n\nTarget", block: "Target"),
        KeyCase(name: "a table row added above", before: "| a | b |\n|---|---|\n| 1 | 2 |\n\nTarget", after: "| a | b |\n|---|---|\n| 0 | 0 |\n| 1 | 2 |\n\nTarget", block: "1 | 2"),
        KeyCase(name: "a heading moved into a list", before: "# Target\n\nText", after: "- Item\n\n# Target\n\nText", block: "Target"),
    ]

    @Test(arguments: keyCases)
    func aDraftKeyFollowsItsBlock(_ testCase: KeyCase) throws {
        let before = try #require(MarkdownParser.blocks(from: testCase.before).first { $0.plainText == testCase.block })
        let after = try #require(MarkdownParser.blocks(from: testCase.after).first { $0.plainText == testCase.block })
        #expect(before.id != after.id)
        #expect(before.draftKey == after.draftKey)
    }

    @Test func repeatedBlocksGetTheirOccurrence() {
        let keys = MarkdownParser.blocks(from: "Same\n\nSame\n\n- Same").map(\.draftKey)
        #expect(Set(keys).count == 3)
        #expect(keys[0].hasSuffix(":0"))
        #expect(keys[1].hasSuffix(":1"))
        #expect(keys[2].hasPrefix("item:"))
        #expect(keys[2].hasSuffix(":0"))
    }

    @Test func theKeyDependsOnKindAndText() {
        let first = MarkdownParser.blocks(from: "Alpha").map(\.draftKey)
        let again = MarkdownParser.blocks(from: "Alpha").map(\.draftKey)
        let other = MarkdownParser.blocks(from: "Beta").map(\.draftKey)
        let quoted = MarkdownParser.blocks(from: "> Alpha").map(\.draftKey)
        #expect(first == again)
        #expect(first != other)
        #expect(first != quoted)
    }

    @Test func consecutiveRowsFormOneTableUntilTheNextHeader() {
        let source = "| a |\n|---|\n| 1 |\n| 2 |\n\n| b |\n|---|\n| 3 |\n\nAfter"
        let segments = MarkdownParser.segments(from: source)
        let shapes = segments.map { segment -> String in
            switch segment {
            case .block(let block): block.plainText
            case .table(let rows): "table \(rows.count)"
            }
        }
        #expect(shapes == ["table 3", "table 2", "After"])
    }
}

@Suite struct MarkdownBlockCacheTests {
    @Test func aCachedParseEqualsAFreshOne() {
        let source = "# Title\n\n| a | b |\n|:-|-:|\n| 1 | 2 |\n\n- [x] done"
        let cache = MarkdownBlockCache(capacity: 4)
        let first = cache.blocks(for: source)
        let again = cache.blocks(for: source)
        #expect(first == again)
        #expect(first == MarkdownParser.blocks(from: source))
        #expect(cache.count == 1)
    }

    @Test func theCacheKeepsTheMostRecentlyUsedWithinItsCapacity() {
        let cache = MarkdownBlockCache(capacity: 3)
        for index in 0..<10 {
            _ = cache.blocks(for: "Text \(index)")
            #expect(cache.count == min(index + 1, 3))
        }
        #expect(cache.contains("Text 9") && !cache.contains("Text 6"))

        let small = MarkdownBlockCache(capacity: 2)
        _ = small.blocks(for: "a")
        _ = small.blocks(for: "b")
        _ = small.blocks(for: "a")
        _ = small.blocks(for: "c")
        #expect(small.contains("a") && small.contains("c") && !small.contains("b"))
    }
}
