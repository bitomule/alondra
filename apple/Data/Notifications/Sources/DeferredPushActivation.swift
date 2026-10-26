import Foundation

/// What a session commit needs from the platform's push center.
@MainActor
public protocol PushAuthorizing: AnyObject {
    var isAuthorized: Bool { get }
    func refreshAuthorization() async
    /// Shows the system alert and registers if the person allows it.
    func activate(whileCurrent isCurrent: @escaping @MainActor () -> Bool) async
    func registerWithAPNs()
}

/// Permission dialogs can outlive a session. Keep one physical activation until it exits,
/// then service only the latest committed session without holding up its publication.
@MainActor
public final class DeferredPushActivation {
    public typealias Work = @MainActor (@escaping @MainActor () -> Bool) async -> Void
    private var generation: UInt64 = 0
    private var wanted: (generation: UInt64, work: Work)?
    private var task: Task<Void, Never>?
    private var running: UInt64?

    public init() {}

    public func prepare(_ work: @escaping Work) async -> @MainActor () -> Void {
        { [weak self] in self?.start(work) }
    }

    /// A committed session only renews the registration of alerts the person already allowed.
    /// The system alert comes from the screen that first says what alerts are for, or from
    /// Settings, never from connecting.
    public func prepareRenewal(_ push: some PushAuthorizing,
                               committed: @escaping @MainActor () -> Bool) async -> @MainActor () -> Void {
        await prepare { [push] isCurrent in
            await push.refreshAuthorization()
            guard isCurrent(), committed(), push.isAuthorized else { return }
            push.registerWithAPNs()
        }
    }

    public func start(_ work: @escaping Work) {
        generation &+= 1
        wanted = (generation, work)
        task?.cancel()
        admit()
    }

    public func cancel() {
        generation &+= 1
        wanted = nil
        task?.cancel()
    }

    private func admit() {
        guard task == nil, let next = wanted else { return }
        wanted = nil
        running = next.generation
        task = Task { [weak self] in
            guard let self else { return }
            if !Task.isCancelled, generation == next.generation {
                await next.work { [weak self] in
                    !Task.isCancelled && self?.generation == next.generation
                }
            }
            guard running == next.generation else { return }
            task = nil
            running = nil
            admit()
        }
    }

    func drain() async {
        while let task { await task.value }
    }
}
