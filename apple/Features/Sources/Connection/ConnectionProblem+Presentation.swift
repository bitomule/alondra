import AlondraContract
import Foundation
#if canImport(UIKit)
import UIKit
#endif

public extension ConnectionProblem {
    var title: LocalizedStringResource {
        switch self {
        case .noInternet: "No internet connection"
        case .serverUnreachable: "Can’t reach the Alondra server"
        case .macNotAnswering: "Your Mac isn’t answering"
        case .alondraNotRunning: "Alondra isn’t running on this Mac"
        case .sessionExpired: "Your session has expired"
        case .accessNotConfirmed: "Alondra can’t confirm your access right now"
        case .deviceRevoked(let macName): Self.revokedTitle(macName)
        case .macNotOnNetwork, .macChangedIdentity: "Your Mac isn’t answering"
        case .signOutIncomplete: "Signing out didn’t finish"
        case .unexpected: "Can’t connect right now"
        }
    }

    private static func revokedTitle(_ macName: String?) -> LocalizedStringResource {
        guard let macName else { return "This \(deviceName) is no longer paired with your Mac" }
        return "This \(deviceName) is no longer paired with “\(macName)”"
    }

    /// When pairing again is the way out of `accessNotConfirmed`: the relay cannot say, unsealed,
    /// that the Mac removed this device.
    static var removedDeviceHint: LocalizedStringResource {
        #if canImport(UIKit)
        "If you removed this \(deviceName) on your Mac, pair it again."
        #else
        "If you removed this Mac on the other Mac, pair it again."
        #endif
    }

    /// What this device is called on its own screen: iPhone, iPad or Mac.
    private static var deviceName: String {
        #if canImport(UIKit)
        UIDevice.current.localizedModel
        #else
        "Mac"
        #endif
    }

    var detail: LocalizedStringResource {
        switch self {
        case .noInternet: "This device is offline. Alondra reconnects as soon as the network is back."
        case .serverUnreachable: "You’re online, but the server isn’t answering. Alondra keeps trying."
        case .macNotAnswering: "It may be off, or Alondra may not be running on it. Alondra reconnects as soon as it answers."
        case .alondraNotRunning: "Install it with Homebrew (`brew install bitomule/tap/alondra`), then run `alondra` once. This window connects as soon as it answers."
        case .sessionExpired: "Sign in again to reconnect. Your pairing and saved work are kept."
        case .accessNotConfirmed: "Your session and pairing are kept. Alondra keeps trying."
        case .deviceRevoked: "Pair it again to keep using it."
        case .macChangedIdentity: "There’s a Mac with this name but a different identity. Pair it again only if you reinstalled that Mac."
        case .macNotOnNetwork: "It may be in sleep mode, or on another network. Alondra reconnects as soon as it answers; to reach it away from home, turn on Tailscale on both."
        case .signOutIncomplete: "Alondra couldn’t finish signing this device out. Finish signing out, then sign in again to reconnect."
        case .unexpected: "Something unexpected went wrong. Alondra keeps trying."
        }
    }

    var systemImage: String {
        switch self {
        case .noInternet: "wifi.slash"
        case .serverUnreachable: "icloud.slash"
        case .macNotAnswering: "desktopcomputer.trianglebadge.exclamationmark"
        case .alondraNotRunning: "bolt.horizontal.circle"
        case .sessionExpired: "person.crop.circle.badge.exclamationmark"
        case .accessNotConfirmed: "person.badge.clock"
        case .deviceRevoked: "lock.slash"
        case .macChangedIdentity: "questionmark.circle"
        case .macNotOnNetwork: "moon.zzz"
        case .signOutIncomplete: "rectangle.portrait.and.arrow.right"
        case .unexpected: "exclamationmark.triangle"
        }
    }
}
