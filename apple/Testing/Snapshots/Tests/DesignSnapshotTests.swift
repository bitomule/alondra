import AlondraDesign
import SwiftUI
import XCTest

/// The shared pieces every screen is built from, in the three variants each baseline is recorded in.
@MainActor
final class DesignSnapshotTests: XCTestCase {
    func test_palette() {
        assertVariants { PaletteSwatches().padding() }
    }

    func test_glyphSizes() {
        assertVariants {
            HStack(spacing: 16) {
                Glyph(symbol: "hand.point.right", tint: Palette.ember)
                Glyph(symbol: "questionmark.bubble", tint: Palette.dawn, size: .small)
                Glyph(symbol: "checkmark", tint: Palette.sage, size: .tiny)
                Glyph(symbol: "xmark", tint: Palette.rose)
            }
            .padding()
        }
    }

    func test_buttonStyles() {
        assertVariants {
            VStack(spacing: 12) {
                Button("Approve") {}
                    .buttonStyle(FilledStyle(tint: Palette.sage))
                    .padding(.horizontal, 20)
                    .padding(.vertical, 12)
                Button("Leave a comment") {}
                    .buttonStyle(PressStyle())
                    .padding(.horizontal, 20)
                    .padding(.vertical, 12)
            }
            .padding()
        }
    }

    func test_dawnBackground() {
        assertVariants {
            DawnBackground().frame(height: 220)
        }
    }

    func test_markdownBlocks() {
        assertVariants {
            MarkdownSample().padding()
        }
    }

    func test_markdownReadme() {
        assertVariants {
            ReadmeSample().padding()
        }
    }

    func test_markdownStoredImage() async throws {
        let png = StoredImageSample.png
        let files = StoredFiles { _ in png }
        _ = try await files.fetch(try XCTUnwrap(StoredImage.attachment(StoredImageSample.url)))
        assertVariants {
            StoredImageSample().padding().environment(\.storedFiles, files)
        }
    }

    func test_diffFile() {
        assertVariants {
            DiffSample().padding()
        }
    }
}

private struct PaletteSwatches: View {
    private let swatches: [(String, Color)] = [
        ("dawn", Palette.dawn), ("ember", Palette.ember), ("sky", Palette.sky), ("sage", Palette.sage),
        ("rose", Palette.rose), ("slate", Palette.slate), ("night", Palette.night), ("dusk", Palette.dusk),
        ("card", Palette.card), ("cardRaised", Palette.cardRaised), ("ink", Palette.ink),
        ("onAccent", Palette.onAccent), ("sageDeep", Palette.sageDeep), ("skyDeep", Palette.skyDeep),
    ]

    var body: some View {
        LazyVGrid(columns: [GridItem(.adaptive(minimum: 110), spacing: 12)], spacing: 12) {
            ForEach(swatches, id: \.0) { name, color in
                VStack(spacing: 4) {
                    RoundedRectangle(cornerRadius: 10)
                        .fill(color)
                        .frame(height: 44)
                        .overlay(RoundedRectangle(cornerRadius: 10).stroke(Palette.hairline))
                    Text(name).font(.caption2)
                }
            }
        }
    }
}

private struct MarkdownSample: View {
    private let segments = MarkdownParser.segments(from: """
    ## Beta candidate ready

    - Restored the native review layouts
    - Kept swipe navigation between cards

    Verified the **ARM** simulator build with `make simulator`.

    | Step | Result |
    | --- | --- |
    | Build | Passed |
    | Tests | Passed |
    """)

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            ForEach(segments) { MarkdownSegmentView(segment: $0) }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

private struct ReadmeSample: View {
    private let segments = MarkdownParser.segments(from: """
    <h1 align=center>Alondra</h1>

    <p align="center">
      <a href="https://alondra.dev/docs">Docs</a> ·
      <a href='https://github.com/example/alondra'>Code</a><br>
      Made for <b>people</b> &amp; agents
    </p>

    Read [the install guide](https://alondra.dev/install) first.

    <img src="docs/shot.png" alt="Local shot">
    """)

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            ForEach(segments) { MarkdownSegmentView(segment: $0) }
        }
        .fixedSize(horizontal: false, vertical: true)
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

private struct DiffSample: View {
    private let files = UnifiedDiff.parse("""
    diff --git a/Welcome.swift b/Welcome.swift
    --- a/Welcome.swift
    +++ b/Welcome.swift
    @@ -1,3 +1,3 @@
     struct Welcome {
    -    let greeting = "Welcome to the dashboard"
    +    let greeting = "What needs your attention?"
     }
    """)

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ForEach(files) { file in
                DiffFileHeader(file: file, comments: 2)
                ForEach(file.lines) { DiffCodeLine(line: $0, coloured: nil) }
            }
        }
    }
}

private struct StoredImageSample: View {
    static let url = URL(string: "/files?path=files/images/\(String(repeating: "c", count: 64)).png")!
    static let png: Data = UIGraphicsImageRenderer(size: CGSize(width: 320, height: 160)).pngData { context in
        UIColor.systemTeal.setFill()
        context.fill(CGRect(x: 0, y: 0, width: 320, height: 160))
        UIColor.white.setFill()
        context.fill(CGRect(x: 24, y: 24, width: 120, height: 112))
        context.fill(CGRect(x: 168, y: 24, width: 128, height: 20))
        context.fill(CGRect(x: 168, y: 60, width: 96, height: 20))
    }
    private let segments = MarkdownParser.segments(from: """
    ## Screens

    The main screen, from the worktree:

    ![Main screen](\(url.absoluteString))

    ![Missing](docs/missing.png)
    """)

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            ForEach(segments) { MarkdownSegmentView(segment: $0) }
        }
        .fixedSize(horizontal: false, vertical: true)
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}
