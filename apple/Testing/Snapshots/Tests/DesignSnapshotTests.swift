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
