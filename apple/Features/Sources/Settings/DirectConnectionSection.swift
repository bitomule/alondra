import SwiftUI

/// «Direct connection»: whether devices away from home reach this Mac without the relay, the
/// facts behind it, and the router steps that would open a way in.
public struct DirectConnectionSection: View {
    let direct: DirectConnection

    public init(direct: DirectConnection) { self.direct = direct }

    public var body: some View {
        Section {
            VStack(alignment: .leading, spacing: 4) {
                Label { Text(direct.status) } icon: { icon }
                if let detail = direct.detail {
                    Text(detail).font(.footnote).foregroundStyle(.secondary)
                }
            }
            .accessibilityElement(children: .combine)
            .accessibilityIdentifier("macSettings.direct.status")
            if let mapping = direct.portMappingText {
                LabeledContent("Port mapping") { Text(mapping) }
            }
            if direct.verdict != .checking {
                LabeledContent("IPv6") {
                    if let ipv6 = direct.publicIpv6 { Text(verbatim: ipv6) } else { Text("None") }
                }
                LabeledContent("NAT") { Text(direct.natText) }
            }
            if let port = direct.port {
                LabeledContent("UDP port") { Text(verbatim: String(port)) }
            }
            if let sessions = direct.sessionsText {
                LabeledContent("Recent connections") { Text(sessions) }
            }
            ForEach(direct.reasons, id: \.self) { reason in
                Text(DirectConnection.explanation(reason)).font(.footnote).foregroundStyle(.secondary)
            }
        } header: {
            Text("Direct connection")
        }
        if !direct.guide.isEmpty {
            Section {
                ForEach(direct.guide.enumerated(), id: \.offset) { index, step in
                    Label { Text(step) } icon: { Text(verbatim: "\(index + 1).").monospacedDigit() }
                }
            } header: {
                Text("How to make it direct")
            }
            .accessibilityIdentifier("macSettings.direct.guide")
        }
    }

    @ViewBuilder private var icon: some View {
        switch direct.verdict {
        case .reachable: Image(systemName: "bolt.horizontal.circle.fill").foregroundStyle(.green)
        case .relayOnly: Image(systemName: "arrow.triangle.branch").foregroundStyle(.orange)
        case .checking: ProgressView().controlSize(.small)
        case .unknown: Image(systemName: "questionmark.circle").foregroundStyle(.secondary)
        }
    }
}
