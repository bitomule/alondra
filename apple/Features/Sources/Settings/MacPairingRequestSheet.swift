import SwiftUI

/// A device on this network asks to use this Mac. Allow it only when its screen shows the
/// same code; the first answer here, in the notification or in the TUI decides.
public struct MacPairingRequestSheet: View {
    let request: PairingRequest
    let error: String?
    let decide: (Bool) -> Void

    public init(request: PairingRequest, error: String?, decide: @escaping (Bool) -> Void) {
        self.request = request
        self.error = error
        self.decide = decide
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Label("A device wants to use Alondra", systemImage: "lock.shield").font(.title2.bold())
            Text("Allow “\(request.name)” only if its screen shows this same code.")
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            Text(verbatim: request.code)
                .font(.system(size: 40, weight: .bold, design: .monospaced))
                .tracking(4)
                .frame(maxWidth: .infinity)
                .padding(.vertical, 12)
                .background(.quaternary, in: .rect(cornerRadius: 12))
                .accessibilityIdentifier("pairingRequest.code")
            if let error { Text(error).foregroundStyle(.red) }
            HStack {
                Button("Deny", role: .destructive) { decide(false) }
                    .accessibilityIdentifier("pairingRequest.deny")
                Spacer()
                Button("Allow") { decide(true) }
                    .keyboardShortcut(.defaultAction)
                    .buttonStyle(.borderedProminent)
                    .accessibilityIdentifier("pairingRequest.allow")
            }
            .controlSize(.large)
        }
        .padding(28)
        .frame(idealWidth: 440, maxWidth: 440)
    }
}
