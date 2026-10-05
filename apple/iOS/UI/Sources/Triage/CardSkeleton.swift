import AlondraDesign
import SwiftUI

/// What a card shows while its view loads: the real card's anatomy as redacted placeholders, so
/// the stack keeps its shape and the content settles into place instead of replacing a spinner.
struct CardSkeleton: View {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var sweeping = false

    var body: some View {
        Color.clear
            .overlay(alignment: .topLeading) {
                VStack(alignment: .leading, spacing: 14) {
                    header
                    summary
                    contentRow
                    Spacer(minLength: 0)
                    buttons
                    reply
                }
                .padding(20)
            }
        .redacted(reason: .placeholder)
        .background { Palette.card }
        .overlay { sheen }
        .clipShape(.rect(cornerRadius: 34, style: .continuous))
        .overlay { RoundedRectangle(cornerRadius: 34, style: .continuous).strokeBorder(Palette.hairline) }
        .shadow(color: .black.opacity(0.22), radius: 24, y: 14)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Loading…")
        .onAppear {
            guard !reduceMotion else { return }
            withAnimation(.easeInOut(duration: 1.4).repeatForever(autoreverses: false)) { sweeping = true }
        }
    }

    private var header: some View {
        HStack(spacing: 8) {
            Capsule().fill(Palette.hairline).frame(width: 96, height: 28)
            Spacer(minLength: 4)
            Text(verbatim: "2 min ago").font(.footnote)
        }
    }

    private var summary: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(verbatim: "Waiting for your answer before it can go on")
                .font(.display(.title2, weight: .bold)).lineLimit(2, reservesSpace: true)
            Text(verbatim: "It stopped here and needs one decision from you")
                .font(.subheadline).lineLimit(1)
        }
    }

    private var contentRow: some View {
        HStack(spacing: 12) {
            Circle().fill(Palette.hairline).frame(width: 32, height: 32)
            VStack(alignment: .leading, spacing: 2) {
                Text(verbatim: "Command").font(.headline)
                Text(verbatim: "A short summary of what it holds").font(.subheadline).lineLimit(1)
            }
            Spacer(minLength: 0)
        }
        .padding(14)
        .background(Palette.cardRaised, in: .rect(cornerRadius: 18, style: .continuous))
    }

    private var buttons: some View {
        VStack(spacing: 8) {
            Capsule().fill(Palette.hairline).frame(height: 50)
            Capsule().fill(Palette.hairline).frame(height: 50)
        }
    }

    private var reply: some View {
        RoundedRectangle(cornerRadius: 22, style: .continuous)
            .fill(Palette.cardRaised).frame(height: 52)
            .overlay(RoundedRectangle(cornerRadius: 22, style: .continuous).strokeBorder(Palette.hairline))
    }

    private var sheen: some View {
        GeometryReader { proxy in
            LinearGradient(
                colors: [.clear, Palette.ink.opacity(0.08), .clear], startPoint: .leading, endPoint: .trailing
            )
            .frame(width: proxy.size.width * 0.6)
            .offset(x: sweeping ? proxy.size.width : -proxy.size.width * 0.6)
        }
        .allowsHitTesting(false)
    }
}
