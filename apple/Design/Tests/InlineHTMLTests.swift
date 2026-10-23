import AlondraDesign
import Foundation
import Testing

@Suite struct InlineHTMLTests {
    struct Case: CustomTestStringConvertible, Sendable {
        let html: String
        let markdown: String

        var testDescription: String { html }
    }

    static let cases: [Case] = [
        Case(html: #"<img src="https://a.dev/l.png" alt="Logo" width=120>"#, markdown: "![Logo](https://a.dev/l.png)"),
        Case(html: #"<img width=120 alt='Logo' src='https://a.dev/l.png'>"#, markdown: "![Logo](https://a.dev/l.png)"),
        Case(html: "<IMG ALT=Logo SRC=https://a.dev/l.png />", markdown: "![Logo](https://a.dev/l.png)"),
        Case(html: "<img\n  alt=\"Logo\"\n  src = \"https://a.dev/l.png\"/>", markdown: "![Logo](https://a.dev/l.png)"),
        Case(html: #"<p align="center"><img src="https://a.dev/l.png" alt="Logo"></p>"#, markdown: "![Logo](https://a.dev/l.png)"),
        Case(html: #"See <a href="https://a.dev/docs">the docs</a> now"#, markdown: "See [the docs](https://a.dev/docs) now"),
        Case(html: #"<a href=https://a.dev><img alt=Badge src="https://a.dev/b.svg"/></a>"#, markdown: "[![Badge](https://a.dev/b.svg)](https://a.dev)"),
        Case(html: "<picture>\n  <source srcset=\"https://a.dev/dark.png\">\n  <img src=\"https://a.dev/light.png\" alt=\"Diagram\">\n</picture>", markdown: "![Diagram](https://a.dev/light.png)"),
        Case(html: #"<a href="https://a.dev/?a=1&amp;b=2">x &amp; y</a>"#, markdown: "[x &amp; y](https://a.dev/?a=1&amp;b=2)"),
        Case(html: "<p>\nOne<br>\nTwo <b>bold</b>\n</p>", markdown: "One\\\nTwo bold"),
        Case(html: "Press <kbd>v</kbd> to open.", markdown: "Press <kbd>v</kbd> to open."),
        Case(html: "<a>no href</a> stays", markdown: "no href stays"),
        Case(html: #"<img src="https://a.dev/a b(1).png" alt="[x]">"#, markdown: #"![\[x\]](https://a.dev/a%20b\(1\).png)"#),
    ]

    @Test(arguments: cases)
    func htmlBecomesTheMarkdownItMeans(_ testCase: Case) {
        #expect(InlineHTML.markdown(from: testCase.html) == testCase.markdown)
    }

    static let untouched: [String] = [
        "plain text with 3 < 4 and a > b",
        "see <https://a.dev> and <me@a.dev>",
        "code `<img src=x>` stays",
        "escaped \\<img src=x> stays",
        "```\n<img src=\"https://a.dev/c.png\">\n```",
        "para\n\n    <img src=\"https://a.dev/c.png\">",
        "<!-- note -->",
    ]

    @Test(arguments: untouched)
    func textThatIsNotHtmlStaysAsItIs(_ source: String) {
        let expected = source == "<!-- note -->" ? "" : source
        #expect(InlineHTML.markdown(from: source) == expected)
    }

    @Test func anImageAloneInItsParagraphIsAnImageBlock() throws {
        let blocks = MarkdownParser.blocks(from: "<p align=center>\n  <img src=\"https://a.dev/l.png\" alt=\"Logo\">\n</p>\n\n![](docs/shot.png)")
        #expect(blocks.map(\.kind) == [
            .image(url: try #require(URL(string: "https://a.dev/l.png")), alt: "Logo"),
            .image(url: try #require(URL(string: "docs/shot.png")), alt: ""),
        ])
        #expect(blocks.map(\.plainText) == ["Logo", ""])
    }

    @Test func anImageInsideASentenceIsALinkBehindACameraMark() throws {
        let block = try #require(MarkdownParser.blocks(from: "Look ![the screen](https://a.dev/s.png) and ![local](docs/l.png).").first)
        #expect(block.kind == .paragraph)
        #expect(block.plainText == "Look the screen and local.")
        #expect(String(block.content.characters) == "Look 📷 the screen and 📷 local.")
        let links = block.content.runs[\.link].compactMap { link, range in link.map { ($0, String(block.content[range].characters)) } }
        #expect(links.map(\.0) == [try #require(URL(string: "https://a.dev/s.png"))])
        #expect(links.map(\.1) == ["📷 the screen"])
        #expect(block.content.runs[\.imageURL].allSatisfy { url, _ in url == nil }, "Text draws a run with an image URL as a missing image")
    }

    @Test func aBlockKnowsWhenItOpensLinks() throws {
        let blocks = MarkdownParser.blocks(from: "<a href=\"https://a.dev\">Docs</a>\n\nplain\n\n![logo](https://a.dev/l.png)")
        #expect(blocks.map(\.opensLinks) == [true, false, false])
    }
}
