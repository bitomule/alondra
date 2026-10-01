import AlondraDesign
import Foundation
import Testing

@Suite struct CodeColourTests {
    struct FenceCase: CustomTestStringConvertible, Sendable {
        let fence: String?
        let language: String?

        var testDescription: String { fence ?? "nil" }
    }

    static let fenceCases: [FenceCase] = [
        FenceCase(fence: nil, language: nil),
        FenceCase(fence: "", language: nil),
        FenceCase(fence: "swift", language: "swift"),
        FenceCase(fence: "Swift", language: "swift"),
        FenceCase(fence: "swift title=\"a.swift\"", language: "swift"),
        FenceCase(fence: "{.rust}", language: "rust"),
        FenceCase(fence: ".sh", language: "sh"),
        FenceCase(fence: "bzl", language: "python"),
        FenceCase(fence: "jsonl", language: "json"),
        FenceCase(fence: "text", language: nil),
        FenceCase(fence: "plaintext", language: nil),
        FenceCase(fence: "{}", language: nil),
    ]

    @Test(arguments: fenceCases)
    func aFenceNamesItsLanguage(_ testCase: FenceCase) {
        #expect(CodeColourKey.language(fence: testCase.fence) == testCase.language)
    }

    struct KeyCase: CustomTestStringConvertible, Sendable {
        let name: String
        let other: CodeColourKey?
        let same: Bool

        var testDescription: String { name }
    }

    static let base = CodeColourKey(code: "let a = 1", fence: "swift", dark: true)

    static let keyCases: [KeyCase] = [
        KeyCase(name: "the same block", other: CodeColourKey(code: "let a = 1", fence: "swift", dark: true), same: true),
        KeyCase(name: "the same language spelt differently", other: CodeColourKey(code: "let a = 1", fence: "Swift title=x", dark: true), same: true),
        KeyCase(name: "other text", other: CodeColourKey(code: "let a = 2", fence: "swift", dark: true), same: false),
        KeyCase(name: "another language", other: CodeColourKey(code: "let a = 1", fence: "rust", dark: true), same: false),
        KeyCase(name: "the light scheme", other: CodeColourKey(code: "let a = 1", fence: "swift", dark: false), same: false),
    ]

    @Test(arguments: keyCases)
    func theKeyIsTextLanguageAndScheme(_ testCase: KeyCase) throws {
        let base = try #require(Self.base)
        let other = try #require(testCase.other)
        #expect((base == other) == testCase.same)
    }

    @Test func nothingToColourHasNoKey() {
        #expect(CodeColourKey(code: "let a = 1", fence: nil, dark: true) == nil)
        #expect(CodeColourKey(code: "let a = 1", fence: "text", dark: true) == nil)
        #expect(CodeColourKey(code: " \n ", fence: "swift", dark: true) == nil)
    }

    @Test func theCacheDropsTheLeastRecentlyUsed() throws {
        let cache = CodeColourCache(capacity: 2)
        let first = try #require(CodeColourKey(code: "a", fence: "swift", dark: true))
        let second = try #require(CodeColourKey(code: "b", fence: "swift", dark: true))
        let third = try #require(CodeColourKey(code: "c", fence: "swift", dark: true))
        cache.store(AttributedString("a"), for: first)
        cache.store(AttributedString("b"), for: second)
        #expect(cache[first] == AttributedString("a"))
        cache.store(AttributedString("c"), for: third)
        #expect(cache.count == 2)
        #expect(cache[first] != nil)
        #expect(cache[second] == nil)
        #expect(cache[third] != nil)
    }
}
