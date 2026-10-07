import AlondraContract
import AlondraDesign
import SwiftUI

/// The one screen both apps show while the fleet is out of reach. It says what is wrong in
/// plain words and keeps retrying by itself; signing out lives only in Settings.
public struct ConnectionLostView: View {
    let problem: ConnectionProblem?
    let retry: () -> Void
    let signIn: (() -> Void)?
    let settings: (() -> Void)?
    let signOut: (() -> Void)?
    let pairAgain: (() -> Void)?

    /// `problem == nil` means the first attempt is still in flight. `pairAgain` starts pairing with
    /// the Mac this screen is about; without it a revoked device is sent to Settings.
    public init(problem: ConnectionProblem?, retry: @escaping () -> Void,
                signIn: (() -> Void)? = nil, settings: (() -> Void)? = nil, signOut: (() -> Void)? = nil,
                pairAgain: (() -> Void)? = nil) {
        self.problem = problem
        self.retry = retry
        self.signIn = signIn
        self.settings = settings
        self.signOut = signOut
        self.pairAgain = pairAgain
    }

    public var body: some View {
        Group {
            if let problem {
                notice(problem)
            } else {
                VStack(spacing: 16) {
                    ProgressView().controlSize(.large).tint(Palette.dawn)
                    Text("Connecting to the fleet…").font(.subheadline).foregroundStyle(.secondary)
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .animation(.smooth, value: problem)
        .accessibilityIdentifier("connection.lost")
    }

    private func notice(_ problem: ConnectionProblem) -> some View {
        VStack(spacing: 18) {
            Spacer()
            Image(systemName: problem.systemImage)
                .font(.system(size: 54, weight: .light))
                .foregroundStyle(problem.needsAction ? Palette.rose : Palette.slate)
                .symbolEffect(.pulse, options: .repeating, isActive: !problem.needsAction)
                .accessibilityHidden(true)
            VStack(spacing: 8) {
                Text(problem.title)
                    .font(.display(.title2, weight: .bold))
                    .foregroundStyle(Palette.ink)
                    .multilineTextAlignment(.center)
                Text(problem.detail)
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
            }
            actions(for: problem)
                .padding(.top, 6)
            Spacer()
            Spacer()
        }
        .padding(.horizontal, 32)
        .frame(maxWidth: .infinity)
    }

    private func pairAgainButton(_ action: @escaping () -> Void) -> some View {
        Button("Pair Again", systemImage: "link.badge.plus", action: action)
            .buttonStyle(.glassProminent)
            .controlSize(.large)
            .tint(Palette.dawn)
    }

    @ViewBuilder
    private func actions(for problem: ConnectionProblem) -> some View {
        VStack(spacing: 12) {
            if problem == .sessionExpired, let signIn {
                Button("Sign In Again", systemImage: "person.crop.circle", action: signIn)
                    .buttonStyle(.glassProminent)
                    .controlSize(.large)
                    .tint(Palette.dawn)
            } else if problem == .signOutIncomplete, let signOut {
                Button("Finish Signing Out", systemImage: "rectangle.portrait.and.arrow.right", action: signOut)
                    .buttonStyle(.glassProminent)
                    .controlSize(.large)
                    .tint(Palette.dawn)
            } else if problem.isDeviceRevoked, let pairAgain {
                pairAgainButton(pairAgain)
            } else if problem.isDeviceRevoked, let settings {
                Button("Open Settings", systemImage: "gearshape", action: settings)
                    .buttonStyle(.glassProminent)
                    .controlSize(.large)
                    .tint(Palette.dawn)
            } else {
                Button("Try Again", systemImage: "arrow.clockwise", action: retry)
                    .buttonStyle(.glassProminent)
                    .controlSize(.large)
                    .tint(Palette.dawn)
            }
            if !problem.needsAction {
                Label { Text("Trying again automatically…") } icon: { ProgressView().controlSize(.small) }
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
            if problem == .macChangedIdentity, let pairAgain {
                Button("Pair Again", systemImage: "link.badge.plus", action: pairAgain)
                    .buttonStyle(.bordered)
                    .controlSize(.regular)
            }
            if problem == .accessNotConfirmed, let pairAgain {
                VStack(spacing: 8) {
                    Text(ConnectionProblem.removedDeviceHint)
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                    Button("Pair Again", systemImage: "link.badge.plus", action: pairAgain)
                        .buttonStyle(.bordered)
                        .controlSize(.regular)
                }
                .padding(.top, 6)
            }
            if let settings, !problem.isDeviceRevoked, problem != .signOutIncomplete {
                Button("Settings", systemImage: "gearshape", action: settings)
                    .buttonStyle(.borderless)
                    .font(.footnote)
            }
        }
    }
}
