import UserNotifications

public enum ForegroundPresentation {
    /// The app already shows pushed changes live, so a remote push arriving while it is open stays silent.
    /// Local notifications (such as a failed send from a notification action) are the only feedback and still show.
    public static func options(isRemotePush: Bool) -> UNNotificationPresentationOptions {
        isRemotePush ? [] : [.banner, .list, .badge]
    }
}
