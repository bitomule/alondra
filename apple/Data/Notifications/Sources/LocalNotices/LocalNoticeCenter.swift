import Foundation

/// The system notification center as the coordinator sees it: one notification per card,
/// identified by the card id, so a later notice for the same card replaces the earlier one.
public protocol LocalNoticeCenter: Sendable {
    func show(_ notice: LocalNotices.Notice) async
    func delivered() async -> [String]
    func remove(_ cards: [String]) async
    func setBadge(_ count: Int) async
}
