import Foundation

public enum SchedulePresentation {
    public static var bundle: Bundle { #bundle }
    public static func description(_ task: ScheduledTask) -> String {
        let time = String(format: "%02d:%02d", task.hour, task.minute)
        let at = String(localized: "at", table: "AlondraShared", bundle: #bundle)
        let project = task.project ?? String(localized: "Projects folder", table: "AlondraShared", bundle: #bundle)
        let description: String
        switch task.frequency {
        case "weekly" where task.weekday != nil:
            let weekday = min(6, max(0, task.weekday!))
            let names = Calendar.current.weekdaySymbols
            description = "\(String(localized: "Every", table: "AlondraShared", bundle: #bundle)) \(names[weekday]) \(at) \(time)"
        case "every_hours":
            description = task.interval == 1 ? String(localized: "Every hour", table: "AlondraShared", bundle: #bundle) :
                String.localizedStringWithFormat(String(localized: "Every %lld hours", table: "AlondraShared", bundle: #bundle), Int64(task.interval ?? 1))
        case "every_days":
            description = task.interval == 1 ? String(localized: "Every day", table: "AlondraShared", bundle: #bundle) :
                String.localizedStringWithFormat(String(localized: "Every %lld days", table: "AlondraShared", bundle: #bundle), Int64(task.interval ?? 1))
        case "every_months":
            description = task.interval == 1 ? String(localized: "Every month", table: "AlondraShared", bundle: #bundle) :
                String.localizedStringWithFormat(String(localized: "Every %lld months", table: "AlondraShared", bundle: #bundle), Int64(task.interval ?? 1))
        case "once":
            description = task.startAt.map { Date(timeIntervalSince1970: Double($0) / 1000).formatted(date: .abbreviated, time: .shortened) } ?? ""
        default:
            description = "\(String(localized: "Every day", table: "AlondraShared", bundle: #bundle)) \(at) \(time)"
        }
        return "\(description) · \(project)"
    }

public static func error(_ message: String) -> String {
    let key: String
    switch message {
    case "title must be 1–120 characters": key = "Enter a name of up to 120 characters."
    case "mission must be 1–20,000 characters": key = "Enter instructions of up to 20,000 characters."
    case "disallowed tools must be at most 100 nonempty tool patterns without spaces or flags": key = "Enter up to 100 tool patterns without spaces."
    case "time must be a valid local hour and minute": key = "Choose a valid time."
    case "weekly schedule needs weekday 0–6": key = "Choose a day of the week."
    case "custom interval must be 1–365": key = "Choose an interval from 1 to 365."
    case "start date must be before 2100": key = "Choose a date before 2100."
    case "one-time date must be in the future": key = "Choose a future date and time."
    case "project cannot be empty", "project must be one of the available project folders": key = "Choose an available project."
    case "projects directory does not exist": key = "The Projects folder is unavailable."
    case "schedule changed elsewhere; refresh before editing": key = "This schedule changed. Refresh and try again."
    case "schedule not found or a run is being sent", "already_running": key = "This schedule is running. Try again shortly."
    case "not_due": key = "This schedule is not due yet."
    case "the orchestrator is not connected to Alondra": key = "The orchestrator is offline"
    case "The scheduled task is waiting for the orchestrator": key = "Waiting for orchestrator"
    default: return message
    }
    return String(localized: String.LocalizationValue(key), table: "AlondraShared", bundle: #bundle)
}

}
