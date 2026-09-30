import Foundation
import AlondraContracts

/// One of the small bars next to the Mac status: the week's Claude usage, today against its
/// share, memory and CPU. The bar and the value are shown; `help` says what it is in words.
public struct StatusReading: Identifiable, Hashable, Sendable {
    public enum Level: Sendable { case calm, warm, hot }

    public let id: String
    public let value: Double
    public let label: String
    public let level: Level
    public let help: String
}

public enum StatusReadings {
    public static func of(machine: Machine, usage: Usage?) -> [StatusReading] {
        var out: [StatusReading] = []
        if let usage {
            let week = Int(usage.weekPercent.rounded())
            let reset = Date(timeIntervalSince1970: Double(usage.weekResetsAt) / 1000).formatted(.relative(presentation: .named))
            let perDay = Int(usage.perDayLeft.rounded())
            out.append(StatusReading(
                id: "week",
                value: usage.weekPercent / 100,
                label: "\(week)%",
                level: usage.weekPercent >= 85 ? .hot : .calm,
                help: String(localized: "Claude this week: \(week)% of the weekly limit used. It resets \(reset); from here that leaves about \(perDay)% per day.", table: "AlondraShared", bundle: #bundle)
            ))
            if let spent = usage.todaySpent, let share = usage.todayShare {
                var help = String(localized: "Today: \(Int(spent.rounded()))% of the week spent, against the \(Int(share.rounded()))% that today's share allows.", table: "AlondraShared", bundle: #bundle)
                if usage.isOverPace {
                    help += " " + String(localized: "Over pace: the orchestrator asks you before launching new nodes.", table: "AlondraShared", bundle: #bundle)
                }
                out.append(StatusReading(
                    id: "today",
                    value: min(1, spent / max(share, 0.1)),
                    label: "\(Int(spent.rounded()))/\(Int(share.rounded()))%",
                    level: usage.isOverPace ? .hot : spent >= share * 0.75 ? .warm : .calm,
                    help: help
                ))
            }
        }
        if let free = machine.memoryFreePercent {
            let used = 100 - Int(min(free, 100))
            let available = ByteCountFormatter.string(fromByteCount: Int64(machine.memoryAvailable), countStyle: .memory)
            out.append(StatusReading(
                id: "memory",
                value: Double(used) / 100,
                label: "\(used)%",
                level: free < 15 ? .hot : free < 30 ? .warm : .calm,
                help: String(localized: "Memory in use: \(used)% (\(available) available). Below 15% free, Alondra tells the orchestrator not to launch nodes.", table: "AlondraShared", bundle: #bundle)
            ))
        }
        if let cpu = machine.cpuUsage {
            let load = (machine.load.first ?? 0).formatted(.number.precision(.fractionLength(1)))
            var help = String(localized: "CPU in use: \(Int(cpu.rounded()))%. Load average \(load) over \(machine.cpus ?? 0) cores: how many processes want the CPU at once.", table: "AlondraShared", bundle: #bundle)
            if machine.unreliable {
                help += " " + String(localized: "The load is so high that these readings may be wrong.", table: "AlondraShared", bundle: #bundle)
            }
            out.append(StatusReading(
                id: "cpu",
                value: min(cpu, 100) / 100,
                label: "\(Int(cpu.rounded()))%",
                level: machine.unreliable ? .hot : cpu >= 85 ? .warm : .calm,
                help: help
            ))
        }
        return out
    }
}
