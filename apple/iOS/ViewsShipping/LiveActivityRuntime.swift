import AlondraLiveActivity
import Foundation

/// Owns the app's Live Activity controller for the whole process, and wakes it when a Mac's
/// grace after reaching zero runs out.
@MainActor final class LiveActivityRuntime {
    private var wakeTask: Task<Void, Never>?
    private var startedTask: Task<Void, Never>?
    private(set) lazy var controller = LiveActivityController(host: ActivityKitHost()) { [weak self] date in
        self?.wake(at: date)
    }

    /// Listens first thing at launch, so an activity iOS started while the app was closed is seen.
    func start() {
        startedTask = Task { [weak self] in
            for await _ in ActivityKitHost.started() { self?.controller.reconcile() }
        }
    }

    private func wake(at date: Date) {
        wakeTask?.cancel()
        wakeTask = Task { [weak self] in
            try? await Task.sleep(for: .seconds(max(0, date.timeIntervalSinceNow)))
            guard !Task.isCancelled else { return }
            self?.controller.reconcile()
        }
    }
}
